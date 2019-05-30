use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};

use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};
use diesel::result::{DatabaseErrorKind, Error as QueryError};
use diesel::SqliteConnection;
use failure::{Fail, Fallible, ResultExt};
use futures::compat::Future01CompatExt;
use futures::stream::{FuturesUnordered, TryStreamExt};
use hyper::client::connect::Connect;
use hyper::Client;
use hyper::StatusCode;
use itertools::Itertools;
use twitter_stream::{TwitterStream, TwitterStreamBuilder};

use crate::models;
use crate::twitter::{self, Request as _};
use crate::Manifest;

use super::TwitterListTimeline;

/// An object referenced by `poll`-like methods under `app` module.
pub struct Core<C> {
    manifest: Manifest,
    pool: Pool<ConnectionManager<SqliteConnection>>,
    client: Client<C>,
    twitter_tokens: HashMap<i64, twitter::Credentials<Box<str>>>,
    twitter_dump: Option<BufWriter<File>>,
}

impl<C> Core<C>
where
    C: Connect + Sync + 'static,
    C::Transport: 'static,
    C::Future: 'static,
{
    pub async fn new(manifest: Manifest, client: Client<C>) -> Fallible<Self> {
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
            twitter_dump: None,
        })
    }

    pub async fn init_twitter(&self) -> Fallible<TwitterStream> {
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

        let twitter = TwitterStreamBuilder::filter(stream_token)
            .follow(&*twitter_topics)
            .listen_with_client(&self.client)
            .compat()
            .await
            .context("error while connecting to Twitter's Streaming API")?;

        Ok(twitter)
    }

    pub(super) fn init_twitter_list(&self) -> Fallible<TwitterListTimeline> {
        use crate::schema::last_tweet::dsl::*;

        let list = if let Some(list) = self.manifest.twitter.list {
            list
        } else {
            return Ok(TwitterListTimeline::empty());
        };

        let since_id = last_tweet
            .find(&0)
            .select(status_id)
            .first::<i64>(&*self.conn()?)
            .optional()?
            .filter(|&n| n > 0);

        Ok(TwitterListTimeline::new(list, since_id, self))
    }
}

impl<C> Core<C> {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn manifest_mut(&mut self) -> &mut Manifest {
        &mut self.manifest
    }

    pub fn database_pool(&self) -> &Pool<ConnectionManager<SqliteConnection>> {
        &self.pool
    }

    pub fn http_client(&self) -> &Client<C> {
        &self.client
    }

    pub fn conn(&self) -> Fallible<PooledConnection<ConnectionManager<SqliteConnection>>> {
        self.pool
            .get()
            .context("failed to retrieve a connection from the connection pool")
            .map_err(Into::into)
    }

    pub fn twitter_token(&self, user: i64) -> Option<twitter::Credentials<&str>> {
        self.twitter_tokens
            .get(&user)
            .map(twitter::Credentials::as_ref)
    }

    pub fn with_twitter_dump<F, E>(&mut self, f: F) -> Fallible<()>
    where
        F: FnOnce(&mut BufWriter<File>) -> Result<(), E>,
        E: Fail,
    {
        if let Some(ref mut dump) = self.twitter_dump {
            f(dump).map_err(|e| {
                self.twitter_dump = None;
                e.context("failed to write a Tweet to the dump file")
            })?;
        }
        Ok(())
    }

    pub fn set_twitter_dump(&mut self, twitter_dump: File) -> io::Result<()> {
        if let Some(mut old) = self.twitter_dump.replace(BufWriter::new(twitter_dump)) {
            old.flush()?;
        }
        Ok(())
    }
}
