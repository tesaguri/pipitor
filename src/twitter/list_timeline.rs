mod interval;

use std::borrow::Borrow;
use std::future::{self, Future};
use std::marker::PhantomData;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use futures::channel::mpsc;
use futures::{ready, FutureExt, Stream, StreamExt};
use http_body::Body;
use oauth_credentials::Credentials;
use pin_project::pin_project;

use crate::manifest;
use crate::util::{snowflake_to_system_time, HttpService};

use super::{Request, Tweet};

use self::interval::Interval;

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

#[pin_project(project = InnerProj)]
struct Inner<S, B>
where
    S: HttpService<B>,
{
    rx: mpsc::Receiver<Vec<Tweet>>,
    sender: Arc<RequestSender<S, B>>,
    #[pin]
    backfill: Option<Backfill<S, B>>,
}

struct RequestSender<S, B> {
    list_id: NonZeroU64,
    delay: Duration,
    since_id: AtomicI64,
    tx: mpsc::Sender<Vec<Tweet>>,
    handle: interval::Handle,
    client: Credentials<Box<str>>,
    token: Credentials<Box<str>>,
    http: S,
    marker: PhantomData<fn() -> B>,
}

#[pin_project(project = BackfillProj)]
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
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    pub fn new(
        list: &manifest::TwitterList,
        since_id: Option<i64>,
        client: Credentials<Box<str>>,
        token: Credentials<Box<str>>,
        http: S,
    ) -> Self {
        let backfill = since_id.map(|since_id| {
            let response = super::lists::Statuses::new(list.id)
                .since_id(Some(since_id))
                .send(&client, &token, http.clone());
            Backfill { since_id, response }
        });

        let manifest::TwitterList { id: list_id, delay } = *list;
        let (tx, rx) = mpsc::channel(0);

        let sender = Arc::new(RequestSender {
            list_id,
            delay,
            since_id: AtomicI64::new(0),
            tx,
            handle: interval::Handle::new(),
            client,
            token,
            http,
            marker: PhantomData,
        });

        // Periodically send API requests in the background.
        tokio::spawn(Interval::new(Arc::downgrade(&sender)).for_each(|sender| {
            sender.send();
            future::ready(())
        }));

        let inner = Inner {
            sender,
            rx,
            backfill,
        };
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
            None => Poll::Ready(None),
        }
    }
}

impl<S, B> Stream for ListTimeline<S, B>
where
    S: HttpService<B> + Clone + Send + 'static,
    S::Future: Send + 'static,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>> + Send + 'static,
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

        let tweets = ready!(inner.as_mut().project().rx.poll_next_unpin(cx)).unwrap();

        if let Some(t) = tweets.last() {
            inner
                .project()
                .sender
                .since_id
                .fetch_max(t.id, Ordering::Relaxed);

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
    fn poll_next_backfill(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Vec<Tweet>>> {
        let InnerProj {
            sender,
            backfill: mut backfill_opt,
            ..
        } = self.project();

        let BackfillProj {
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

        sender
            .since_id
            .fetch_max(tweets.first().unwrap().id, Ordering::Relaxed);

        let max_id = tweets.last().map(|t| t.id - 1);
        let res = super::lists::Statuses::new(sender.list_id)
            .since_id(Some(*backfill_since_id))
            .max_id(max_id)
            .send(&sender.client, &sender.token, sender.http.clone());
        response.set(res);

        Poll::Ready(Some(tweets))
    }
}

impl<S, B> RequestSender<S, B>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    fn send(self: Arc<Self>) {
        trace_fn!(RequestSender::<S, B>::send);

        let since_id = self.since_id.load(Ordering::Relaxed);
        let since_id = if since_id == 0 {
            None
        } else if self.delay == Duration::from_secs(0) {
            Some(since_id)
        } else {
            // Subtract `delay` from the "time part"
            // and round down the non-time part of Snowflake ID.
            Some(((since_id >> 22) - self.delay.as_millis() as i64) << 22)
        };
        let count = if since_id.is_some() { 200 } else { 1 };

        let task = super::lists::Statuses::new(self.list_id)
            .count(Some(count))
            .include_rts(Some(false))
            .since_id(since_id)
            .send(&self.client, &self.token, self.http.clone())
            .map(move |result| {
                let rate_limit = match result {
                    Ok(resp) => {
                        if let Err(e) = self.tx.clone().start_send(resp.data) {
                            debug_assert!(e.is_disconnected());
                            return;
                        }
                        resp.rate_limit
                    }
                    Err(e) => {
                        // This error should not abort the whole task
                        // since the request will be retried soon.
                        warn!("error while retrieving Tweets from the list: {:?}", e);

                        if let super::Error::Twitter(e) = e {
                            e.rate_limit
                        } else {
                            return;
                        }
                    }
                };

                if let Some(limit) = rate_limit {
                    if limit.remaining == 0 {
                        self.handle.delay(limit.reset);
                    }
                }
            });

        tokio::spawn(task);
    }
}

impl<S, B> Borrow<interval::Handle> for RequestSender<S, B> {
    fn borrow(&self) -> &interval::Handle {
        &self.handle
    }
}
