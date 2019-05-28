use std::collections::HashSet;
use std::pin::Pin;
use std::task::Context;

use futures::future::Future;
use futures::stream::{FuturesUnordered, StreamExt};
use futures::Poll;
use serde::de;

use crate::twitter;

#[derive(Default)]
pub struct RetweetQueue {
    pub queue: FuturesUnordered<PendingRetweets>,
    pub tweet_ids: HashSet<i64>,
}

pub struct PendingRetweets {
    tweet: Option<twitter::Tweet>,
    queue: FuturesUnordered<twitter::ResponseFuture<de::IgnoredAny>>,
}

impl RetweetQueue {
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
