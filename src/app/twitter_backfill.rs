use std::io::Write;

use std::task::Context;

use failure::Fallible;

use futures::future::FutureExt;
use futures::{ready, Poll};
use hyper::client::connect::Connect;

use crate::twitter::{self, Request as _};

use super::{Core, Sender};

pub struct TwitterBackfill {
    inner: Option<Inner>,
}

struct Inner {
    list: u64,
    since_id: i64,
    response: twitter::ResponseFuture<Vec<twitter::Tweet>>,
}

impl TwitterBackfill {
    pub fn new<C>(list: u64, since_id: i64, core: &Core<C>) -> Self
    where
        C: Connect + Sync + 'static,
        C::Transport: 'static,
        C::Future: 'static,
    {
        let token = core.twitter_token(core.manifest().twitter.user).unwrap();
        let response = twitter::lists::Statuses::new(list)
            .since_id(Some(since_id))
            .send(
                core.manifest().twitter.client.as_ref(),
                token,
                core.http_client(),
            );
        TwitterBackfill {
            inner: Some(Inner {
                list,
                since_id,
                response,
            }),
        }
    }

    pub fn empty() -> Self {
        TwitterBackfill { inner: None }
    }

    pub fn poll<C>(
        &mut self,
        core: &mut Core<C>,
        sender: &mut Sender,
        cx: &mut Context<'_>,
    ) -> Poll<Fallible<()>>
    where
        C: Connect + Sync + 'static,
        C::Transport: 'static,
        C::Future: 'static,
    {
        let inner = if let Some(ref mut inner) = self.inner {
            inner
        } else {
            return Poll::Ready(Ok(()));
        };

        let tweets = match ready!(inner.response.poll_unpin(cx)) {
            Ok(resp) => resp.response,
            Err(e) => {
                self.inner = None;
                return Poll::Ready(Err(e.into()));
            }
        };

        if tweets.is_empty() {
            debug!("timeline backfilling completed");
            self.inner = None;
            return Poll::Ready(Ok(()));
        }

        let max_id = tweets.iter().map(|t| t.id).min().map(|id| id - 1);
        // Make borrowck happy
        let since_id = inner.since_id;
        let list = inner.list;

        let response = twitter::lists::Statuses::new(list)
            .since_id(Some(since_id))
            .max_id(max_id)
            .send(
                core.manifest().twitter.client.as_ref(),
                core.twitter_token(core.manifest().twitter.user).unwrap(),
                core.http_client(),
            );

        for t in tweets {
            core.with_twitter_dump(|mut dump| {
                json::to_writer(&mut dump, &t)?;
                dump.write_all(b"\n")
            })?;
            if t.retweeted_status.is_none() {
                sender.send_tweet(t, core)?;
            }
        }

        self.inner = Some(Inner {
            list,
            since_id,
            response,
        });

        Poll::Pending
    }
}
