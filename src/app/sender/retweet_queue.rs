use std::collections::HashSet;
use std::pin::Pin;
use std::task::Context;

use diesel::prelude::*;
use failure::Fallible;
use futures::future::Future;
use futures::stream::{FuturesUnordered, StreamExt};
use futures::{ready, Poll};
use serde::de;

use crate::twitter;

use super::super::Core;

#[derive(Default)]
pub struct RetweetQueue {
    queue: FuturesUnordered<PendingRetweets>,
    tweet_ids: HashSet<i64>,
}

pub struct PendingRetweets {
    tweet: Option<twitter::Tweet>,
    queue: FuturesUnordered<twitter::ResponseFuture<de::IgnoredAny>>,
}

impl RetweetQueue {
    pub fn poll<C>(&mut self, core: &Core<C>, cx: &mut Context<'_>) -> Poll<Fallible<()>> {
        use crate::models::NewTweet;
        use crate::schema::tweets::dsl::*;

        let mut conn = None;
        while let Some(retweeted_status) = ready!(self.queue.poll_next_unpin(cx)?) {
            let conn = if let Some(ref c) = conn {
                c
            } else {
                conn = Some(core.conn()?);
                conn.as_ref().unwrap()
            };

            diesel::replace_into(tweets)
                .values(&NewTweet::from(&retweeted_status))
                .execute(&*conn)?;
            self.tweet_ids.remove(&retweeted_status.id);
        }

        Poll::Ready(Ok(()))
    }

    pub fn contains(&self, tweet_id: i64) -> bool {
        self.tweet_ids.contains(&tweet_id)
    }

    pub fn insert(&mut self, tweet: twitter::Tweet, mut retweets: PendingRetweets) {
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

impl PendingRetweets {
    pub fn new() -> Self {
        PendingRetweets {
            tweet: None,
            queue: FuturesUnordered::new(),
        }
    }

    pub fn push(&mut self, request: twitter::ResponseFuture<de::IgnoredAny>) {
        self.queue.push(request)
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl Future for PendingRetweets {
    type Output = Result<twitter::Tweet, twitter::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        trace_fn!(PendingRetweets::poll);

        while let Poll::Ready(v) = self.queue.poll_next_unpin(cx) {
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
                        .tweet
                        .take()
                        .expect("polled `PendingRetweets` after completion")));
                }
            }
        }

        Poll::Pending
    }
}
