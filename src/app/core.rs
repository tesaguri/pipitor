use std::collections::{HashMap, HashSet};

use anyhow::Context;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};
use diesel::result::{DatabaseErrorKind, Error as QueryError};
use diesel::SqliteConnection;
use futures::{future, Future, FutureExt, TryFutureExt};
use http_body::Body;
use pin_project::pin_project;
use twitter_stream::TwitterStream;

use crate::models;
use crate::router::Router;
use crate::twitter;
use crate::util::{open_credentials, HttpService};
use crate::{Credentials, Manifest};

/// An object referenced by `poll`-like methods under `app` module.
#[pin_project]
pub struct Core<S> {
    manifest: Manifest,
    credentials: Credentials,
    router: Router,
    pool: Pool<ConnectionManager<SqliteConnection>>,
    #[pin]
    client: S,
    pub(super) twitter_tokens: HashMap<i64, oauth_credentials::Credentials<Box<str>>>,
}

impl<S> Core<S> {
    pub fn new<B>(manifest: Manifest, client: S) -> anyhow::Result<Self>
    where
        S: HttpService<B> + Clone,
        <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    {
        trace_fn!(Core::<S>::new);

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
            router: Router::from_manifest(&manifest),
            manifest,
            credentials,
            pool,
            client,
            twitter_tokens: HashMap::new(),
        };

        ret.load_twitter_tokens()?;

        Ok(ret)
    }

    pub fn init_twitter<'a, B>(
        &'a self,
    ) -> impl Future<Output = anyhow::Result<TwitterStream<S::ResponseBody>>> + 'a
    where
        S: HttpService<B> + Clone,
        <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
        S::Future: 'a,
        B: Default + From<Vec<u8>>,
    {
        trace_fn!(Core::<S>::init_twitter);

        let token = self.twitter_token(self.manifest.twitter.user).unwrap();

        let stream_token =
            oauth_credentials::Token::new(self.credentials().twitter.client.as_ref(), token);

        let mut twitter_topics: Vec<_> =
            self.manifest.twitter_topics().map(|id| id as u64).collect();
        twitter_topics.sort();
        twitter_topics.dedup();

        let mut client = Some(self.client.clone().into_service());
        future::poll_fn(move |cx| {
            client.as_mut().unwrap().poll_ready(cx).map(|result| {
                result.context("error while connecting to Twitter's Streaming API")?;
                Ok(client.take().unwrap())
            })
        })
        .and_then(move |client| {
            twitter_stream::Builder::new(stream_token)
                .follow(&*twitter_topics)
                .listen_with_client(client)
                .map(|result| result.context("error while connecting to Twitter's Streaming API"))
        })
    }

    pub(super) fn init_twitter_list<B>(&self) -> anyhow::Result<twitter::ListTimeline<S, B>>
    where
        S: HttpService<B> + Clone + Send + Sync + 'static,
        S::Future: Send + 'static,
        S::ResponseBody: Send,
        <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
        B: Default + From<Vec<u8>> + Send + 'static,
    {
        use crate::schema::last_tweet::dsl::*;

        let list = if let Some(list) = self.manifest.twitter.list.clone() {
            list
        } else {
            return Ok(twitter::ListTimeline::empty());
        };

        let since_id = last_tweet
            .find(&0)
            .select(status_id)
            .first::<i64>(&*self.conn()?)
            .optional()?
            .filter(|&n| n > 0);

        let user = self.manifest().twitter.user;
        let client = self.credentials().twitter.client.clone();
        let token = self.twitter_tokens.get(&user).unwrap().clone();

        let http = self.client.clone();

        Ok(twitter::ListTimeline::new(
            list, since_id, client, token, http,
        ))
    }

    pub fn load_twitter_tokens(&mut self) -> anyhow::Result<()> {
        let manifest = self.manifest();

        let unauthed_users = manifest
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

        anyhow::ensure!(
            unauthed_users.len() == tokens.len(),
            "not all Twitter users are authorized; please run `pipitor twitter-login`",
        );

        self.twitter_tokens
            .extend(tokens.into_iter().map(|token| (token.id, token.into())));

        Ok(())
    }
}

impl<S> Core<S> {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    pub fn credentials_mut(&mut self) -> &mut Credentials {
        &mut self.credentials
    }

    pub fn router(&self) -> &Router {
        &self.router
    }

    pub fn router_mut(&mut self) -> &mut Router {
        &mut self.router
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

    pub fn http_client(&self) -> &S {
        &self.client
    }

    pub fn conn(&self) -> anyhow::Result<PooledConnection<ConnectionManager<SqliteConnection>>> {
        self.pool
            .get()
            .context("failed to retrieve a connection from the connection pool")
            .map_err(Into::into)
    }

    pub fn twitter_token(&self, user: i64) -> Option<oauth_credentials::Credentials<&str>> {
        self.twitter_tokens
            .get(&user)
            .map(oauth_credentials::Credentials::as_ref)
    }
}
