mod retweet_queue;

use std::pin::Pin;
use std::task::{Context, Poll};

use diesel::prelude::*;
use futures::ready;
use futures::stream::{FuturesUnordered, Stream};
use http_body::Body;
use pin_project::pin_project;
use serde::de;

use crate::rules::Outbox;
use crate::twitter;
use crate::util::{HttpService, ResolveWith};

use super::{Core, TwitterRequestExt as _};

use self::retweet_queue::{PendingRetweets, RetweetQueue};

/// A non-thread-safe object that sends entries to corresponding outboxes.
#[pin_project]
pub struct Sender<S, B>
where
    S: HttpService<B>,
{
    #[pin]
    find_duplicate_tweet_queue: FuturesUnordered<
        ResolveWith<twitter::ResponseFuture<de::IgnoredAny, S, B>, twitter::Tweet>,
    >,
    #[pin]
    retweet_queue: RetweetQueue<S, B>,
}

impl<S, B> Sender<S, B>
where
    S: HttpService<B> + Clone,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    pub fn new() -> Self {
        Default::default()
    }

    pub fn has_pending(&self) -> bool {
        !self.retweet_queue.is_empty()
    }

    pub fn send_tweet(
        self: Pin<&mut Self>,
        tweet: twitter::Tweet,
        core: &Core<S>,
    ) -> anyhow::Result<()> {
        trace_fn!(Sender::<S, B>::send_tweet, "tweet={:?}", tweet);

        let conn = core.conn()?;

        let already_processed = {
            use crate::schema::tweets::dsl::*;
            use diesel::dsl::*;

            self.retweet_queue.contains(tweet.id)
                || select(exists(tweets.find(&tweet.id))).get_result::<bool>(&*conn)?
        };

        if already_processed {
            trace!("the Tweet has already been processed");
            return Ok(());
        }

        {
            use crate::schema::last_tweet::dsl::*;

            diesel::update(last_tweet)
                .filter(status_id.lt(tweet.id))
                .set(status_id.eq(tweet.id))
                .execute(&*conn)?;
        }

        if tweet.retweeted_status.is_some() {
            return Ok(());
        }

        if !core.manifest().skip_duplicate {
            self.retweet(tweet, core);
            return Ok(());
        }

        if core.manifest().rule.route_tweet(&tweet).next().is_none() {
            return Ok(());
        }

        let duplicate = {
            use crate::schema::tweets::dsl::*;
            use diesel::dsl::*;

            tweets
                .filter(user_id.eq(&tweet.user.id))
                .filter(text.eq(&*tweet.text))
                .select(max(id))
                .first::<Option<i64>>(&*conn)?
        };

        if let Some(dup) = duplicate {
            // Check whether the duplicate Tweet still exists
            let res = twitter::statuses::Show::new(dup).send(core, None);
            self.find_duplicate_tweet_queue
                .push(ResolveWith::new(res, tweet));
        } else {
            self.retweet(tweet, core);
        }

        Ok(())
    }

    pub fn poll_done(
        mut self: Pin<&mut Self>,
        core: &Core<S>,
        cx: &mut Context<'_>,
    ) -> Poll<anyhow::Result<()>> {
        let mut ready = true;

        ready &= self.as_mut().poll_find_duplicate_tweet(core, cx).is_ready();
        ready &= self.project().retweet_queue.poll(core, cx)?.is_ready();

        if ready {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn poll_find_duplicate_tweet(
        mut self: Pin<&mut Self>,
        core: &Core<S>,
        cx: &mut Context<'_>,
    ) -> Poll<()> {
        while let Some((result, tweet)) = ready!(self
            .as_mut()
            .project()
            .find_duplicate_tweet_queue
            .poll_next(cx))
        {
            if result.is_err() {
                // Either the duplicate Tweet does not exist anymore (404)
                // or the Tweet's existence could not be verified because of an error.
                self.as_mut().retweet(tweet, core);
            }
        }

        Poll::Ready(())
    }

    fn retweet(self: Pin<&mut Self>, tweet: twitter::Tweet, core: &Core<S>) {
        let mut retweets = PendingRetweets::new();

        for outbox in core.manifest().rule.route_tweet(&tweet) {
            debug!("sending a Tweet to outbox {:?}: {:?}", outbox, tweet);

            match *outbox {
                Outbox::Twitter(user) => {
                    retweets.push(twitter::statuses::Retweet::new(tweet.id).send(core, user));
                }
                Outbox::None => {}
                _ => unimplemented!(),
            }
        }

        self.project().retweet_queue.insert(tweet, retweets);
    }
}

impl<S, B> Default for Sender<S, B>
where
    S: HttpService<B> + Clone,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    fn default() -> Self {
        Sender {
            find_duplicate_tweet_queue: Default::default(),
            retweet_queue: Default::default(),
        }
    }
}
