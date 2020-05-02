pub(crate) mod core;

mod sender;
mod twitter_request_ext;

use std::marker::PhantomData;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::SqliteConnection;
use futures::{future, ready, Future, Stream, StreamExt};
use http_body::Body;
use pin_project::pin_project;
use twitter_stream::TwitterStream;

use crate::credentials::Credentials;
use crate::manifest::{Manifest, TopicId};
use crate::router::Router;
use crate::twitter;
use crate::util::{open_credentials, HttpService, Maybe};

use self::core::Core;
use self::sender::Sender;
use self::twitter_request_ext::TwitterRequestExt;

#[pin_project]
pub struct App<S, B>
where
    S: HttpService<B>,
{
    #[pin]
    core: Core<S>,
    #[pin]
    twitter_list: twitter::ListTimeline<S, B>,
    #[pin]
    twitter: TwitterStream<S::ResponseBody>,
    twitter_done: bool,
    #[pin]
    sender: Sender<S, B>,
    body_marker: PhantomData<B>,
}

#[cfg(feature = "native-tls")]
impl App<hyper::Client<hyper_tls::HttpsConnector<hyper::client::HttpConnector>>, hyper::Body> {
    pub fn new(manifest: Manifest) -> impl Future<Output = anyhow::Result<Self>> {
        let conn = hyper_tls::HttpsConnector::new();
        let client = hyper::Client::builder().build(conn);
        Self::with_http_client(client, manifest)
    }
}

impl<S, B> App<S, B>
where
    S: HttpService<B> + Clone,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    pub async fn with_http_client(client: S, manifest: Manifest) -> anyhow::Result<Self> {
        trace_fn!(App::<S, B>::with_http_client);
        let core = Core::new(manifest, client)?;
        let twitter = core.init_twitter().await?;
        let twitter_list = core.init_twitter_list()?;
        Ok(App {
            core,
            twitter_list,
            twitter,
            twitter_done: false,
            sender: Sender::new(),
            body_marker: PhantomData,
        })
    }

    pub fn shutdown<'a>(
        mut self: Pin<&'a mut Self>,
    ) -> impl Future<Output = anyhow::Result<()>> + 'a {
        future::poll_fn(move |cx| self.as_mut().poll_shutdown(cx))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        let mut this = self.project();
        let poll_sender = this.sender.as_mut().poll_done(&this.core, cx);
        while let Some(tweets) = ready!(this.twitter_list.as_mut().poll_next_backfill(cx)) {
            this.sender.as_mut().send_tweets(tweets, &this.core)?;
        }
        poll_sender
    }

    pub async fn reset(mut self: Pin<&mut Self>) -> anyhow::Result<()> {
        let twitter_list = if self.twitter_done {
            let mut this = self.as_mut().project();
            this.twitter.set(this.core.init_twitter().await?);
            *this.twitter_done = false;
            self.core.init_twitter_list()?
        } else {
            twitter::ListTimeline::empty()
        };

        self.as_mut().shutdown().await?;
        debug_assert!(!self.sender.has_pending());

        self.project().twitter_list.set(twitter_list);

        Ok(())
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

        let old_pool = if manifest.database_url != self.manifest().database_url {
            let pool = try_!(Pool::new(ConnectionManager::new(manifest.database_url()))
                .context("failed to initialize the connection pool"));
            Some(mem::replace(self.core.database_pool_mut(), pool))
        } else {
            None
        };
        let credentials = try_!(open_credentials(manifest.credentials_path()));
        let old_credentials = mem::replace(self.core.credentials_mut(), credentials);
        let old = mem::replace(self.manifest_mut(), manifest);

        // An RAII guard to rollback the `App`'s state when the future is canceled.
        struct Guard<'a, S, B>
        where
            S: HttpService<B>,
        {
            this: &'a mut App<S, B>,
            old: Option<(Manifest, Credentials)>,
            old_pool: Option<Pool<ConnectionManager<SqliteConnection>>>,
        }

        impl<S, B> Guard<'_, S, B>
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

        impl<S, B> Drop for Guard<'_, S, B>
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
        let this = &mut guard.this;
        let (ref old, ref old_credentials) = *guard.old.as_ref().unwrap();

        let catch = async {
            this.core.load_twitter_tokens()?;

            let new = this.manifest();
            if this.credentials().twitter.client.identifier
                != old_credentials.twitter.client.identifier
                || new.twitter.user != old.twitter.user
                || new
                    .twitter_topics()
                    .any(|user| !old.has_topic(&TopicId::Twitter(user)))
            {
                let twitter = this.core.init_twitter().await?;
                let twitter_list = this.core.init_twitter_list()?;
                this.twitter = twitter;
                this.twitter_list = twitter_list;
                this.twitter_done = false;
            }

            Ok(())
        }
        .await;

        match catch {
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

    fn poll_twitter(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        let mut this = self.project();

        while let Poll::Ready(v) = this.twitter.poll_next_unpin(cx) {
            let result = if let Some(r) = v {
                r
            } else {
                *this.twitter_done = true;
                return Poll::Ready(Ok(()));
            };

            let json = match result.context("error while listening to Twitter's Streaming API") {
                Ok(json) => json,
                Err(e) => {
                    *this.twitter_done = true;
                    return Poll::Ready(Err(e));
                }
            };

            let tweet = if let Maybe::Just(t) = json::from_str::<Maybe<twitter::Tweet>>(&json)? {
                t
            } else {
                continue;
            };

            if log_enabled!(log::Level::Trace) {
                let created_at = snowflake_to_system_time(tweet.id as u64);
                match SystemTime::now().duration_since(created_at) {
                    Ok(latency) => trace!("Twitter stream latency: {:.2?}", latency),
                    Err(e) => trace!("Twitter stream latency: -{:.2?}", e.duration()),
                }
            }

            let from = tweet.user.id;
            let will_process = this.core.manifest().has_topic(&TopicId::Twitter(from))
                && tweet.in_reply_to_user_id.map_or(true, |to| {
                    this.core.manifest().has_topic(&TopicId::Twitter(to))
                });
            if will_process {
                this.sender.as_mut().send_tweet(tweet, &this.core)?;
            }
        }

        Poll::Pending
    }
}

impl<S: HttpService<B>, B> App<S, B> {
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

impl<S, B> Future for App<S, B>
where
    S: HttpService<B> + Clone,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    type Output = anyhow::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<anyhow::Result<()>> {
        trace_fn!(App::<S, B>::poll);

        let mut this = self.as_mut().project();

        while let Poll::Ready(Some(tweets)) = this.twitter_list.as_mut().poll_next(cx)? {
            this.sender.as_mut().send_tweets(tweets, &this.core)?;
        }

        if let Poll::Ready(result) = self.as_mut().poll_twitter(cx) {
            return Poll::Ready(result);
        }

        let this = self.project();
        let _ = this.sender.poll_done(&this.core, cx)?;

        Poll::Pending
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
