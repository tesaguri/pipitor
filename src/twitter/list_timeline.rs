mod interval;

use std::cmp;
use std::convert::TryFrom;
use std::fmt::Debug;
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
use crate::util::time::system_time_now;
use crate::util::{snowflake_to_system_time, system_time_to_snowflake, HttpService};

use super::{Request, Tweet};

use self::interval::Interval;

/// A stream that continuously yields Tweets from a list timeline.
///
/// This optionally _backfill_s past Tweets since the specified Tweet ID.
pub struct ListTimeline<S, B> {
    inner: Option<Inner<S, B>>,
}

struct Inner<S, B> {
    sender: Option<Arc<RequestSender<S, B>>>,
    rx: mpsc::Receiver<Vec<Tweet>>,
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
    sender: Arc<RequestSender<S, B>>,
    #[pin]
    response: super::ResponseFuture<Vec<Tweet>, S, B>,
}

impl<S, B> ListTimeline<S, B>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: Debug,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    pub fn new(
        list: &manifest::TwitterList,
        since_id: Option<i64>,
        client: Credentials<Box<str>>,
        token: Credentials<Box<str>>,
        http: S,
    ) -> Self {
        let manifest::TwitterList {
            id: list_id,
            interval,
            delay,
        } = *list;
        let (tx, rx) = mpsc::channel(0);

        let sender = Arc::new(RequestSender {
            list_id,
            delay,
            since_id: AtomicI64::new(0),
            tx,
            handle: interval::Handle::new(interval),
            client,
            token,
            http,
            marker: PhantomData,
        });

        if let Some(since_id) = since_id {
            let response = super::lists::Statuses::new(list.id)
                .since_id(Some(since_id))
                .send(&sender.client, &sender.token, sender.http.clone());
            tokio::spawn(Backfill {
                since_id,
                response,
                sender: sender.clone(),
            });
        }

        // Periodically send API requests in the background.
        tokio::spawn(Interval::new(Arc::downgrade(&sender)).for_each(|sender| {
            sender.send();
            future::ready(())
        }));

        let inner = Inner {
            sender: Some(sender),
            rx,
        };
        ListTimeline { inner: Some(inner) }
    }

    pub fn empty() -> Self {
        ListTimeline { inner: None }
    }

    pub fn shutdown(&mut self) {
        if let Some(ref mut inner) = self.inner {
            // Drop the `Arc` to stop the `Interval` task.
            // If a `Backfill` task is running, the `ListTimeline` will wait for it to complete.
            inner.sender = None;
        }
    }
}

impl<S, B> Stream for ListTimeline<S, B> {
    type Item = Vec<Tweet>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        trace_fn!(ListTimeline::<S, B>::poll_next);

        let inner = match self.inner.as_mut() {
            Some(inner) => inner,
            None => return Poll::Ready(None),
        };

        let tweets = if let Some(tweets) = ready!(inner.rx.poll_next_unpin(cx)) {
            tweets
        } else {
            // `None` should only be returned in a shutdown process.
            debug_assert!(inner.sender.is_none());
            return Poll::Ready(None);
        };

        if let Some(t) = tweets.first() {
            if log_enabled!(log::Level::Trace) {
                let created_at = snowflake_to_system_time(t.id as u64);
                match SystemTime::now().duration_since(created_at) {
                    Ok(latency) => trace!("Twitter list worst latency: {:.2?}", latency),
                    Err(e) => trace!("Twitter list worst latency: -{:.2?}", e.duration()),
                }
            }
        }

        Poll::Ready(Some(tweets))
    }
}

impl<S, B> Future for Backfill<S, B>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: Debug,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut this = self.project();

        let tweets = match ready!(this.response.as_mut().poll(cx)) {
            Ok(resp) => resp.data,
            Err(e) => {
                warn!("error while backfilling the list: {:?}", e);
                // Just discard the backfill.
                return Poll::Ready(());
            }
        };

        if tweets.is_empty() {
            debug!("timeline backfilling completed");
            return Poll::Ready(());
        }

        this.sender.set_since_id(tweets.first().unwrap().id);

        let res = super::lists::Statuses::new(this.sender.list_id)
            .since_id(Some(*this.since_id))
            .max_id(Some(tweets.last().unwrap().id - 1))
            .send(
                &this.sender.client,
                &this.sender.token,
                this.sender.http.clone(),
            );
        this.response.set(res);

        if let Err(e) = this.sender.tx.clone().start_send(tweets) {
            debug_assert!(e.is_disconnected());
            return Poll::Ready(());
        }

        cx.waker().wake_by_ref();

        Poll::Pending
    }
}

impl<S, B> RequestSender<S, B>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send + 'static,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: Debug,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    fn send(self: Arc<Self>) {
        trace_fn!(RequestSender::<S, B>::send);

        let since_id = self.since_id.load(Ordering::Acquire);
        let since_id = (since_id != 0).then(|| {
            let min = system_time_to_snowflake(system_time_now() - self.delay);
            cmp::min(since_id, i64::try_from(min).unwrap())
        });
        let count = if since_id.is_some() { 200 } else { 1 };

        let task = super::lists::Statuses::new(self.list_id)
            .count(Some(count))
            .include_rts(Some(false))
            .since_id(since_id)
            .send(&self.client, &self.token, self.http.clone())
            .map(move |result| {
                let rate_limit = match result {
                    Ok(resp) => {
                        if let Some(t) = resp.data.first() {
                            let id = t.id;
                            if let Err(e) = self.tx.clone().start_send(resp.data) {
                                debug_assert!(e.is_disconnected());
                                return;
                            }
                            self.set_since_id(id);
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

    fn set_since_id(&self, since_id: i64) {
        self.since_id.fetch_max(since_id, Ordering::AcqRel);
    }
}

impl<S, B> AsRef<interval::Handle> for RequestSender<S, B> {
    fn as_ref(&self) -> &interval::Handle {
        &self.handle
    }
}
