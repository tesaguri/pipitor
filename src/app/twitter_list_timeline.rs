use std::io::Write;

use std::task::Context;
use std::time::{Duration, Instant, SystemTime};

use failure::Fallible;

use futures::compat::Stream01CompatExt;
use futures::future::FutureExt;
use futures::stream::StreamExt;
use futures::{ready, Poll};
use hyper::client::connect::Connect;
use tokio::timer::Interval;

use crate::twitter::{self, Request as _};

use super::{Core, Sender};

pub struct TwitterListTimeline {
    inner: Option<Inner>,
}

struct Inner {
    list_id: u64,
    responses: Vec<twitter::ResponseFuture<Vec<twitter::Tweet>>>,
    interval: Interval,
    backfill: Option<Backfill>,
}

struct Backfill {
    since_id: i64,
    response: twitter::ResponseFuture<Vec<twitter::Tweet>>,
}

const RESP_BUF_CAP: usize = 8;

impl TwitterListTimeline {
    pub fn new<C>(list_id: u64, since_id: Option<i64>, core: &Core<C>) -> Self
    where
        C: Connect + Sync + 'static,
        C::Transport: 'static,
        C::Future: 'static,
    {
        let token = core.twitter_token(core.manifest().twitter.user).unwrap();

        let backfill = since_id.map(|since_id| {
            let response = twitter::lists::Statuses::new(list_id)
                .since_id(Some(since_id))
                .send(
                    core.manifest().twitter.client.as_ref(),
                    token,
                    core.http_client(),
                );
            Backfill { since_id, response }
        });

        let mut inner = Inner {
            list_id,
            responses: Vec::with_capacity(RESP_BUF_CAP),
            // Rate limit of GET lists/statuses (user auth): 900 reqs/15-min window (1 req/sec)
            interval: Interval::new_interval(Duration::from_secs(1)),
            backfill,
        };
        inner.send_request(core);

        TwitterListTimeline { inner: Some(inner) }
    }

    pub fn empty() -> Self {
        TwitterListTimeline { inner: None }
    }

    pub fn poll<C>(
        &mut self,
        core: &Core<C>,
        sender: &mut Sender,
        cx: &mut Context<'_>,
    ) -> Poll<Fallible<()>>
    where
        C: Connect + Sync + 'static,
        C::Transport: 'static,
        C::Future: 'static,
    {
        trace_fn!(TwitterListTimeline::poll::<C>);

        let inner = if let Some(ref mut inner) = self.inner {
            inner
        } else {
            return Poll::Ready(Ok(()));
        };

        if let Poll::Ready(Some(_)) = (&mut inner.interval).compat().poll_next_unpin(cx) {
            inner.send_request(core);
        }

        let tweets = match ready!(inner.poll_next(cx)) {
            Ok(resp) => resp.response,
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
                            inner.interval = Interval::new(now_i + eta, Duration::from_secs(1));
                        }
                    }
                }
                return Poll::Pending;
            }
        };

        if log_enabled!(log::Level::Trace) {
            if let Some(t) = tweets.first() {
                let created_at = super::snowflake_to_system_time(t.id as u64);
                match SystemTime::now().duration_since(created_at) {
                    Ok(latency) => trace!("Twitter list worst latency: {:.2?}", latency),
                    Err(e) => trace!("Twitter list worst latency: -{:.2?}", e.duration()),
                }
            }
        }

        for t in tweets {
            sender.send_tweet(t, core)?;
        }

        Poll::Pending
    }

    pub fn poll_backfill<C>(
        &mut self,
        core: &mut Core<C>,
        sender: &mut Sender,
        cx: &mut Context<'_>,
    ) -> Poll<Fallible<()>>
    where
        C: Connect + Sync + 'static,
        C::Transport: 'static,
        C::Future: 'static,
    {
        let (list_id, backfill_opt) = if let Some(Inner {
            list_id,
            ref mut backfill,
            ..
        }) = self.inner
        {
            (list_id, backfill)
        } else {
            return Poll::Ready(Ok(()));
        };

        let backfill = if let &mut Some(ref mut bf) = backfill_opt {
            bf
        } else {
            return Poll::Ready(Ok(()));
        };

        let tweets = match ready!(backfill.response.poll_unpin(cx)) {
            Ok(resp) => resp.response,
            Err(e) => {
                *backfill_opt = None;
                return Poll::Ready(Err(e.into()));
            }
        };

        if tweets.is_empty() {
            debug!("timeline backfilling completed");
            *backfill_opt = None;
            return Poll::Ready(Ok(()));
        }

        let max_id = tweets.iter().map(|t| t.id).min().map(|id| id - 1);
        self.inner
            .as_mut()
            .unwrap()
            .backfill
            .as_mut() // We can't just use `backfill.response` here
            .unwrap() // because `backfill` borrows `self` mutably, and...
            .response = twitter::lists::Statuses::new(list_id)
            .since_id(Some(backfill.since_id))
            .max_id(max_id)
            .send(
                // ...`self` is also borrowed here.
                core.manifest().twitter.client.as_ref(),
                core.twitter_token(core.manifest().twitter.user).unwrap(),
                core.http_client(),
            );

        for t in tweets {
            core.with_twitter_dump(|mut dump| {
                json::to_writer(&mut dump, &t)?;
                dump.write_all(b"\n")
            })?;
            if t.retweeted_status.is_none() {
                sender.send_tweet(t, core)?;
            }
        }

        Poll::Pending
    }
}

impl Inner {
    fn poll_next(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<twitter::Result<twitter::Response<Vec<twitter::Tweet>>>> {
        // Iterate from later responses.
        for (i, resp) in self.responses.iter_mut().enumerate().rev() {
            if let Poll::Ready(result) = resp.poll_unpin(cx) {
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

    fn send_request<C>(&mut self, core: &Core<C>)
    where
        C: Connect + Sync + 'static,
        C::Transport: 'static,
        C::Future: 'static,
    {
        trace_fn!(Inner::send_request::<C>);

        let resp = twitter::lists::Statuses::new(self.list_id)
            .count(None)
            .include_rts(Some(false))
            .send(
                core.manifest().twitter.client.as_ref(),
                core.twitter_token(core.manifest().twitter.user).unwrap(),
                core.http_client(),
            );

        if self.responses.len() == self.responses.capacity() {
            debug!("respone buffer reached its capacity");
            self.responses.remove(0);
        }

        self.responses.push(resp);
    }
}
