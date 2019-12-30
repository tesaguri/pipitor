mod retweet_queue;

use std::task::{Context, Poll};

use diesel::prelude::*;
use failure::Fallible;
use futures::ready;
use futures::stream::{FuturesUnordered, StreamExt};
use hyper::client::connect::Connect;
use serde::de;

use crate::rules::Outbox;
use crate::twitter;
use crate::util::ResolveWith;

use super::{Core, TwitterRequestExt as _};

use self::retweet_queue::{PendingRetweets, RetweetQueue};

/// A non-thread-safe object that sends entries to corresponding outboxes.
#[derive(Default)]
pub struct Sender {
    find_duplicate_tweet_queue:
        FuturesUnordered<ResolveWith<twitter::ResponseFuture<de::IgnoredAny>, twitter::Tweet>>,
    retweet_queue: RetweetQueue,
}

impl Sender {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn has_pending(&self) -> bool {
        !self.retweet_queue.is_empty()
    }

    pub fn send_tweet<C>(&mut self, tweet: twitter::Tweet, core: &Core<C>) -> Fallible<()>
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
        trace_fn!(Sender::send_tweet::<C>, "tweet={:?}", tweet);

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
                .filter(user_id.eq(&tweet.user.id).and(text.eq(&*tweet.text)))
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

    pub fn poll_done<C>(&mut self, core: &Core<C>, cx: &mut Context<'_>) -> Poll<Fallible<()>>
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
        let mut ready = true;

        ready &= self.poll_find_duplicate_tweet(core, cx).is_ready();
        ready &= self.retweet_queue.poll(core, cx)?.is_ready();

        if ready {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn poll_find_duplicate_tweet<C>(&mut self, core: &Core<C>, cx: &mut Context<'_>) -> Poll<()>
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
        while let Some((result, tweet)) =
            ready!(self.find_duplicate_tweet_queue.poll_next_unpin(cx))
        {
            if result.is_err() {
                // Either the duplicate Tweet does not exist anymore (404)
                // or the Tweet's existence could not be verified because of an error.
                self.retweet(tweet, core);
            }
        }

        Poll::Ready(())
    }

    fn retweet<C>(&mut self, tweet: twitter::Tweet, core: &Core<C>)
    where
        C: Connect + Clone + Send + Sync + 'static,
    {
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

        self.retweet_queue.insert(tweet, retweets);
    }
}
