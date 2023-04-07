mod core;
mod sender;
mod shutdown;
mod twitter_request_ext;

use std::collections::HashSet;
use std::convert::TryInto;
use std::error::Error;
use std::mem;
use std::pin::Pin;
use std::str;
use std::task::{Context, Poll};

use anyhow::Context as _;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::SqliteConnection;
use futures::{future, Future, FutureExt, Stream, TryStream};
use http_body::Body;
use listenfd::ListenFd;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::credentials::Credentials;
use crate::manifest::Manifest;
use crate::router::Router;
use crate::schema::*;
use crate::socket;

use crate::util::{self, open_credentials, HttpService};
use crate::websub;

use self::core::Core;
use self::sender::Sender;
use self::twitter_request_ext::TwitterRequestExt;

#[pin_project]
pub struct App<S, B, I = socket::Listener>
where
    S: HttpService<B>,
{
    #[pin]
    core: Core<S>,
    #[pin]
    websub: Option<websub::Subscriber<S, B, I>>,
    sender: Sender<S, B>,
}

#[cfg(feature = "native-tls")]
impl<I> App<hyper::Client<hyper_tls::HttpsConnector<hyper::client::HttpConnector>>, hyper::Body, I>
where
    I: TryStream + socket::Bind<socket::Addr>,
    I::Ok: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    <I as TryStream>::Error: Error + Send + Sync + 'static,
    <I as socket::Bind<socket::Addr>>::Error: Error + Send + Sync + 'static,
    std::net::TcpListener: TryInto<I>,
    <std::net::TcpListener as TryInto<I>>::Error: Error + Send + Sync + 'static,
{
    // XXX: Switching trait boundary with `cfg` seems to be a suboptimal solution...
    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            pub fn new(manifest: Manifest) -> impl Future<Output = anyhow::Result<Self>>
            where
                std::os::unix::net::UnixListener: TryInto<I>,
                <std::os::unix::net::UnixListener as TryInto<I>>::Error:
                    Error + Send + Sync + 'static,
            {
                let conn = hyper_tls::HttpsConnector::new();
                let client = hyper::Client::builder().build(conn);
                Self::with_http_client(client, manifest)
            }
        } else {
            pub fn new(manifest: Manifest) -> impl Future<Output = anyhow::Result<Self>> {
                let conn = hyper_tls::HttpsConnector::new();
                let client = hyper::Client::builder().build(conn);
                Self::with_http_client(client, manifest)
            }
        }
    }
}

impl<S, B, I> App<S, B, I>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>> + Send + 'static,
    I: TryStream + socket::Bind<socket::Addr>,
    I::Ok: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    <I as TryStream>::Error: Error + Send + Sync + 'static,
    <I as socket::Bind<socket::Addr>>::Error: Error + Send + Sync + 'static,
    std::net::TcpListener: TryInto<I>,
    <std::net::TcpListener as TryInto<I>>::Error: Error + Send + Sync + 'static,
{
    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            pub async fn with_http_client(client: S, manifest: Manifest) -> anyhow::Result<Self>
            where
                std::os::unix::net::UnixListener: TryInto<I>,
                <std::os::unix::net::UnixListener as TryInto<I>>::Error:
                    Error + Send + Sync + 'static,
            {
                trace_fn!(App::<S, B, I>::with_http_client);
                Self::with_http_client_(client, manifest, |fds| {
                    if let Some(i) = fds.take_unix_listener(0)? {
                        Ok(Some(i.try_into()?))
                    } else {
                        Ok(None)
                    }
                })
                .await
            }
        } else {
            pub async fn with_http_client(client: S, manifest: Manifest) -> anyhow::Result<Self> {
                trace_fn!(App::<S, B, I>::with_http_client);
                Self::with_http_client_(client, manifest, |_| Ok(None)).await
            }
        }
    }

    async fn with_http_client_<F>(
        client: S,
        manifest: Manifest,
        make_unix_incoming: F,
    ) -> anyhow::Result<Self>
    where
        F: FnOnce(&mut ListenFd) -> anyhow::Result<Option<I>>,
    {
        let core = Core::new(manifest, client)?;
        let websub = Self::init_websub(&core, make_unix_incoming)?;

        let app = App {
            core,
            websub,
            sender: Sender::new(),
        };

        app.sync_websub_subscriptions()?;

        Ok(app)
    }

    fn init_websub<F>(
        core: &Core<S>,
        make_unix_incoming: F,
    ) -> anyhow::Result<Option<websub::Subscriber<S, B, I>>>
    where
        F: FnOnce(&mut ListenFd) -> anyhow::Result<Option<I>>,
    {
        let config = if let Some(ref config) = core.credentials().websub {
            config
        } else {
            return Ok(None);
        };

        let incoming = if let Some(ref bind) = config.bind {
            I::bind(bind)?
        } else {
            let mut fds = ListenFd::from_env();
            if let Some(i) = fds.take_tcp_listener(0).ok().flatten() {
                i.try_into()?
            } else if let Some(i) = make_unix_incoming(&mut fds)? {
                i
            } else {
                anyhow::bail!("Either `websub.bind` in `credentials.toml` or `LISTEN_FD` must be provided for WebSub subscriber");
            }
        };

        let host = config.host.clone();
        let http = core.http_client().clone();
        let pool = core.database_pool().clone();
        let websub = websub::Subscriber::new(core.manifest(), incoming, host, http, pool);

        websub.service().remove_dangling_subscriptions();

        Ok(Some(websub))
    }
}

impl<S, B, I> App<S, B, I>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    pub fn shutdown<'a>(
        mut self: Pin<&'a mut Self>,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        future::poll_fn(move |cx| self.as_mut().poll_shutdown(cx))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        let this = self.project();
        this.core.poll_shutdown(cx).map(|()| Ok(()))
    }

    pub async fn reset(self: Pin<&mut Self>) -> anyhow::Result<()> {
        self.shutdown().await
    }

    /// Replaces the `App`'s manifest.
    ///
    /// Unlike `manifest_mut`, this method takes care of keeping the `App`'s state
    /// consistent with the new manifest.
    ///
    /// Returns the old `Manifest` if successful,
    /// or a tuple of an error value and `manifest` otherwise.
    pub async fn replace_manifest(
        &mut self,
        manifest: Manifest,
    ) -> Result<Manifest, (anyhow::Error, Manifest)> {
        macro_rules! try_ {
            ($r:expr) => {
                match $r {
                    Ok(x) => x,
                    Err(e) => return Err((e.into(), manifest)),
                }
            };
        }

        let old_pool = if manifest.database_url == self.manifest().database_url {
            None
        } else {
            let manager = ConnectionManager::new(manifest.database_url());
            let pool =
                try_!(util::r2d2::new_pool(manager)
                    .context("failed to initialize the connection pool"));
            Some(mem::replace(self.core.database_pool_mut(), pool))
        };
        let credentials = try_!(open_credentials(manifest.credentials_path()));
        let old_credentials = mem::replace(self.core.credentials_mut(), credentials);
        let old = mem::replace(self.manifest_mut(), manifest);

        // An RAII guard to rollback the `App`'s state when the future is canceled.
        struct Guard<'a, S, B, I>
        where
            S: HttpService<B>,
        {
            this: &'a mut App<S, B, I>,
            old: Option<(Manifest, Credentials)>,
            old_pool: Option<Pool<ConnectionManager<SqliteConnection>>>,
        }

        impl<S, B, I> Guard<'_, S, B, I>
        where
            S: HttpService<B>,
        {
            fn rollback(&mut self) -> Manifest {
                let (old, credentials) = self.old.take().unwrap();
                *self.this.core.credentials_mut() = credentials;
                if let Some(pool) = self.old_pool.take() {
                    *self.this.core.database_pool_mut() = pool;
                }

                // We could remove the new tokens from `self.core.twitter_tokens`,
                // but we don't do that because, in case of an error, the caller is typically
                // expected to retry later and reuse the tokens, or just drop the `App`.

                mem::replace(self.this.manifest_mut(), old)
            }
        }

        impl<S, B, I> Drop for Guard<'_, S, B, I>
        where
            S: HttpService<B>,
        {
            fn drop(&mut self) {
                self.rollback();
            }
        }

        let mut guard = Guard {
            this: self,
            old: Some((old, old_credentials)),
            old_pool,
        };
        let this = &mut *guard.this;

        let result = this
            .core
            .load_twitter_tokens()
            .and_then(|()| this.sync_websub_subscriptions());
        match result {
            Ok(()) => {
                let old = guard.old.take().unwrap().0;
                mem::forget(guard);
                *self.core.router_mut() = Router::from_manifest(self.manifest());
                Ok(old)
            }
            Err(e) => {
                let manifest = guard.rollback();
                mem::forget(guard);
                Err((e, manifest))
            }
        }
    }

    fn sync_websub_subscriptions(&self) -> anyhow::Result<()> {
        let websub = if let Some(ref s) = self.websub {
            s
        } else {
            return Ok(());
        };

        let conn = &*self.core.conn()?;

        let topics: HashSet<&str> = self.manifest().feed_topics().collect();
        let subscribed: HashSet<String> = websub_subscriptions::table
            .select(websub_subscriptions::topic)
            .filter(websub_subscriptions::topic.eq_any(&topics))
            .load(conn)?
            .into_iter()
            .collect();

        for &topic in topics.iter().filter(|&&t| !subscribed.contains(t)) {
            let task = websub.service().discover_and_subscribe(topic.to_owned());
            let task = task.map(|result| {
                if let Err(e) = result {
                    log::error!("Error: {:?}", e);
                }
            });
            tokio::spawn(task);
        }

        for topic in subscribed.into_iter().filter(|t| !topics.contains(&**t)) {
            for task in websub.service().unsubscribe_all(topic, conn) {
                let task = task.map(|result| {
                    if let Err(e) = result {
                        log::error!("Error while unsubscribing from a topic: {:?}", e);
                    }
                });
                tokio::spawn(task);
            }
        }

        Ok(())
    }
}

impl<S: HttpService<B>, B, I> App<S, B, I> {
    pub fn manifest(&self) -> &Manifest {
        self.core.manifest()
    }

    pub fn manifest_mut(&mut self) -> &mut Manifest {
        self.core.manifest_mut()
    }

    pub fn credentials(&self) -> &Credentials {
        self.core.credentials()
    }

    pub fn router(&self) -> &Router {
        self.core.router()
    }

    pub fn router_mut(&mut self) -> &mut Router {
        self.core.router_mut()
    }

    pub fn database_pool(&self) -> &Pool<ConnectionManager<SqliteConnection>> {
        self.core.database_pool()
    }

    pub fn http_client(&self) -> &S {
        self.core.http_client()
    }
}

impl<S, B, I> Future for App<S, B, I>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>> + Send + 'static,
    I: TryStream,
    I::Ok: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    I::Error: Error + Send + Sync + 'static,
{
    type Output = anyhow::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        trace_fn!(App::<S, B, I>::poll);

        let this = self.as_mut().project();

        if let Some(mut websub) = this.websub.as_pin_mut() {
            while let Poll::Ready(Some((topic, content))) = websub.as_mut().poll_next(cx)? {
                if let Some(feed) = content.parse_feed() {
                    this.sender.send_feed(&topic, feed, &this.core)?;
                } else {
                    log::warn!("Failed to parse an updated content of topic {}", topic);
                }
            }
        }

        Poll::Pending
    }
}
