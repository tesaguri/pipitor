use std::collections::HashSet;
use std::pin::Pin;
use std::task::{Context, Poll};

use diesel::prelude::*;
use failure::Fallible;
use futures::future::Future;
use futures::ready;
use futures::stream::{FuturesUnordered, Stream};
use http_body::Body;
use pin_project::pin_project;
use serde::de;

use crate::twitter;
use crate::util::HttpResponseFuture;

use super::super::Core;

#[pin_project]
pub struct RetweetQueue<F>
where
    F: HttpResponseFuture,
{
    #[pin]
    queue: FuturesUnordered<PendingRetweets<F>>,
    tweet_ids: HashSet<i64>,
}

#[pin_project]
pub struct PendingRetweets<F>
where
    F: HttpResponseFuture,
{
    tweet: Option<twitter::Tweet>,
    #[pin]
    queue: FuturesUnordered<twitter::ResponseFuture<de::IgnoredAny, F>>,
}

impl<F> RetweetQueue<F>
where
    F: HttpResponseFuture,
    <F::Body as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    pub fn poll<S>(
        mut self: Pin<&mut Self>,
        core: &Core<S>,
        cx: &mut Context<'_>,
    ) -> Poll<Fallible<()>>
    where
        F::Error: 'static,
        <F::Body as Body>::Error: 'static,
    {
        use crate::models::NewTweet;
        use crate::schema::tweets::dsl::*;

        let mut conn = None;
        while let Some(retweeted_status) = ready!(self.as_mut().project().queue.poll_next(cx)?) {
            let conn = if let Some(ref c) = conn {
                c
            } else {
                conn = Some(core.conn()?);
                conn.as_ref().unwrap()
            };

            diesel::replace_into(tweets)
                .values(&NewTweet::from(&retweeted_status))
                .execute(&*conn)?;
            self.as_mut()
                .project()
                .tweet_ids
                .remove(&retweeted_status.id);
        }

        Poll::Ready(Ok(()))
    }

    pub fn contains(&self, tweet_id: i64) -> bool {
        self.tweet_ids.contains(&tweet_id)
    }

    pub fn insert(&mut self, tweet: twitter::Tweet, mut retweets: PendingRetweets<F>) {
        if retweets.is_empty() {
            return;
        }
        retweets.tweet = Some(tweet);
        self.queue.push(retweets);
    }

    pub fn is_empty(&self) -> bool {
        self.tweet_ids.is_empty()
    }
}

impl<F> PendingRetweets<F>
where
    F: HttpResponseFuture,
    <F::Body as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    pub fn new() -> Self {
        PendingRetweets {
            tweet: None,
            queue: FuturesUnordered::new(),
        }
    }

    pub fn push(&mut self, request: twitter::ResponseFuture<de::IgnoredAny, F>) {
        self.queue.push(request)
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl<F> Future for PendingRetweets<F>
where
    F: HttpResponseFuture,
    <F::Body as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    type Output = Result<twitter::Tweet, twitter::Error<F::Error, <F::Body as Body>::Error>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        trace_fn!(PendingRetweets::<F>::poll);

        while let Poll::Ready(v) = self.as_mut().project().queue.poll_next(cx) {
            match v {
                Some(Ok(_)) => {}
                Some(Err(e)) => {
                    if let twitter::Error::Twitter(ref e) = e {
                        let is_negligible = |code| {
                            code == twitter::ErrorCode::YOU_HAVE_ALREADY_RETWEETED_THIS_TWEET
                                || code == twitter::ErrorCode::NO_STATUS_FOUND_WITH_THAT_ID
                        };
                        if e.codes().any(is_negligible) {
                            continue;
                        }
                    }
                    return Poll::Ready(Err(e));
                }
                None => {
                    return Poll::Ready(Ok(self
                        .project()
                        .tweet
                        .take()
                        .expect("polled `PendingRetweets` after completion")));
                }
            }
        }

        Poll::Pending
    }
}

impl<F> Default for RetweetQueue<F>
where
    F: HttpResponseFuture,
    <F::Body as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    fn default() -> Self {
        RetweetQueue {
            queue: Default::default(),
            tweet_ids: Default::default(),
        }
    }
}
