use std::collections::{HashMap, HashSet};
use std::pin::Pin;
use std::task::Context;

use diesel::prelude::*;
use diesel::SqliteConnection;
use failure::{Fallible, ResultExt};
use futures::compat::Stream01CompatExt;
use futures::future::{self, FusedFuture, Future, FutureExt};
use futures::stream::{FuturesUnordered, StreamExt, TryStreamExt};
use futures::Poll;
use hyper::client::{Client, HttpConnector};
use hyper::StatusCode;
use hyper_tls::HttpsConnector;
use itertools::Itertools;
use r2d2::Pool;
use r2d2_diesel::ConnectionManager;
use serde::de;
use tokio::codec::FramedRead;
use twitter_stream::{TwitterStream, TwitterStreamBuilder};

use crate::models;
use crate::rules::{Outbox, TopicId};
use crate::twitter::{self, Request as _};
use crate::Manifest;

pub struct App {
    core: Core,
    twitter_backfill: Option<TwitterBackfill>,
    twitter: TwitterStream,
    rt_queue: FuturesUnordered<RTQueue>,
    pending_rts: HashSet<i64>,
}

struct Core {
    manifest: Manifest,
    pool: Pool<ConnectionManager<SqliteConnection>>,
    client: Client<HttpsConnector<HttpConnector>>,
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

impl App {
    pub async fn new(manifest: Manifest) -> Fallible<Self> {
        trace_fn!(App::new);

        let core = await!(Core::authenticate(manifest))?;
        let (twitter_backfill, twitter) = await!(core.init_twitter())?;

        Ok(App {
            core,
            twitter_backfill,
            twitter,
            rt_queue: FuturesUnordered::new(),
            pending_rts: HashSet::new(),
        })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.core.manifest
    }

    pub fn manifest_mut(&mut self) -> &mut Manifest {
        &mut self.core.manifest
    }

    pub fn database_pool(&self) -> &Pool<ConnectionManager<SqliteConnection>> {
        &self.core.pool
    }

    pub fn http_client(&self) -> &Client<HttpsConnector<HttpConnector>> {
        &self.core.client
    }

    pub async fn reset(&mut self) -> Fallible<()> {
        let (twitter_backfill, twitter) = await!(self.core.init_twitter())?;
        self.twitter_backfill = twitter_backfill;
        self.twitter = twitter;
        self.rt_queue = FuturesUnordered::new();
        self.pending_rts.clear();
        Ok(())
    }

    pub fn process_tweet(&mut self, tweet: twitter::Tweet) -> Fallible<()> {
        trace_fn!(App::process_tweet, "tweet={:?}", tweet);

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

            match outbox {
                &Outbox::Twitter(user) => {
                    pending.push(twitter::statuses::Retweet::new(tweet.id).send(
                        self.manifest().twitter.client.as_ref(),
                        self.core.twitter_token(user).unwrap(),
                        &self.http_client(),
                    ));
                }
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
}

impl Future for App {
    type Output = Fallible<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Fallible<()>> {
        trace_fn!(App::poll);

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
                let tweet = json::from_str(&json?)?;
                self.process_tweet(tweet)?;
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

impl Core {
    async fn authenticate(manifest: Manifest) -> Fallible<Self> {
        trace_fn!(Core::authenticate);

        use crate::schema::twitter_tokens::dsl::*;

        let pool = Pool::new(ConnectionManager::new(manifest.database_url()))?;
        let client = Client::builder()
            .build(HttpsConnector::new(4).context("failed to initialize TLS client")?);

        enum TwitterUser {
            Authed(i64, twitter::Credentials<Box<str>>),
            Unauthed(i64),
        }

        let twitter_users: FuturesUnordered<_> = manifest
            .rule
            .outboxes()
            .filter_map(|outbox| match outbox {
                &Outbox::Twitter(user) => Some(user),
            })
            .chain(Some(manifest.twitter.user))
            .unique()
            .map(|user| {
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
                            &client,
                        )) {
                            Ok(_) => return Ok(TwitterUser::Authed(user, token.into())),
                            Err(twitter::Error::StatusCode(StatusCode::UNAUTHORIZED)) => (),
                            Err(e) => {
                                return Err(e)
                                    .context("error while verifying Twitter credentials")
                                    .map_err(Into::into)
                                    as Fallible<_>;
                            }
                        }
                    }

                    Ok(TwitterUser::Unauthed(user))
                })
            })
            .collect::<Fallible<_>>()?;

        let (mut tokens, mut unauthed_users) = await!(twitter_users.try_fold(
            (HashMap::new(), HashSet::new()),
            |(mut authed, mut unauthed), user| {
                match user {
                    TwitterUser::Authed(user, token) => {
                        trace!("authorized user: {}", user);
                        authed.insert(user, token);
                    }
                    TwitterUser::Unauthed(user) => {
                        unauthed.insert(user);
                    }
                }
                future::ok((authed, unauthed))
            }
        ))?;

        let mut stdin = FramedRead::new(
            tokio_file_unix::File::new_nb(tokio_file_unix::raw_stdin()?)?
                .into_io(&tokio::reactor::Handle::default())?,
            tokio_file_unix::DelimCodec(tokio_file_unix::Newline),
        )
        .compat();

        while !unauthed_users.is_empty() {
            let temporary = await!(twitter::oauth::request_token(
                manifest.twitter.client.as_ref(),
                &client,
            ))
            .context("error while getting OAuth request token from Twitter")?;

            let verifier = loop {
                println!();
                println!("Open the following URL in a Web browser:");
                println!(
                    "https://api.twitter.com/oauth/authorize?force_login=true&oauth_token={}",
                    temporary.key,
                );
                println!("..., log into an account with one of the following `user_id`s:");
                for user in &unauthed_users {
                    println!("{}", user);
                }
                println!("... and enter the PIN code given by Twitter:");
                print!(">");

                if let Some(input) = await!(stdin.by_ref().next()) {
                    break String::from_utf8(input?)?;
                }
            };

            let (user, token) = await!(twitter::oauth::access_token(
                &verifier,
                manifest.twitter.client.as_ref(),
                temporary.as_ref(),
                &client,
            ))
            .context("error while getting OAuth access token from Twitter")?;

            if unauthed_users.remove(&user) {
                diesel::replace_into(twitter_tokens)
                    .values(models::NewTwitterTokens {
                        id: user,
                        access_token: &token.key,
                        access_token_secret: &token.secret,
                    })
                    .execute(&*pool.get()?)?;
                tokens.insert(user, token.map(Into::into));
                println!("Successfully logged in as user_id={}", user);
            } else {
                println!("Invalid user, try again");
                // TODO: invalidate token
            }
        }

        Ok(Core {
            manifest,
            pool,
            client,
            twitter_tokens: tokens,
        })
    }

    async fn init_twitter(&self) -> Fallible<(Option<TwitterBackfill>, TwitterStream)> {
        trace_fn!(Core::init_twitter);

        let token = {
            use crate::schema::twitter_tokens::dsl::*;
            twitter_tokens
                .find(&self.manifest.twitter.user)
                .get_result::<models::TwitterToken>(&*self.pool.get()?)
                .context("failed to load token from the database")?
        };

        let stream_token = twitter_stream::Token {
            consumer_key: &*self.manifest.twitter.client.key,
            consumer_secret: &*self.manifest.twitter.client.secret,
            access_key: &*token.access_token,
            access_secret: &*token.access_token_secret,
        };

        let mut twitter_topics: Vec<_> = self
            .manifest
            .rule
            .topics()
            .filter_map(|topic| match topic {
                &TopicId::Twitter(user) => Some(user as u64),
                _ => None,
            })
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
                        TwitterBackfill {
                            list,
                            since_id,
                            response: twitter::lists::Statuses::new(list)
                                .since_id(Some(since_id))
                                .send(
                                    self.manifest.twitter.client.as_ref(),
                                    (&token).into(),
                                    &self.client,
                                ),
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
                result?;
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
