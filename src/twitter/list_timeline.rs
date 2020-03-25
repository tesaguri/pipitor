use std::num::NonZeroU64;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use futures::{ready, Future, Stream, StreamExt};
use http_body::Body;
use oauth1::Credentials;
use pin_project::{pin_project, project};
use smallvec::SmallVec;
use tokio::time::Interval;

use crate::util::{snowflake_to_system_time, HttpService};

use super::{Request, Tweet};

/// A stream that continuously yields Tweets from a list timeline.
///
/// This optionally _backfill_s past Tweets since the specified Tweet ID.
#[pin_project]
pub struct ListTimeline<S, B>
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
    current: Current<S, B>,
    #[pin]
    backfill: Option<Backfill<S, B>>,
}

struct Current<S, B>
where
    S: HttpService<B>,
{
    list_id: NonZeroU64,
    since_id: Option<i64>,
    responses: SmallVec<[Pin<Box<super::ResponseFuture<Vec<Tweet>, S, B>>>; RESP_BUF_CAP]>,
    interval: Interval,
    client: Credentials<Box<str>>,
    token: Credentials<Box<str>>,
    http: S,
}

#[pin_project]
struct Backfill<S, B>
where
    S: HttpService<B>,
{
    since_id: i64,
    #[pin]
    response: super::ResponseFuture<Vec<Tweet>, S, B>,
}

impl<S, B> ListTimeline<S, B>
where
    S: HttpService<B> + Clone,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    pub fn new(
        list_id: NonZeroU64,
        since_id: Option<i64>,
        client: Credentials<Box<str>>,
        token: Credentials<Box<str>>,
        http: S,
    ) -> Self {
        let backfill = since_id.map(|since_id| {
            let response = super::lists::Statuses::new(list_id)
                .since_id(Some(since_id))
                .send(&client, &token, http.clone());
            Backfill { since_id, response }
        });

        let mut current = Current {
            list_id,
            since_id: None,
            responses: SmallVec::new(),
            // Rate limit of GET lists/statuses (user auth): 900 reqs/15-min window (1 req/sec)
            interval: tokio::time::interval(Duration::from_secs(1)),
            client,
            token,
            http,
        };
        current.send_request();

        let inner = Inner { current, backfill };
        ListTimeline { inner: Some(inner) }
    }

    pub fn empty() -> Self {
        ListTimeline { inner: None }
    }

    pub fn poll_next_backfill(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Vec<Tweet>>> {
        match self.project().inner.as_pin_mut() {
            Some(inner) => inner.poll_next_backfill(cx),
            None => return Poll::Ready(None),
        }
    }
}

impl<S, B> Stream for ListTimeline<S, B>
where
    S: HttpService<B> + Clone,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    type Item = anyhow::Result<Vec<Tweet>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        trace_fn!(ListTimeline::<S, B>::poll_next);

        let mut inner = match self.project().inner.as_pin_mut() {
            Some(inner) => inner,
            None => return Poll::Ready(None),
        };

        if let Poll::Ready(Some(tweets)) = inner.as_mut().poll_next_backfill(cx) {
            return Poll::Ready(Some(Ok(tweets)));
        }

        if let Poll::Ready(Some(_)) = inner
            .as_mut()
            .project()
            .current
            .interval
            .poll_next_unpin(cx)
        {
            inner.as_mut().project().current.send_request();
        }

        let tweets = match ready!(inner.as_mut().project().current.poll_next(cx)) {
            Ok(resp) => resp.data,
            Err(e) => {
                // Skip the error as the list timeline is only for complementary purpose.
                warn!("error while retrieving Tweets from the list: {:?}", e);
                if let super::Error::Twitter(e) = e {
                    if let Some(limit) = e.rate_limit {
                        if limit.remaining == 0 {
                            let reset = SystemTime::UNIX_EPOCH + Duration::from_secs(limit.reset);
                            let (now_s, now_i) = (SystemTime::now(), Instant::now());
                            let eta = reset
                                .duration_since(now_s)
                                .unwrap_or(Duration::from_secs(0));
                            let at = (now_i + eta).into();
                            inner.project().current.interval =
                                tokio::time::interval_at(at, Duration::from_secs(1));
                        }
                    }
                }
                return Poll::Pending;
            }
        };

        if let Some(t) = tweets.last() {
            inner.project().current.since_id = Some(t.id);

            if log_enabled!(log::Level::Trace) {
                let created_at = snowflake_to_system_time(t.id as u64);
                match SystemTime::now().duration_since(created_at) {
                    Ok(latency) => trace!("Twitter list worst latency: {:.2?}", latency),
                    Err(e) => trace!("Twitter list worst latency: -{:.2?}", e.duration()),
                }
            }
        }

        Poll::Ready(Some(Ok(tweets)))
    }
}

impl<S, B> Inner<S, B>
where
    S: HttpService<B> + Clone,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    #[project]
    fn poll_next_backfill(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Vec<Tweet>>> {
        #[project]
        let Inner {
            current,
            backfill: mut backfill_opt,
        } = self.project();

        #[project]
        let Backfill {
            since_id: backfill_since_id,
            mut response,
        } = if let Some(bf) = backfill_opt.as_mut().as_pin_mut() {
            bf.project()
        } else {
            return Poll::Ready(None);
        };

        let tweets = match ready!(response.as_mut().poll(cx)) {
            Ok(resp) => resp.data,
            Err(e) => {
                warn!("error while backfilling the list: {:?}", e);
                // Just discard the backfill.
                backfill_opt.set(None);
                return Poll::Ready(None);
            }
        };

        if tweets.is_empty() {
            debug!("timeline backfilling completed");
            backfill_opt.set(None);
            return Poll::Ready(None);
        }

        if current.since_id.is_none() {
            current.since_id = tweets.first().map(|t| t.id);
        }

        let max_id = tweets.last().map(|t| t.id - 1);
        let res = super::lists::Statuses::new(current.list_id)
            .since_id(Some(*backfill_since_id))
            .max_id(max_id)
            .send(&current.client, &current.token, current.http.clone());
        response.set(res);

        Poll::Ready(Some(tweets))
    }
}

impl<S, B> Current<S, B>
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
            super::Response<Vec<Tweet>>,
            super::Error<S::Error, <S::ResponseBody as Body>::Error>,
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

    fn send_request(&mut self) {
        trace_fn!(Current::<S, B>::send_request);

        let count = if self.since_id.is_some() { 200 } else { 1 };
        let resp = super::lists::Statuses::new(self.list_id)
            .count(Some(count))
            .include_rts(Some(false))
            .since_id(self.since_id)
            .send(&self.client, &self.token, self.http.clone());

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
