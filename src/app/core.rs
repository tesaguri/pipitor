use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::pin::Pin;

use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};
use diesel::result::{DatabaseErrorKind, Error as QueryError};
use diesel::SqliteConnection;
use failure::{Fail, Fallible, ResultExt};
use futures::{Future, FutureExt};
use hyper::client::connect::Connect;
use hyper::{Body, Client};
use pin_project::pin_project;
use twitter_stream::TwitterStream;

use crate::models;
use crate::util::open_credentials;
use crate::{Credentials, Manifest};

use super::TwitterListTimeline;

/// An object referenced by `poll`-like methods under `app` module.
#[pin_project]
pub struct Core<C> {
    manifest: Manifest,
    credentials: Credentials,
    pool: Pool<ConnectionManager<SqliteConnection>>,
    #[pin]
    client: Client<C>,
    pub(super) twitter_tokens: HashMap<i64, oauth1::Credentials<Box<str>>>,
    twitter_dump: Option<BufWriter<File>>,
}

impl<C> Core<C>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    pub fn new(manifest: Manifest, client: Client<C>) -> Fallible<Self> {
        trace_fn!(Core::<C>::new);

        let pool = Pool::new(ConnectionManager::new(manifest.database_url()))
            .context("failed to initialize the connection pool")?;
        let credentials: Credentials = open_credentials(manifest.credentials_path())?;

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

        let mut ret = Core {
            manifest,
            credentials,
            pool,
            client,
            twitter_tokens: HashMap::new(),
            twitter_dump: None,
        };

        ret.load_twitter_tokens()?;

        Ok(ret)
    }

    pub fn init_twitter(&self) -> impl Future<Output = Fallible<TwitterStream<Body>>> {
        trace_fn!(Core::<C>::init_twitter);

        let token = self.twitter_token(self.manifest.twitter.user).unwrap();

        let stream_token = twitter_stream::Token::from_credentials(
            self.credentials().twitter.client.as_ref(),
            token.as_ref(),
        );

        let mut twitter_topics: Vec<_> = self
            .manifest
            .rule
            .twitter_topics()
            .map(|id| id as u64)
            .collect();
        twitter_topics.sort();
        twitter_topics.dedup();

        twitter_stream::Builder::filter(stream_token)
            .follow(&*twitter_topics)
            .listen_with_client(self.client.clone())
            .map(|result| Ok(result.context("error while connecting to Twitter's Streaming API")?))
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

    pub fn load_twitter_tokens(&mut self) -> Fallible<()> {
        let manifest = self.manifest();

        let unauthed_users = manifest
            .rule
            .twitter_outboxes()
            .chain(Some(manifest.twitter.user))
            .filter(|user| !self.twitter_tokens.contains_key(&user))
            // Make the values unique so that the later `_.len() != _.len()` comparison makes sense.
            .collect::<HashSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        let tokens: Vec<models::TwitterToken> = {
            use crate::schema::twitter_tokens::dsl::*;

            twitter_tokens
                .filter(id.eq_any(&unauthed_users))
                .load(&*self.conn()?)?
        };

        if unauthed_users.len() != tokens.len() {
            return Err(failure::err_msg(
                "not all Twitter users are authorized; please run `pipitor twitter-login`",
            ));
        }

        self.twitter_tokens
            .extend(tokens.into_iter().map(|token| (token.id, token.into())));

        Ok(())
    }
}

impl<C> Core<C> {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    pub fn credentials_mut(&mut self) -> &mut Credentials {
        &mut self.credentials
    }

    pub fn manifest_mut(&mut self) -> &mut Manifest {
        &mut self.manifest
    }

    pub fn database_pool(&self) -> &Pool<ConnectionManager<SqliteConnection>> {
        &self.pool
    }

    pub fn database_pool_mut(&mut self) -> &mut Pool<ConnectionManager<SqliteConnection>> {
        &mut self.pool
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

    pub fn twitter_token(&self, user: i64) -> Option<oauth1::Credentials<&str>> {
        self.twitter_tokens
            .get(&user)
            .map(oauth1::Credentials::as_ref)
    }

    pub fn with_twitter_dump<F, E>(self: Pin<&mut Self>, f: F) -> Fallible<()>
    where
        F: FnOnce(&mut BufWriter<File>) -> Result<(), E>,
        E: Fail,
    {
        let twitter_dump = self.project().twitter_dump;

        if let Some(ref mut dump) = twitter_dump {
            f(dump).map_err(|e| {
                *twitter_dump = None;
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
