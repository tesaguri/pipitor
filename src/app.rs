mod core;
mod sender;
mod twitter_list_timeline;

use std::fs::File;
use std::io::{self, Write};
use std::pin::Pin;
use std::task::Context;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use diesel::r2d2::{ConnectionManager, Pool};
use diesel::SqliteConnection;
use failure::{Fail, Fallible, ResultExt};
use futures::compat::Stream01CompatExt;
use futures::future::{self, Future};
use futures::stream::StreamExt;
use futures::Poll;
use hyper::client::connect::Connect;
use hyper::Client;
use twitter_stream::TwitterStream;

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
                    Ok(latency) => debug!("Twitter stream latency: {:.2?}", latency),
                    Err(e) => debug!("Twitter stream latency: -{:.2?}", e.duration()),
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
