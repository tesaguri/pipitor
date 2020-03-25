use std::num::NonZeroU64;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use futures::future::Future;
use futures::ready;
use futures::stream::StreamExt;
use http_body::Body;
use pin_project::{pin_project, project};
use smallvec::SmallVec;
use tokio::time::Interval;

use crate::twitter;
use crate::util::HttpService;

use super::{Core, Sender, TwitterRequestExt as _};

#[pin_project]
pub struct TwitterListTimeline<S, B>
where
    S: HttpService<B>,
{
    #[pin]
    inner: Option<Inner<S, B>>,
}

const RESP_BUF_CAP: usize = 8;

#[pin_project]
struct Inner<S, B>
where
    S: HttpService<B>,
{
    unpin: Unpinned<S, B>,
    #[pin]
    backfill: Option<Backfill<S, B>>,
}

struct Unpinned<S, B>
where
    S: HttpService<B>,
{
    list_id: NonZeroU64,
    since_id: Option<i64>,
    responses:
        SmallVec<[Pin<Box<twitter::ResponseFuture<Vec<twitter::Tweet>, S, B>>>; RESP_BUF_CAP]>,
    interval: Interval,
}

#[pin_project]
struct Backfill<S, B>
where
    S: HttpService<B>,
{
    since_id: i64,
    #[pin]
    response: twitter::ResponseFuture<Vec<twitter::Tweet>, S, B>,
}

impl<S, B> TwitterListTimeline<S, B>
where
    S: HttpService<B> + Clone,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    pub fn new(list_id: NonZeroU64, since_id: Option<i64>, core: &Core<S>) -> Self {
        let backfill = since_id.map(|since_id| {
            let response = twitter::lists::Statuses::new(list_id)
                .since_id(Some(since_id))
                .send(core, None);
            Backfill { since_id, response }
        });

        let mut inner = Inner {
            unpin: Unpinned {
                list_id,
                since_id: None,
                responses: SmallVec::new(),
                // Rate limit of GET lists/statuses (user auth): 900 reqs/15-min window (1 req/sec)
                interval: tokio::time::interval(Duration::from_secs(1)),
            },
            backfill,
        };
        inner.unpin.send_request(core);

        TwitterListTimeline { inner: Some(inner) }
    }

    pub fn empty() -> Self {
        TwitterListTimeline { inner: None }
    }

    pub fn poll(
        self: Pin<&mut Self>,
        core: &Core<S>,
        mut sender: Pin<&mut Sender<S, B>>,
        cx: &mut Context<'_>,
    ) -> Poll<anyhow::Result<()>> {
        trace_fn!(TwitterListTimeline::<S, B>::poll);

        let mut inner = if let Some(inner) = self.project().inner.as_pin_mut() {
            inner
        } else {
            return Poll::Ready(Ok(()));
        };

        if let Poll::Ready(Some(_)) = inner.as_mut().project().unpin.interval.poll_next_unpin(cx) {
            inner.as_mut().project().unpin.send_request(core);
        }

        let tweets = match ready!(inner.as_mut().project().unpin.poll_next(cx)) {
            Ok(resp) => resp.data,
            Err(e) => {
                // Skip the error as the list timeline is only for complementary purpose.
                warn!("error while retrieving Tweets from the list: {:?}", e);
                if let twitter::Error::Twitter(e) = e {
                    if let Some(limit) = e.rate_limit {
                        if limit.remaining == 0 {
                            let reset = SystemTime::UNIX_EPOCH + Duration::from_secs(limit.reset);
                            let (now_s, now_i) = (SystemTime::now(), Instant::now());
                            let eta = reset
                                .duration_since(now_s)
                                .unwrap_or(Duration::from_secs(0));
                            let at = (now_i + eta).into();
                            inner.project().unpin.interval =
                                tokio::time::interval_at(at, Duration::from_secs(1));
                        }
                    }
                }
                return Poll::Pending;
            }
        };

        // Reverse the order so that older Tweets get Retweeted first.
        let mut tweets = tweets.into_iter().rev();

        if let Some(t) = tweets.next() {
            inner.project().unpin.since_id = Some(t.id);

            if log_enabled!(log::Level::Trace) {
                let created_at = super::snowflake_to_system_time(t.id as u64);
                match SystemTime::now().duration_since(created_at) {
                    Ok(latency) => trace!("Twitter list worst latency: {:.2?}", latency),
                    Err(e) => trace!("Twitter list worst latency: -{:.2?}", e.duration()),
                }
            }

            sender.as_mut().send_tweet(t, core)?;
        }

        for t in tweets {
            sender.as_mut().send_tweet(t, core)?;
        }

        Poll::Pending
    }

    #[project]
    pub fn poll_backfill(
        self: Pin<&mut Self>,
        core: Pin<&mut Core<S>>,
        mut sender: Pin<&mut Sender<S, B>>,
        cx: &mut Context<'_>,
    ) -> Poll<anyhow::Result<()>> {
        #[project]
        let Inner {
            unpin,
            backfill: mut backfill_opt,
        } = {
            match self.project().inner.as_pin_mut() {
                Some(inner) => inner.project(),
                _ => {
                    return Poll::Ready(Ok(()));
                }
            }
        };

        #[project]
        let Backfill {
            since_id: backfill_since_id,
            mut response,
        } = if let Some(bf) = backfill_opt.as_mut().as_pin_mut() {
            bf.project()
        } else {
            return Poll::Ready(Ok(()));
        };

        let tweets = match ready!(response.as_mut().poll(cx)) {
            Ok(resp) => resp.data,
            Err(e) => {
                backfill_opt.set(None);
                return Poll::Ready(Err(e.into()));
            }
        };

        if tweets.is_empty() {
            debug!("timeline backfilling completed");
            backfill_opt.set(None);
            return Poll::Ready(Ok(()));
        }

        if unpin.since_id.is_none() {
            unpin.since_id = tweets.first().map(|t| t.id);
        }

        let max_id = tweets.last().map(|t| t.id - 1);
        response.set(
            twitter::lists::Statuses::new(unpin.list_id)
                .since_id(Some(*backfill_since_id))
                .max_id(max_id)
                .send(&core, None),
        );

        for t in tweets {
            if t.retweeted_status.is_none() {
                sender.as_mut().send_tweet(t, &core)?;
            }
        }

        Poll::Pending
    }
}

impl<S, B> Unpinned<S, B>
where
    S: HttpService<B> + Clone,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<
        Result<
            twitter::Response<Vec<twitter::Tweet>>,
            twitter::Error<S::Error, <S::ResponseBody as Body>::Error>,
        >,
    > {
        // Iterate from later responses.
        for (i, resp) in self.responses.iter_mut().enumerate().rev() {
            if let Poll::Ready(result) = resp.as_mut().poll(cx) {
                // Discard earlier pending responses as well since we have newer data now.
                if i > 0 {
                    debug!("dropping {} pending list response(s)", i);
                }
                self.responses.drain(0..=i);

                return Poll::Ready(result);
            }
        }

        Poll::Pending
    }

    fn send_request(&mut self, core: &Core<S>) {
        trace_fn!(Unpinned::<S, B>::send_request);

        let count = if self.since_id.is_some() { 200 } else { 1 };
        let resp = twitter::lists::Statuses::new(self.list_id)
            .count(Some(count))
            .include_rts(Some(false))
            .since_id(self.since_id)
            .send(core, None);

        if self.responses.len() == self.responses.capacity() {
            debug!("respone buffer reached its capacity");
            let mut slot = self.responses.remove(0);
            slot.set(resp);
            self.responses.push(slot);
        } else {
            self.responses.push(Box::pin(resp));
        }
    }
}
