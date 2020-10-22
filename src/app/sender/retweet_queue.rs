use std::collections::HashSet;
use std::pin::Pin;
use std::task::{Context, Poll};

use diesel::prelude::*;
use futures::future::Future;
use futures::ready;
use futures::stream::{FuturesUnordered, Stream};
use http_body::Body;
use pin_project::pin_project;
use serde::de;

use crate::schema::*;
use crate::util::HttpService;
use crate::{models, twitter};

use super::super::Core;

#[pin_project]
pub struct RetweetQueue<S, B>
where
    S: HttpService<B>,
{
    #[pin]
    queue: FuturesUnordered<PendingRetweets<S, B>>,
    tweet_ids: HashSet<i64>,
}

#[pin_project]
pub struct PendingRetweets<S, B>
where
    S: HttpService<B>,
{
    tweet: Option<twitter::Tweet>,
    #[pin]
    queue: FuturesUnordered<twitter::ResponseFuture<de::IgnoredAny, S, B>>,
}

impl<S, B> RetweetQueue<S, B>
where
    S: HttpService<B>,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    pub fn poll(
        mut self: Pin<&mut Self>,
        core: &Core<S>,
        cx: &mut Context<'_>,
    ) -> Poll<anyhow::Result<()>> {
        let mut conn = None;
        while let Some(retweeted_status) = ready!(self.as_mut().project().queue.poll_next(cx)?) {
            let conn = if let Some(ref c) = conn {
                c
            } else {
                conn = Some(core.conn()?);
                conn.as_ref().unwrap()
            };

            diesel::replace_into(tweets::table)
                .values(&models::NewTweet::from(&retweeted_status))
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

    pub fn insert(&mut self, tweet: twitter::Tweet, mut retweets: PendingRetweets<S, B>) {
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

impl<S, B> PendingRetweets<S, B>
where
    S: HttpService<B>,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    pub fn new() -> Self {
        PendingRetweets {
            tweet: None,
            queue: FuturesUnordered::new(),
        }
    }

    pub fn push(&mut self, request: twitter::ResponseFuture<de::IgnoredAny, S, B>) {
        self.queue.push(request)
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl<S, B> Future for PendingRetweets<S, B>
where
    S: HttpService<B>,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    type Output =
        Result<twitter::Tweet, twitter::Error<S::Error, <S::ResponseBody as Body>::Error>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        trace_fn!(PendingRetweets::<S, B>::poll);

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

impl<S, B> Default for RetweetQueue<S, B>
where
    S: HttpService<B>,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    fn default() -> Self {
        RetweetQueue {
            queue: Default::default(),
            tweet_ids: Default::default(),
        }
    }
}
