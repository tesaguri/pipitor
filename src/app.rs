mod core;
mod sender;
mod twitter_list_timeline;

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Write};
use std::mem;
use std::pin::Pin;
use std::task::Context;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::SqliteConnection;
use failure::{Fail, Fallible, ResultExt};
use futures::compat::Stream01CompatExt;
use futures::future;
use futures::{Future, Poll, StreamExt};
use hyper::client::connect::Connect;
use hyper::Client;
use twitter_stream::TwitterStream;

use crate::models;
use crate::rules::TopicId;
use crate::twitter;
use crate::util::Maybe;
use crate::Manifest;

use self::core::Core;
use self::sender::Sender;
use self::twitter_list_timeline::TwitterListTimeline;

pub struct App<C> {
    core: Core<C>,
    twitter_list: TwitterListTimeline,
    twitter: TwitterStream,
    twitter_done: bool,
    sender: Sender,
}

#[cfg(feature = "native-tls")]
impl App<hyper_tls::HttpsConnector<hyper::client::HttpConnector>> {
    pub async fn new(manifest: Manifest) -> Fallible<Self> {
        let conn = hyper_tls::HttpsConnector::new(4).context("failed to initialize TLS client")?;
        let client = Client::builder().build(conn);
        Self::with_http_client(client, manifest).await
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

        let core = Core::new(manifest, client).await?;
        let twitter = core.init_twitter().await?;
        let twitter_list = core.init_twitter_list()?;

        Ok(App {
            core,
            twitter_list,
            twitter,
            twitter_done: false,
            sender: Sender::new(),
        })
    }

    pub fn set_twitter_dump(&mut self, twitter_dump: File) -> io::Result<()> {
        self.core.set_twitter_dump(twitter_dump)
    }

    pub async fn shutdown(&mut self) -> Fallible<()> {
        future::poll_fn(|cx| -> Poll<Fallible<()>> {
            match (
                self.twitter_list
                    .poll_backfill(&mut self.core, &mut self.sender, cx)?,
                self.sender.poll_done(&self.core, cx)?,
            ) {
                (Poll::Ready(()), Poll::Ready(())) => Poll::Ready(Ok(())),
                _ => Poll::Pending,
            }
        })
        .await
    }

    pub async fn reset(&mut self) -> Fallible<()> {
        let twitter_list = if self.twitter_done {
            self.twitter = self.core.init_twitter().await?;
            self.twitter_done = false;
            self.core.init_twitter_list()?
        } else {
            TwitterListTimeline::empty()
        };

        self.shutdown().await?;
        debug_assert!(!self.sender.has_pending());

        self.twitter_list = twitter_list;

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
    ) -> Result<Manifest, (failure::Error, Manifest)> {
        // Store the old field values so that the fields can be rolledback in case of an error.
        let old_pool = if manifest.database_url != self.manifest().database_url {
            let pool = match Pool::new(ConnectionManager::new(manifest.database_url()))
                .context("failed to initialize the connection pool")
            {
                Ok(pool) => pool,
                Err(e) => return Err((e.into(), manifest)),
            };
            Some(mem::replace(self.core.database_pool_mut(), pool))
        } else {
            None
        };
        let old = mem::replace(self.manifest_mut(), manifest);

        let catch = async {
            let new = self.manifest();
            let conn = self.core.conn()?;

            let unauthed_users = new
                .rule
                .twitter_outboxes()
                .chain(Some(new.twitter.user))
                .filter(|user| !self.core.twitter_tokens.contains_key(&user))
                // Make the values unique so that the later `_.len() != _.len()`
                // comparison makes sense.
                .collect::<HashSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();

            let tokens: Vec<models::TwitterToken> = {
                use crate::schema::twitter_tokens::dsl::*;

                twitter_tokens
                    .filter(id.eq_any(&unauthed_users))
                    .load(&*conn)?
            };

            if unauthed_users.len() != tokens.len() {
                return Err(unauthorized());
            }

            self.core
                .twitter_tokens
                .extend(tokens.into_iter().map(|token| (token.id, token.into())));

            // Make borrowck happy
            let new = self.manifest();

            if new.twitter.client.key != old.twitter.client.key
                || new.twitter.user != old.twitter.user
                || new
                    .rule
                    .twitter_topics()
                    .any(|user| !old.rule.contains_topic(&TopicId::Twitter(user)))
            {
                let twitter = self.core.init_twitter().await?;
                let twitter_list = self.core.init_twitter_list()?;
                self.twitter = twitter;
                self.twitter_list = twitter_list;
                self.twitter_done = false;
            }

            Ok(())
        }
            .await;

        match catch {
            Ok(()) => Ok(old),
            Err(e) => {
                let manifest = mem::replace(self.manifest_mut(), old);
                if let Some(pool) = old_pool {
                    *self.core.database_pool_mut() = pool;
                }

                // We could remove the tokens added above from `self.core.twitter_tokens`,
                // but we don't do that because, in case of an error, the caller is typically
                // expected to retry later and reuse the tokens, or just drop the `App`.

                Err((e, manifest))
            }
        }
    }

    fn poll_twitter(&mut self, cx: &mut Context<'_>) -> Poll<Fallible<()>> {
        while let Poll::Ready(v) = (&mut self.twitter).compat().poll_next_unpin(cx) {
            let result = if let Some(r) = v {
                r
            } else {
                self.twitter_done = true;
                return Poll::Ready(Ok(()));
            };

            let json = result.map_err(|e| {
                self.twitter_done = true;
                e.context("error while listening to Twitter's Streaming API")
            })?;

            self.core.with_twitter_dump(|dump| {
                dump.write_all(json.trim_end().as_bytes())?;
                dump.write_all(b"\n")
            })?;

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
            let will_process = self.manifest().rule.contains_topic(&TopicId::Twitter(from))
                && tweet.in_reply_to_user_id.map_or(true, |to| {
                    self.manifest().rule.contains_topic(&TopicId::Twitter(to))
                });
            if will_process {
                self.sender.send_tweet(tweet, &self.core)?;
            }
        }

        Poll::Pending
    }
}

impl<C> App<C> {
    pub fn manifest(&self) -> &Manifest {
        self.core.manifest()
    }

    pub fn manifest_mut(&mut self) -> &mut Manifest {
        self.core.manifest_mut()
    }

    pub fn database_pool(&self) -> &Pool<ConnectionManager<SqliteConnection>> {
        self.core.database_pool()
    }

    pub fn http_client(&self) -> &Client<C> {
        self.core.http_client()
    }
}

impl<C> Future for App<C>
where
    C: Connect + Sync + 'static,
    C::Transport: 'static,
    C::Future: 'static,
{
    type Output = Fallible<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Fallible<()>> {
        trace_fn!(App::<C>::poll);

        let this = self.get_mut();

        let _ = this.twitter_list.poll(&this.core, &mut this.sender, cx)?;

        let _ = this
            .twitter_list
            .poll_backfill(&mut this.core, &mut this.sender, cx)?;

        if let Poll::Ready(result) = this.poll_twitter(cx) {
            return Poll::Ready(result);
        }

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

fn unauthorized() -> failure::Error {
    failure::err_msg("not all Twitter users are authorized; please run `pipitor twitter-login`")
}
