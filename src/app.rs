use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::task::Context;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};
use diesel::result::{DatabaseErrorKind, Error as QueryError};
use diesel::SqliteConnection;
use failure::{Fail, Fallible, ResultExt};
use futures::compat::{Future01CompatExt, Stream01CompatExt};
use futures::future::{self, FusedFuture, Future, FutureExt};
use futures::ready;
use futures::stream::{FuturesUnordered, StreamExt, TryStreamExt};
use futures::Poll;
use hyper::client::connect::Connect;
use hyper::Client;
use hyper::StatusCode;
use itertools::Itertools;
use serde::de;
use twitter_stream::{TwitterStream, TwitterStreamBuilder};

use crate::models;
use crate::rules::{Outbox, TopicId};
use crate::twitter::{self, Request as _};
use crate::util::Maybe;
use crate::Manifest;

pub struct App<C> {
    core: Core<C>,
    twitter_backfill: Option<TwitterBackfill>,
    twitter: TwitterStream,
    twitter_done: bool,
    rt_queue: FuturesUnordered<RTQueue>,
    pending_rts: HashSet<i64>,
}

struct Core<C> {
    manifest: Manifest,
    pool: Pool<ConnectionManager<SqliteConnection>>,
    client: Client<C>,
    twitter_tokens: HashMap<i64, twitter::Credentials<Box<str>>>,
}

struct TwitterBackfill {
    list: u64,
    since_id: i64,
    response: twitter::ResponseFuture<Vec<twitter::Tweet>>,
}

struct RTQueue {
    tweet: Option<twitter::Tweet>,
    pending: FuturesUnordered<twitter::ResponseFuture<de::IgnoredAny>>,
}

#[cfg(feature = "native-tls")]
impl App<hyper_tls::HttpsConnector<hyper::client::HttpConnector>> {
    pub async fn new(manifest: Manifest) -> Fallible<Self> {
        let conn = hyper_tls::HttpsConnector::new(4).context("failed to initialize TLS client")?;
        let client = Client::builder().build(conn);
        Self::with_http_client(client, manifest).await
    }
}

impl<C: Connect> App<C>
where
    C: Connect + Sync + 'static,
    C::Transport: 'static,
    C::Future: 'static,
{
    pub async fn with_http_client(client: Client<C>, manifest: Manifest) -> Fallible<Self> {
        trace_fn!(App::<C>::with_http_client);

        let core = Core::new(manifest, client).await?;
        let (twitter_backfill, twitter) = core.init_twitter().await?;

        Ok(App {
            core,
            twitter_backfill,
            twitter,
            twitter_done: false,
            rt_queue: FuturesUnordered::new(),
            pending_rts: HashSet::new(),
        })
    }

    pub fn process_tweet(&mut self, tweet: twitter::Tweet) -> Fallible<()> {
        trace_fn!(App::<C>::process_tweet, "tweet={:?}", tweet);

        let conn = self.conn()?;

        let already_processed = {
            use crate::schema::tweets::dsl::*;
            use diesel::dsl::*;

            self.pending_rts.contains(&tweet.id)
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

        let mut pending = FuturesUnordered::new();

        for outbox in self.manifest().rule.route_tweet(&tweet) {
            debug!("sending a Tweet to outbox {:?}: {:?}", outbox, tweet);

            match *outbox {
                Outbox::Twitter(user) => {
                    pending.push(twitter::statuses::Retweet::new(tweet.id).send(
                        self.manifest().twitter.client.as_ref(),
                        self.core.twitter_token(user).unwrap(),
                        self.http_client(),
                    ));
                }
                _ => unimplemented!(),
            }
        }

        if !pending.is_empty() {
            self.pending_rts.insert(tweet.id);
            self.rt_queue.push(RTQueue {
                tweet: Some(tweet),
                pending,
            });
        }

        Ok(())
    }

    pub async fn shutdown(&mut self) -> Fallible<()> {
        future::poll_fn(|cx| -> Poll<Fallible<()>> {
            match (self.poll_twitter_backfill(cx)?, self.poll_rt_queue(cx)?) {
                (Poll::Ready(()), Poll::Ready(())) => Poll::Ready(Ok(())),
                _ => Poll::Pending,
            }
        })
        .await
    }

    pub async fn reset(&mut self) -> Fallible<()> {
        let twitter_backfill = if self.twitter_done {
            let (bf, t) = self.core.init_twitter().await?;
            self.twitter = t;
            self.twitter_done = false;
            bf
        } else {
            None
        };

        self.shutdown().await?;
        debug_assert!(self.pending_rts.is_empty());

        self.twitter_backfill = twitter_backfill;

        Ok(())
    }

    fn poll_twitter_backfill(&mut self, cx: &mut Context<'_>) -> Poll<Fallible<()>> {
        let backfill = if let Some(ref mut bf) = self.twitter_backfill {
            bf
        } else {
            return Poll::Ready(Ok(()));
        };

        let tweets = match ready!(backfill.response.poll_unpin(cx)) {
            Ok(resp) => resp.response,
            Err(e) => {
                self.twitter_backfill = None;
                return Poll::Ready(Err(e.into()));
            }
        };

        if tweets.is_empty() {
            debug!("timeline backfilling completed");
            self.twitter_backfill = None;
            return Poll::Ready(Ok(()));
        }

        let max_id = tweets.iter().map(|t| t.id).min().map(|id| id - 1);
        // Make borrowck happy
        let since_id = backfill.since_id;
        let list = backfill.list;

        let response = twitter::lists::Statuses::new(list)
            .since_id(Some(since_id))
            .max_id(max_id)
            .send(
                self.manifest().twitter.client.as_ref(),
                self.core
                    .twitter_token(self.manifest().twitter.user)
                    .unwrap(),
                self.http_client(),
            );

        tweets
            .into_iter()
            .filter(|t| t.retweeted_status.is_none())
            .try_for_each(|t| self.process_tweet(t))?;

        self.twitter_backfill = Some(TwitterBackfill {
            list,
            since_id,
            response,
        });

        Poll::Pending
    }

    fn poll_rt_queue(&mut self, cx: &mut Context<'_>) -> Poll<Fallible<()>> {
        use crate::models::NewTweet;
        use crate::schema::tweets::dsl::*;

        let mut retweeted_status = if let Some(s) = ready!(self.rt_queue.poll_next_unpin(cx)?) {
            s
        } else {
            return Poll::Ready(Ok(()));
        };
        let conn = self.conn()?;

        loop {
            diesel::replace_into(tweets)
                .values(&NewTweet::from(&retweeted_status))
                .execute(&*conn)?;
            self.pending_rts.remove(&retweeted_status.id);

            retweeted_status = if let Some(s) = ready!(self.rt_queue.poll_next_unpin(cx)?) {
                s
            } else {
                return Poll::Ready(Ok(()));
            };
        }
    }
}

impl<C> App<C> {
    pub fn manifest(&self) -> &Manifest {
        &self.core.manifest
    }

    pub fn manifest_mut(&mut self) -> &mut Manifest {
        &mut self.core.manifest
    }

    pub fn database_pool(&self) -> &Pool<ConnectionManager<SqliteConnection>> {
        &self.core.pool
    }

    pub fn http_client(&self) -> &Client<C> {
        &self.core.client
    }

    fn conn(&self) -> Fallible<PooledConnection<ConnectionManager<SqliteConnection>>> {
        self.core.conn()
    }
}

impl<C> Future for App<C>
where
    C: Connect + Sync + 'static,
    C::Transport: 'static,
    C::Future: 'static,
{
    type Output = Fallible<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Fallible<()>> {
        trace_fn!(App::<C>::poll);

        let _ = self.poll_twitter_backfill(cx)?;

        while let Poll::Ready(v) = (&mut self.twitter).compat().poll_next_unpin(cx) {
            let result = if let Some(r) = v {
                r
            } else {
                self.twitter_done = true;
                return Poll::Ready(Ok(()));
            };

            let json = result.map_err(|e| {
                self.twitter_done = true;
                e.context("error while listening to Twitter's Streaming API")
            })?;

            let tweet = if let Maybe::Just(t) = json::from_str::<Maybe<twitter::Tweet>>(&json)? {
                t
            } else {
                continue;
            };

            if log_enabled!(log::Level::Trace) {
                let created_at = snowflake_to_system_time(tweet.id as u64);
                match SystemTime::now().duration_since(created_at) {
                    Ok(latency) => debug!("Twitter stream latency: {:.2?}", latency),
                    Err(e) => debug!("Twitter stream latency: -{:.2?}", e.duration()),
                }
            }

            let from = tweet.user.id;
            let will_process = self.manifest().rule.contains_topic(&TopicId::Twitter(from))
                && tweet.in_reply_to_user_id.map_or(true, |to| {
                    self.manifest().rule.contains_topic(&TopicId::Twitter(to))
                });
            if will_process {
                self.process_tweet(tweet)?;
            }
        }

        let _ = self.poll_rt_queue(cx)?;

        Poll::Pending
    }
}

impl<C> Core<C>
where
    C: Connect + Sync + 'static,
    C::Transport: 'static,
    C::Future: 'static,
{
    async fn new(manifest: Manifest, client: Client<C>) -> Fallible<Self> {
        trace_fn!(Core::<C>::new);

        let pool = Pool::new(ConnectionManager::new(manifest.database_url()))
            .context("failed to initialize the connection pool")?;

        let twitter_tokens: FuturesUnordered<_> = manifest
            .rule
            .twitter_outboxes()
            .chain(Some(manifest.twitter.user))
            .unique()
            .map(|user| {
                use crate::schema::twitter_tokens::dsl::*;

                let token = twitter_tokens
                    .find(&user)
                    .get_result::<models::TwitterToken>(&*pool.get()?)
                    .optional()
                    .context("failed to load tokens from the database")?;

                // Make borrowck happy
                let (manifest, client) = (&manifest, &client);
                Ok(async move {
                    if let Some(token) = token {
                        match twitter::account::VerifyCredentials::new()
                            .send(manifest.twitter.client.as_ref(), (&token).into(), client)
                            .await
                        {
                            Ok(_) => return Ok((user, token.into())),
                            Err(twitter::Error::Twitter(ref e))
                                if e.status == StatusCode::UNAUTHORIZED => {}
                            Err(e) => {
                                return Err(e)
                                    .context("error while verifying Twitter credentials")
                                    .map_err(Into::into)
                                    as Fallible<_>;
                            }
                        }
                    }

                    Err(failure::err_msg(
                        "not all Twitter users are authorized; please run `pipitor twitter-login`",
                    ))
                })
            })
            .collect::<Fallible<_>>()?;

        {
            use crate::schema::last_tweet::dsl::*;

            diesel::insert_into(last_tweet)
                .values(&models::NewLastTweet {
                    id: 0,
                    status_id: 0,
                })
                .execute(&*pool.get()?)
                .map(|_| ())
                .or_else(|e| match e {
                    QueryError::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => Ok(()),
                    e => Err(e),
                })?;
        }

        let twitter_tokens = twitter_tokens.try_collect().await?;

        Ok(Core {
            manifest,
            pool,
            client,
            twitter_tokens,
        })
    }

    async fn init_twitter(&self) -> Fallible<(Option<TwitterBackfill>, TwitterStream)> {
        trace_fn!(Core::<C>::init_twitter);

        let token = self.twitter_token(self.manifest.twitter.user).unwrap();

        let stream_token = twitter_stream::Token {
            consumer_key: &*self.manifest.twitter.client.key,
            consumer_secret: &*self.manifest.twitter.client.secret,
            access_key: token.key,
            access_secret: token.secret,
        };

        let mut twitter_topics: Vec<_> = self
            .manifest
            .rule
            .twitter_topics()
            .map(|id| id as u64)
            .collect();
        twitter_topics.sort();
        twitter_topics.dedup();

        // `await` immediately to make sure the later call to `send()` happens after Twitter accepts this request
        let twitter = TwitterStreamBuilder::filter(stream_token)
            .follow(&*twitter_topics)
            .listen_with_client(&self.client)
            .compat()
            .await
            .context("error while connecting to Twitter's Streaming API")?;

        let twitter_backfill = {
            use crate::schema::last_tweet::dsl::*;

            if let Some(list) = self.manifest.twitter.list {
                last_tweet
                    .find(&0)
                    .select(status_id)
                    .first::<i64>(&*self.conn()?)
                    .optional()?
                    .filter(|&n| n > 0)
                    .map(|since_id| {
                        debug!("timeline backfilling enabled");
                        let response = twitter::lists::Statuses::new(list)
                            .since_id(Some(since_id))
                            .send(self.manifest.twitter.client.as_ref(), token, &self.client);
                        TwitterBackfill {
                            list,
                            since_id,
                            response,
                        }
                    })
            } else {
                None
            }
        };

        Ok((twitter_backfill, twitter))
    }

    fn twitter_token(&self, user: i64) -> Option<twitter::Credentials<&str>> {
        self.twitter_tokens
            .get(&user)
            .map(twitter::Credentials::as_ref)
    }
}

impl<C> Core<C> {
    fn conn(&self) -> Fallible<PooledConnection<ConnectionManager<SqliteConnection>>> {
        self.pool
            .get()
            .context("failed to retrieve a connection from the connection pool")
            .map_err(Into::into)
    }
}

impl Future for RTQueue {
    type Output = Result<twitter::Tweet, twitter::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        trace_fn!(RTQueue::poll);

        while let Poll::Ready(v) = self.pending.poll_next_unpin(cx) {
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
                        .expect("polled `RTQueue` after completion")));
                }
            }
        }

        Poll::Pending
    }
}

impl FusedFuture for RTQueue {
    fn is_terminated(&self) -> bool {
        self.tweet.is_none()
    }
}

fn snowflake_to_system_time(id: u64) -> SystemTime {
    // timestamp_ms = (snowflake >> 22) + 1_288_834_974_657
    let snowflake_time_ms = id >> 22;
    let timestamp = Duration::new(
        snowflake_time_ms / 1_000 + 1_288_834_974,
        (snowflake_time_ms as u32 % 1_000 + 657) * 1_000 * 1_000,
    );
    UNIX_EPOCH + timestamp
}
