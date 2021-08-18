use std::future::{self, Future};
use std::marker::PhantomData;
use std::sync::Arc;

use diesel::dsl::*;
use diesel::prelude::*;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt, TryFutureExt};
use http_body::Body;
use pin_project::pin_project;

use crate::feed::{Entry, Feed};
use crate::manifest::Outbox;
use crate::schema::*;
use crate::util::HttpService;
use crate::{models, twitter};

use super::{Core, TwitterRequestExt as _};

/// An object used to send entries to their corresponding outboxes.
#[pin_project]
pub struct Sender<S, B>
where
    S: HttpService<B>,
{
    marker: PhantomData<fn() -> (S, B)>,
}

impl<S, B> Sender<S, B>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    pub fn new() -> Self {
        Default::default()
    }

    pub fn send_feed(&self, topic: &str, feed: Feed, core: &Core<S>) -> anyhow::Result<()> {
        trace_fn!(Sender::<S, B>::send_feed, topic, feed.id);

        for e in feed.entries {
            self.send_entry(topic, e, core)?;
        }

        Ok(())
    }

    pub fn send_entry(&self, topic: &str, entry: Entry, core: &Core<S>) -> anyhow::Result<()> {
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
        for outbox in core.router().route_entry(topic, entry) {
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
                                .values(&models::NewEntry::new(&shared.0, &shared.1).unwrap())
                                .execute(&*conn)?;
                            Ok(())
                        })
                        .map(|result| {
                            // TODO: better error handling
                            if let Err(e) = result {
                                error!("{:?}", e);
                            }
                        });
                    tokio::spawn(core.shutdown_handle().wrap_future(fut));
                }
            }
        }

        Ok(())
    }

    pub fn send_tweet(&self, tweet: twitter::Tweet, core: &Core<S>) -> anyhow::Result<()> {
        self.send_tweet_(tweet, core, true)
    }

    pub fn send_tweets<I>(&self, tweets: I, core: &Core<S>) -> anyhow::Result<()>
    where
        I: IntoIterator<Item = twitter::Tweet>,
        I::IntoIter: DoubleEndedIterator,
    {
        // Tweets are assumed to be sorted in descending order of posted time.
        let mut tweets = tweets.into_iter().rev().peekable();
        while let Some(t) = tweets.next() {
            self.send_tweet_(t, core, tweets.peek().is_none())?;
        }
        Ok(())
    }

    fn send_tweet_(
        &self,
        tweet: twitter::Tweet,
        core: &Core<S>,
        update_last_tweet: bool,
    ) -> anyhow::Result<()> {
        trace_fn!(Sender::<S, B>::send_tweet, tweet);

        let conn = core.conn()?;

        if select(exists(tweets::table.find(&tweet.id))).get_result::<bool>(&*conn)? {
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
            let task = self.retweet(&tweet, core)?;
            tokio::spawn(core.shutdown_handle().wrap_future(task));
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
        let retweet = self.retweet(&tweet, core)?;
        if let Some(dup) = duplicate {
            // Check whether the duplicate Tweet still exists
            let task =
                twitter::statuses::Show::new(dup)
                    .send(core, None)
                    .then(|result| async move {
                        if result.is_err() {
                            // Either the duplicate Tweet does not exist anymore (404)
                            // or the Tweet's existence could not be verified because of an error.
                            retweet.await;
                        }
                    });
            tokio::spawn(core.shutdown_handle().wrap_future(task));
        } else {
            tokio::spawn(core.shutdown_handle().wrap_future(retweet));
        }

        Ok(())
    }

    fn retweet(
        &self,
        tweet: &twitter::Tweet,
        core: &Core<S>,
    ) -> anyhow::Result<impl Future<Output = ()>> {
        let conn = core.conn()?;

        let tasks = FuturesUnordered::new();

        conn.transaction(|| {
            diesel::replace_into(tweets::table)
                .values(&models::NewTweet::from(tweet))
                .execute(&*conn)?;

            for outbox in core.router().route_tweet(tweet) {
                debug!("sending a Tweet to outbox {:?}: {:?}", outbox, tweet);

                match *outbox {
                    Outbox::Twitter(user) => {
                        diesel::replace_into(ongoing_retweets::table)
                            .values((
                                ongoing_retweets::id.eq(tweet.id),
                                ongoing_retweets::user.eq(user),
                            ))
                            .execute(&*conn)?;
                        let tweet_id = tweet.id;
                        let pool = core.database_pool().clone();
                        let task = twitter::statuses::Retweet::new(tweet.id)
                            .send(core, user)
                            .map_ok(|_| {})
                            .or_else(|e| {
                                if let twitter::Error::Twitter(ref e) = e {
                                    let is_negligible = |code| {
                                        use twitter::ErrorCode;
                                        [
                                            ErrorCode::YOU_HAVE_ALREADY_RETWEETED_THIS_TWEET,
                                            ErrorCode::NO_STATUS_FOUND_WITH_THAT_ID,
                                        ]
                                        .contains(&code)
                                    };
                                    if !e.codes().any(is_negligible) {
                                        return future::ready(Ok(()));
                                    }
                                }
                                future::ready(Err(e))
                            })
                            .map(move |result| {
                                if let Err(e) = result {
                                    error!("{:?}", e);
                                    return;
                                }
                                if let Ok(conn) = pool.get() {
                                    let row = ongoing_retweets::table.find((tweet_id, user));
                                    let _ = diesel::delete(row).execute(&*conn);
                                }
                            });
                        tasks.push(task);
                    }
                }
            }

            Ok(tasks.for_each(|()| future::ready(())))
        })
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
            marker: PhantomData,
        }
    }
}
