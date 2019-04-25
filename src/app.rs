use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::task::Context;

use diesel::prelude::*;
use diesel::SqliteConnection;
use failure::{Fallible, ResultExt};
use futures::compat::Stream01CompatExt;
use futures::future::{FusedFuture, Future, FutureExt};
use futures::stream::{FuturesUnordered, StreamExt, TryStreamExt};
use futures::Poll;
use hyper::client::connect::Connect;
use hyper::Client;
use hyper::StatusCode;
use itertools::Itertools;
use r2d2::Pool;
use r2d2_diesel::ConnectionManager;
use serde::de;
use twitter_stream::{TwitterStream, TwitterStreamBuilder};

use crate::models;
use crate::rules::Outbox;
use crate::twitter::{self, Request as _};
use crate::util::Maybe;
use crate::Manifest;

pub struct App<C> {
    core: Core<C>,
    twitter_backfill: Option<TwitterBackfill>,
    twitter: TwitterStream,
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
        await!(Self::with_http_client(client, manifest))
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

        let core = await!(Core::new(manifest, client))?;
        let (twitter_backfill, twitter) = await!(core.init_twitter())?;

        Ok(App {
            core,
            twitter_backfill,
            twitter,
            rt_queue: FuturesUnordered::new(),
            pending_rts: HashSet::new(),
        })
    }

    pub fn process_tweet(&mut self, tweet: twitter::Tweet) -> Fallible<()> {
        trace_fn!(App::<C>::process_tweet, "tweet={:?}", tweet);

        use crate::schema::tweets::dsl::*;
        use diesel::dsl::*;

        let already_processed: bool =
            select(exists(tweets.find(&tweet.id))).get_result(&*self.database_pool().get()?)?;

        if already_processed || self.pending_rts.contains(&tweet.id) {
            trace!("the Tweet has already been processed");
            return Ok(());
        }

        let mut pending = FuturesUnordered::new();

        for outbox in self.manifest().rule.route_tweet(&tweet) {
            debug!("sending a Tweet to outbox {:?}", outbox);

            match *outbox {
                Outbox::Twitter(user) => {
                    pending.push(twitter::statuses::Retweet::new(tweet.id).send(
                        self.manifest().twitter.client.as_ref(),
                        self.core.twitter_token(user).unwrap(),
                        &self.http_client(),
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

    pub async fn reset(&mut self) -> Fallible<()> {
        let (twitter_backfill, twitter) = await!(self.core.init_twitter())?;
        self.twitter_backfill = twitter_backfill;
        self.twitter = twitter;
        self.rt_queue = FuturesUnordered::new();
        self.pending_rts.clear();
        Ok(())
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
}

impl<C> Future for App<C>
where
    C: Connect + Sync + 'static,
    C::Transport: 'static,
    C::Future: 'static,
{
    type Output = Fallible<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Fallible<()>> {
        trace_fn!(App::<C>::poll);

        if let Some(ref mut backfill) = self.twitter_backfill {
            if let Poll::Ready(tweets) = backfill.response.poll_unpin(cx)? {
                if tweets.is_empty() {
                    trace!("timeline backfilling completed");
                    self.twitter_backfill = None;
                } else {
                    let max_id = tweets.iter().map(|t| t.id).min().map(|id| id - 1);
                    // Make borrowck happy
                    let since_id = backfill.since_id;
                    let list = backfill.list;

                    tweets
                        .response
                        .into_iter()
                        .filter(|t| t.retweeted_status.is_none())
                        .try_for_each(|t| self.process_tweet(t))?;

                    let response = twitter::lists::Statuses::new(list)
                        .since_id(Some(since_id))
                        .max_id(max_id)
                        .send(
                            self.manifest().twitter.client.as_ref(),
                            self.core
                                .twitter_token(self.manifest().twitter.user)
                                .unwrap(),
                            &self.http_client(),
                        );

                    self.twitter_backfill = Some(TwitterBackfill {
                        list,
                        since_id,
                        response,
                    });
                }
            }
        }

        while let Poll::Ready(v) = (&mut self.twitter).compat().poll_next_unpin(cx) {
            if let Some(result) = v {
                let json = result.context("error while listening to Twitter's Streaming API");
                if let Maybe::Just(tweet) = json::from_str(&json?)? {
                    self.process_tweet(tweet)?;
                }
            } else {
                return Poll::Ready(Ok(()));
            }
        }

        let mut poll = self.rt_queue.poll_next_unpin(cx);
        if let Poll::Ready(Some(_)) = poll {
            let conn = self.database_pool().get()?;
            while let Poll::Ready(Some(result)) = poll {
                use crate::models::NewTweet;
                use crate::schema::tweets::dsl::*;

                let retweeted_status = result.context("failed to Retweet a Tweet")?;
                diesel::replace_into(tweets)
                    .values(&NewTweet::from(&retweeted_status))
                    .execute(&*conn)?;
                self.pending_rts.remove(&retweeted_status.id);

                poll = self.rt_queue.poll_next_unpin(cx);
            }
        }

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

        let pool = Pool::new(ConnectionManager::new(manifest.database_url()))?;

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
                        match await!(twitter::account::VerifyCredentials::new().send(
                            manifest.twitter.client.as_ref(),
                            (&token).into(),
                            client,
                        )) {
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

        let twitter_tokens = await!(twitter_tokens.try_collect())?;

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

        // `await!` immediately to make sure the later call to `send()` happens after Twitter accepts this request
        let twitter = await!(TwitterStreamBuilder::filter(stream_token)
            .follow(&*twitter_topics)
            .listen_with_client(&self.client))
        .context("error while connecting to Twitter's Streaming API")?;

        let twitter_backfill = {
            use crate::schema::tweets::dsl::*;
            use diesel::dsl::*;

            if let Some(list) = self.manifest.twitter.list {
                tweets
                    .select(max(id))
                    .first::<Option<i64>>(&*self.pool.get()?)?
                    .map(|since_id| {
                        trace!("timeline backfilling enabled");
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

impl Future for RTQueue {
    type Output = Result<twitter::Tweet, twitter::Error>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        trace_fn!(RTQueue::poll);

        while let Poll::Ready(v) = self.pending.poll_next_unpin(cx) {
            if let Some(result) = v {
                if let Err(e) = result {
                    if let twitter::Error::Twitter(ref e) = e {
                        if e.codes().any(|c| {
                            c == twitter::ErrorCode::YOU_HAVE_ALREADY_RETWEETED_THIS_TWEET
                                || c == twitter::ErrorCode::NO_STATUS_FOUND_WITH_THAT_ID
                        }) {
                            continue;
                        }
                    }
                    return Poll::Ready(Err(e));
                }
            } else {
                return Poll::Ready(Ok(self
                    .tweet
                    .take()
                    .expect("polled `RTQueue` after completion")));
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
