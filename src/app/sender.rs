mod retweet_queue;

use std::pin::Pin;
use std::task::{Context, Poll};

use diesel::prelude::*;
use failure::Fallible;
use futures::ready;
use futures::stream::{FuturesUnordered, Stream};
use http_body::Body;
use pin_project::pin_project;
use serde::de;

use crate::rules::Outbox;
use crate::twitter;
use crate::util::{HttpResponseFuture, HttpService, ResolveWith};

use super::{Core, TwitterRequestExt as _};

use self::retweet_queue::{PendingRetweets, RetweetQueue};

/// A non-thread-safe object that sends entries to corresponding outboxes.
#[pin_project]
pub struct Sender<F>
where
    F: HttpResponseFuture,
{
    #[pin]
    find_duplicate_tweet_queue:
        FuturesUnordered<ResolveWith<twitter::ResponseFuture<de::IgnoredAny, F>, twitter::Tweet>>,
    #[pin]
    retweet_queue: RetweetQueue<F>,
}

impl<F> Sender<F>
where
    F: HttpResponseFuture,
    <F::Body as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    pub fn new() -> Self {
        Default::default()
    }

    pub fn has_pending(&self) -> bool {
        !self.retweet_queue.is_empty()
    }

    pub fn send_tweet<S>(
        self: Pin<&mut Self>,
        tweet: twitter::Tweet,
        core: &Core<S>,
    ) -> Fallible<()>
    where
        S: HttpService<hyper::Body, ResponseBody = F::Body, Error = F::Error, Future = F> + Clone,
    {
        trace_fn!(Sender::<F>::send_tweet::<S>, "tweet={:?}", tweet);

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

    pub fn poll_done<S>(
        mut self: Pin<&mut Self>,
        core: &Core<S>,
        cx: &mut Context<'_>,
    ) -> Poll<Fallible<()>>
    where
        S: HttpService<hyper::Body, ResponseBody = F::Body, Error = F::Error, Future = F> + Clone,
        F::Error: 'static,
        <F::Body as Body>::Error: 'static,
    {
        let mut ready = true;

        ready &= self.as_mut().poll_find_duplicate_tweet(core, cx).is_ready();
        ready &= self.project().retweet_queue.poll(core, cx)?.is_ready();

        if ready {
            Poll::Ready(Ok(()))
        } else {
            Poll::Pending
        }
    }

    fn poll_find_duplicate_tweet<S>(
        mut self: Pin<&mut Self>,
        core: &Core<S>,
        cx: &mut Context<'_>,
    ) -> Poll<()>
    where
        S: HttpService<hyper::Body, ResponseBody = F::Body, Error = F::Error, Future = F> + Clone,
        F::Error: 'static,
        <F::Body as Body>::Error: 'static,
    {
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

    fn retweet<S>(self: Pin<&mut Self>, tweet: twitter::Tweet, core: &Core<S>)
    where
        S: HttpService<hyper::Body, ResponseBody = F::Body, Error = F::Error, Future = F> + Clone,
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

        self.project().retweet_queue.insert(tweet, retweets);
    }
}

impl<F> Default for Sender<F>
where
    F: HttpResponseFuture,
    <F::Body as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    fn default() -> Self {
        Sender {
            find_duplicate_tweet_queue: Default::default(),
            retweet_queue: Default::default(),
        }
    }
}
