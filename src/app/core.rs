use std::collections::{HashMap, HashSet};
use std::task::{Context, Poll};

use anyhow::Context as _;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool, PooledConnection};
use diesel::result::{DatabaseErrorKind, Error as QueryError};
use diesel::SqliteConnection;
use http_body::Body;
use pin_project::pin_project;

use crate::models;
use crate::router::Router;
use crate::schema::*;
use crate::util::{self, open_credentials, HttpService};
use crate::{Credentials, Manifest};

use super::shutdown::Shutdown;

/// An object referenced by `poll`-like methods under `app` module.
#[pin_project]
pub struct Core<S> {
    manifest: Manifest,
    credentials: Credentials,
    router: Router,
    pool: Pool<ConnectionManager<SqliteConnection>>,
    #[pin]
    client: S,
    twitter_tokens: HashMap<i64, oauth_credentials::Credentials<Box<str>>>,
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
        let credentials: Credentials = open_credentials(manifest.credentials_path())?;

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
            credentials,
            pool,
            client,
            twitter_tokens: HashMap::new(),
            shutdown: Shutdown::default(),
        };

        ret.load_twitter_tokens()?;

        Ok(ret)
    }

    pub fn load_twitter_tokens(&mut self) -> anyhow::Result<()> {
        let manifest = self.manifest();

        let unauthed_users = manifest
            .twitter_outboxes()
            .chain(Some(manifest.twitter.user))
            .filter(|user| !self.twitter_tokens.contains_key(user))
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
