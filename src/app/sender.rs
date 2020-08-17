mod retweet_queue;

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use diesel::prelude::*;
use futures::stream::{FuturesUnordered, Stream};
use futures::{ready, FutureExt};
use http_body::Body;
use pin_project::pin_project;
use serde::de;

use crate::feed::{Entry, Feed};
use crate::manifest::Outbox;
use crate::models::NewEntry;
use crate::schema::*;
use crate::twitter;
use crate::util::{HttpService, ResolveWith};
use diesel::dsl::*;

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
    S: HttpService<B> + Clone + Send + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    pub fn new() -> Self {
        Default::default()
    }

    pub fn has_pending(&self) -> bool {
        !self.retweet_queue.is_empty()
    }

    pub fn send_feed(
        mut self: Pin<&mut Self>,
        topic: &str,
        feed: Feed,
        core: &Core<S>,
    ) -> anyhow::Result<()> {
        trace_fn!(Sender::<S, B>::send_feed, topic, feed.id);

        for e in feed.entries {
            self.as_mut().send_entry(topic, e, core)?;
        }

        Ok(())
    }

    pub fn send_entry(
        self: Pin<&mut Self>,
        topic: &str,
        entry: Entry,
        core: &Core<S>,
    ) -> anyhow::Result<()> {
        trace_fn!(Sender::<S, B>::send_entry, entry);

        let link = if let Some(ref link) = entry.link {
            link
        } else {
            debug!("Received an entry with no link: {:?}", entry);
            return Ok(());
        };

        let conn = core.conn()?;

        let row = entries::table
            .find((topic, &entry.id))
            .or_filter(entries::link.eq(link));
        let already_processed = select(exists(row)).get_result::<bool>(&*conn)?;
        if already_processed {
            trace!("The entry has already been processed");
            return Ok(());
        }

        let text = if let Some(mut text) = entry.title.clone() {
            const TITLE_LEN: usize = twitter::text::MAX_WEIGHTED_TWEET_LENGTH
                - 1
                - twitter::text::TRANSFORMED_URL_LENGTH;
            twitter::text::sanitize(&mut text, TITLE_LEN);
            text.push('\n');
            text.push_str(link);
            text
        } else {
            link.to_owned()
        };

        let shared = Arc::new((Box::<str>::from(topic), entry));
        let entry = &shared.1;
        let mut conn = Some(conn);
        for outbox in core.router().route_entry(topic, &entry) {
            debug!("sending an entry to outbox {:?}: {:?}", outbox, entry);

            match *outbox {
                Outbox::Twitter(user) => {
                    let shared = shared.clone();
                    let conn = conn.take().map(Ok).unwrap_or_else(|| core.conn())?;
                    let fut = twitter::statuses::Update::new(&text)
                        .send(core, user)
                        .map(move |result| -> anyhow::Result<()> {
                            result?;
                            diesel::replace_into(entries::table)
                                .values(&NewEntry::new(&shared.0, &shared.1).unwrap())
                                .execute(&*conn)?;
                            Ok(())
                        })
                        .map(|result| {
                            // TODO: better error handling
                            if let Err(e) = result {
                                error!("{:?}", e);
                            }
                        });
                    tokio::spawn(fut);
                }
            }
        }

        Ok(())
    }

    pub fn send_tweet(
        self: Pin<&mut Self>,
        tweet: twitter::Tweet,
        core: &Core<S>,
    ) -> anyhow::Result<()> {
        self.send_tweet_(tweet, core, true)
    }

    pub fn send_tweets<I>(mut self: Pin<&mut Self>, tweets: I, core: &Core<S>) -> anyhow::Result<()>
    where
        I: IntoIterator<Item = twitter::Tweet>,
        I::IntoIter: DoubleEndedIterator,
    {
        // Tweets are assumed to be sorted in descending order of posted time.
        let mut tweets = tweets.into_iter().rev().peekable();
        while let Some(t) = tweets.next() {
            self.as_mut()
                .send_tweet_(t, core, tweets.peek().is_none())?;
        }
        Ok(())
    }

    fn send_tweet_(
        self: Pin<&mut Self>,
        tweet: twitter::Tweet,
        core: &Core<S>,
        update_last_tweet: bool,
    ) -> anyhow::Result<()> {
        trace_fn!(Sender::<S, B>::send_tweet, tweet);

        let conn = core.conn()?;

        let already_processed = self.retweet_queue.contains(tweet.id)
            || select(exists(tweets::table.find(&tweet.id))).get_result::<bool>(&*conn)?;
        if already_processed {
            trace!("the Tweet has already been processed");
            return Ok(());
        }

        if update_last_tweet {
            diesel::update(last_tweet::table)
                .filter(last_tweet::status_id.lt(tweet.id))
                .set(last_tweet::status_id.eq(tweet.id))
                .execute(&*conn)?;
        }

        if tweet.retweeted_status.is_some() {
            return Ok(());
        }

        if !core.manifest().skip_duplicate {
            self.retweet(tweet, core);
            return Ok(());
        }

        if core.router().route_tweet(&tweet).next().is_none() {
            return Ok(());
        }

        let duplicate = tweets::table
            .filter(tweets::user_id.eq(&tweet.user.id))
            .filter(tweets::text.eq(&*tweet.text))
            .select(max(tweets::id))
            .first::<Option<i64>>(&*conn)?;
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

        for outbox in core.router().route_tweet(&tweet) {
            debug!("sending a Tweet to outbox {:?}: {:?}", outbox, tweet);

            match *outbox {
                Outbox::Twitter(user) => {
                    retweets.push(twitter::statuses::Retweet::new(tweet.id).send(core, user));
                }
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
