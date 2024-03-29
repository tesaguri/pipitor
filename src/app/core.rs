use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt::Debug;
use std::task::{Context, Poll};

use anyhow::Context as _;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};
use diesel::result::{DatabaseErrorKind, Error as QueryError};
use diesel::SqliteConnection;
use http_body::Body;
use oauth_credentials::{Credentials, Token};
use pin_project::pin_project;
use tower::ServiceExt;
use twitter_client::traits::HttpService;
use twitter_stream::TwitterStream;

use crate::manifest::{self, Manifest};
use crate::models;
use crate::router::Router;
use crate::schema::*;
use crate::twitter;
use crate::util::{self, http_service};

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
        <S::ResponseBody as Body>::Error: Debug,
    {
        trace_fn!(Core::<S>::new);

        manifest.validate()?;

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
        S::Error: Error + Send + Sync + 'static,
        B: Default + From<Vec<u8>>,
    {
        trace_fn!(Core::<S>::init_twitter);

        let config = if let Some(ref config) = self.manifest.twitter {
            config
        } else {
            return Ok(None);
        };

        if !config.stream {
            return Ok(None);
        }

        let token = self.twitter_access_token(config.user).unwrap();

        let stream_token = Token::new(config.client.as_ref(), token);

        let mut twitter_topics: Vec<_> =
            self.manifest.twitter_topics().map(|id| id as u64).collect();
        twitter_topics.sort_unstable();
        twitter_topics.dedup();

        let mut client = http_service::IntoService(self.client.clone());
        client
            .ready()
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
        S::Error: Debug,
        S::Future: Send + 'static,
        S::ResponseBody: Send,
        <S::ResponseBody as Body>::Error: Debug,
        B: Default + From<Vec<u8>> + Send + 'static,
    {
        let (user, client, list) = match self.manifest.twitter {
            Some(manifest::Twitter {
                user,
                ref client,
                list: Some(ref list),
                ..
            }) => (user, client.clone(), list),
            _ => return Ok(twitter::ListTimeline::empty()),
        };

        let since_id = last_tweet::table
            .find(&0)
            .select(last_tweet::status_id)
            .first::<i64>(&*self.conn()?)
            .optional()?
            .filter(|&n| n > 0);

        let token = self.twitter_tokens.get(&user).unwrap().clone();
        let token = Token::new(client, token);

        let http = self.client.clone();

        Ok(twitter::ListTimeline::new(list, since_id, token, http))
    }

    pub fn load_twitter_tokens(&mut self) -> anyhow::Result<()> {
        let config = if let Some(ref config) = self.manifest().twitter {
            config
        } else {
            return Ok(());
        };

        let unauthed_users = self
            .manifest()
            .twitter_outboxes()
            .chain(Some(config.user))
            .filter(|user| !self.twitter_tokens.contains_key(user))
            // Make the values unique so that the later `_.len() != _.len()` comparison makes sense.
            .collect::<HashSet<_>>();

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

    pub fn gc_twitter_tokens(&mut self) {
        let config = if let Some(ref config) = self.manifest().twitter {
            config
        } else {
            self.twitter_tokens = HashMap::new();
            return;
        };

        let users = self
            .manifest()
            .twitter_outboxes()
            .chain(Some(config.user))
            .collect::<HashSet<_>>();

        // TODO: Use `HashMap::drain_filter` once it hits stable.
        // <https://github.com/rust-lang/rust/issues/59618>
        self.twitter_tokens = self
            .twitter_tokens
            .drain()
            .filter(|(k, _)| users.contains(k))
            .collect();

        self.twitter_tokens.shrink_to_fit();
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

    pub fn twitter_access_token(&self, user: i64) -> Option<Credentials<&str>> {
        self.twitter_tokens.get(&user).map(Credentials::as_ref)
    }

    pub fn twitter_token(&self, user: Option<i64>) -> Option<Token<&str>> {
        self.manifest.twitter.as_ref().and_then(|config| {
            self.twitter_access_token(user.unwrap_or(config.user))
                .map(|token| Token::new(config.client.as_ref(), token))
        })
    }
}
