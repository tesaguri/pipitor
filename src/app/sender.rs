mod retweet_queue;

use std::task::{Context, Poll};

use diesel::prelude::*;
use failure::Fallible;
use hyper::client::connect::Connect;

use crate::rules::Outbox;
use crate::twitter;
use crate::util::TwitterRequestExt as _;

use super::Core;

use self::retweet_queue::{PendingRetweets, RetweetQueue};

/// A non-thread-safe object that sends entries to corresponding outboxes.
#[derive(Default)]
pub struct Sender {
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
        C: Connect + Sync + 'static,
        C::Transport: 'static,
        C::Future: 'static,
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

        Ok(())
    }

    pub fn poll_done<C>(&mut self, core: &Core<C>, cx: &mut Context<'_>) -> Poll<Fallible<()>> {
        self.retweet_queue.poll(core, cx)
    }
}
