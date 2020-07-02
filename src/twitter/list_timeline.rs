use std::cmp;
use std::marker::PhantomData;
use std::num::NonZeroU64;
use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime};

use futures::channel::mpsc;
use futures::task::AtomicWaker;
use futures::{ready, Future, FutureExt, Stream, StreamExt};
use http_body::Body;
use oauth1::Credentials;
use pin_project::pin_project;
use tokio::time::Delay;

use crate::manifest;
use crate::util::{instant_from_epoch, snowflake_to_system_time, HttpService};

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
    delay: u64,
    since_id: AtomicI64,
    tx: mpsc::Sender<Vec<Tweet>>,
    delay_until: AtomicI64,
    task: AtomicWaker,
    client: Credentials<Box<str>>,
    token: Credentials<Box<str>>,
    http: S,
    marker: PhantomData<fn() -> B>,
}

struct SenderTask<S, B> {
    sender: Weak<RequestSender<S, B>>,
    timer: Delay,
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
        list: manifest::TwitterList,
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

        let manifest::TwitterList { id: list_id, delay } = list;
        let (tx, rx) = mpsc::channel(0);

        let sender = Arc::new(RequestSender {
            list_id,
            delay,
            since_id: AtomicI64::new(0),
            tx,
            delay_until: AtomicI64::new(0),
            task: AtomicWaker::new(),
            client,
            token,
            http,
            marker: PhantomData,
        });

        tokio::spawn(SenderTask {
            sender: Arc::downgrade(&sender),
            timer: tokio::time::delay_until(Instant::now().into()),
        });

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
            None => return Poll::Ready(None),
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
            store_max(&inner.project().sender.since_id, t.id);

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

        store_max(&sender.since_id, tweets.first().unwrap().id);

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

        let since_id = self.since_id.load(Ordering::SeqCst);
        let since_id = if since_id == 0 {
            None
        } else {
            // Subtract `delay` milliseconds from the "time part" of Snowflake ID.
            Some(since_id - (self.delay << 22) as i64)
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
                        store_max(&self.delay_until, limit.reset as i64);
                    }
                }
            });

        tokio::spawn(task);
    }
}

impl<S, B> RequestSender<S, B> {
    fn decode_delay_until(&self) -> Option<Instant> {
        let delay_until = self.delay_until.load(Ordering::SeqCst);
        if delay_until == 0 {
            None
        } else {
            Some(instant_from_epoch(delay_until))
        }
    }
}

impl<S, B> Drop for RequestSender<S, B> {
    fn drop(&mut self) {
        self.task.wake();
    }
}

impl<S, B> Future for SenderTask<S, B>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        trace_fn!(SenderTask::<S, B>::poll);

        let sender = if let Some(sender) = self.sender.upgrade() {
            sender
        } else {
            return Poll::Ready(());
        };

        sender.task.register(cx.waker());

        if let Some(delay_until) = sender.decode_delay_until() {
            if self.timer.deadline().into_std() < delay_until {
                self.timer.reset(delay_until.into());
            }
        }

        ready!(self.timer.poll_unpin(cx));

        let now = Instant::now();

        sender.send();

        // Make the task sleep for 1 sec because the rate limit of GET lists/statuses is
        // 900 reqs/15-min window (about 1 req/sec).
        let next_tick = self.timer.deadline() + Duration::from_secs(1);
        self.timer.reset(cmp::max(next_tick, now.into()));

        Poll::Pending
    }
}

fn store_max(atomic: &AtomicI64, val: i64) {
    let mut prev = atomic.load(Ordering::SeqCst);
    while prev < val {
        match atomic.compare_exchange_weak(prev, val, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return,
            Err(p) => prev = p,
        }
    }
}
