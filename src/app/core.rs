use std::collections::{HashMap, HashSet};
use std::task::{Context, Poll};

use anyhow::Context as _;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};
use diesel::result::{DatabaseErrorKind, Error as QueryError};
use diesel::SqliteConnection;
use http_body::Body;
use oauth_credentials::{Credentials, Token};
use pin_project::pin_project;
use tower_util::ServiceExt;
use twitter_stream::TwitterStream;

use crate::models;
use crate::router::Router;
use crate::schema::*;
use crate::twitter;
use crate::util::{self, HttpService};
use crate::Manifest;

use super::shutdown::Shutdown;

/// An object referenced by `poll`-like methods under `app` module.
#[pin_project]
pub struct Core<S> {
    manifest: Manifest,
    router: Router,
    pool: Pool<ConnectionManager<SqliteConnection>>,
    #[pin]
    client: S,
    twitter_tokens: HashMap<i64, Credentials<Box<str>>>,
    shutdown: Shutdown,
}

impl<S> Core<S> {
    pub fn new<B>(manifest: Manifest, client: S) -> anyhow::Result<Self>
    where
        S: HttpService<B> + Clone,
        <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    {
        trace_fn!(Core::<S>::new);

        let manager = ConnectionManager::new(manifest.database_url());
        let pool =
            util::r2d2::new_pool(manager).context("failed to initialize the connection pool")?;

        diesel::insert_into(last_tweet::table)
            .values((last_tweet::id.eq(0), last_tweet::status_id.eq(0)))
            .execute(&*pool.get()?)
            .map(|_| ())
            .or_else(|e| match e {
                QueryError::DatabaseError(DatabaseErrorKind::UniqueViolation, _) => Ok(()),
                e => Err(e),
            })?;

        let mut ret = Core {
            router: Router::from_manifest(&manifest),
            manifest,
            pool,
            client,
            twitter_tokens: HashMap::new(),
            shutdown: Shutdown::default(),
        };

        ret.load_twitter_tokens()?;

        Ok(ret)
    }

    pub async fn init_twitter<B>(&self) -> anyhow::Result<Option<TwitterStream<S::ResponseBody>>>
    where
        S: HttpService<B> + Clone,
        <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
        B: Default + From<Vec<u8>>,
    {
        trace_fn!(Core::<S>::init_twitter);

        if !self.manifest.twitter.stream {
            return Ok(None);
        }

        let token = self.twitter_token(self.manifest.twitter.user).unwrap();

        let stream_token = Token::new(self.manifest.twitter.client.as_ref(), token);

        let mut twitter_topics: Vec<_> =
            self.manifest.twitter_topics().map(|id| id as u64).collect();
        twitter_topics.sort_unstable();
        twitter_topics.dedup();

        let mut client = self.client.clone().into_service();
        client
            .ready_and()
            .await
            .context("error while connecting to Twitter's Streaming API")?;
        let twitter = twitter_stream::Builder::new(stream_token)
            .follow(&*twitter_topics)
            .listen_with_client(client)
            .await
            .context("error while connecting to Twitter's Streaming API")?;
        Ok(Some(twitter))
    }

    pub(super) fn init_twitter_list<B>(&self) -> anyhow::Result<twitter::ListTimeline<S, B>>
    where
        S: HttpService<B> + Clone + Send + Sync + 'static,
        S::Future: Send + 'static,
        S::ResponseBody: Send,
        <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
        B: Default + From<Vec<u8>> + Send + 'static,
    {
        let list = if let Some(ref list) = self.manifest.twitter.list {
            list
        } else {
            return Ok(twitter::ListTimeline::empty());
        };

        let since_id = last_tweet::table
            .find(&0)
            .select(last_tweet::status_id)
            .first::<i64>(&*self.conn()?)
            .optional()?
            .filter(|&n| n > 0);

        let user = self.manifest().twitter.user;
        let client = self.manifest().twitter.client.clone();
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

        let tokens: Vec<models::TwitterToken> = twitter_tokens::table
            .filter(twitter_tokens::id.eq_any(&unauthed_users))
            .load(&*self.conn()?)?;

        anyhow::ensure!(
            unauthed_users.len() == tokens.len(),
            "not all Twitter users are authorized; please run `pipitor twitter-login`",
        );

        self.twitter_tokens
            .extend(tokens.into_iter().map(|token| (token.id, token.into())));

        Ok(())
    }

    pub fn poll_shutdown(&self, cx: &mut Context<'_>) -> Poll<()> {
        self.shutdown.poll(cx)
    }

    pub fn shutdown_handle(&self) -> super::shutdown::Handle {
        self.shutdown.handle().clone()
    }
}

impl<S> Core<S> {
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
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

    pub fn twitter_token(&self, user: i64) -> Option<Credentials<&str>> {
        self.twitter_tokens.get(&user).map(Credentials::as_ref)
    }
}
