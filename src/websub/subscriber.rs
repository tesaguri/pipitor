mod scheduler;
mod service;

use std::convert::TryInto;
use std::marker::{PhantomData, Unpin};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use diesel::dsl::*;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use futures::channel::mpsc;
use futures::{Stream, StreamExt, TryStream};
use http::Uri;
use hyper::server::conn::Http;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::feed::{self, Feed};
use crate::query;
use crate::schema::*;
use crate::util::{ArcService, HttpService};

use self::scheduler::Scheduler;
use self::service::Service;

/// A WebSub subscriber server.
#[pin_project]
pub struct Subscriber<S, B, I> {
    #[pin]
    incoming: I,
    server: Http,
    rx: mpsc::Receiver<(String, Content)>,
    service: Arc<Service<S, B>>,
}

pub struct Content {
    kind: feed::MediaType,
    content: Vec<u8>,
}

// XXX: mediocre naming
const RENEW: Duration = Duration::from_secs(10);

impl<S, B, I> Subscriber<S, B, I>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    B: Default + From<Vec<u8>> + Send + 'static,
    I: TryStream,
    I::Ok: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    pub fn new(
        incoming: I,
        host: Uri,
        client: S,
        pool: Pool<ConnectionManager<SqliteConnection>>,
    ) -> Self {
        let first_tick = query::expires_at()
            .first::<i64>(&pool.get().unwrap())
            .optional()
            .unwrap()
            .map(|expires_at| expires_at.try_into().map_or(0, refresh_time));

        let (tx, rx) = mpsc::channel(0);

        let service = Arc::new(service::Service {
            host,
            client,
            pool,
            tx,
            handle: scheduler::Handle::new(first_tick),
            _marker: PhantomData,
        });

        tokio::spawn(Scheduler::new(&service, |service| {
            let conn = &*service.pool.get().unwrap();
            service.renew_subscriptions(conn);
            query::expires_at()
                .filter(not(
                    websub_active_subscriptions::id.eq_any(query::renewing_subs())
                ))
                .first::<i64>(conn)
                .optional()
                .unwrap()
                .map(|expires_at| expires_at.try_into().map_or(0, refresh_time))
        }));

        Subscriber {
            incoming,
            server: Http::new(),
            rx,
            service,
        }
    }

    fn accept_all(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), I::Error>> {
        let mut this = self.project();
        while let Poll::Ready(option) = this.incoming.as_mut().try_poll_next(cx)? {
            match option {
                None => return Poll::Ready(Ok(())),
                Some(sock) => {
                    let service = ArcService(this.service.clone());
                    tokio::spawn(this.server.serve_connection(sock, service));
                }
            }
        }

        Poll::Pending
    }
}

impl<S, B, I> Subscriber<S, B, I> {
    // XXX: We expose the `Service` type rather than exposing its methods through `Subscriber`
    // to prevent the return types of the methods from being bound by `I`
    // (https://github.com/rust-lang/rust/issues/42940).
    pub fn service(&self) -> &Service<S, B> {
        &self.service
    }
}

/// The `Stream` impl yields topic updates that the server has received.
impl<S, B, I> Stream for Subscriber<S, B, I>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    B: Default + From<Vec<u8>> + Send + 'static,
    I: TryStream,
    I::Ok: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Item = Result<(String, Content), I::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        log::trace!("Subscriber::poll_next");

        let incoming_done = matches!(self.as_mut().accept_all(cx)?, Poll::Ready(()));

        match self.as_mut().project().rx.poll_next_unpin(cx) {
            Poll::Ready(option) => Poll::Ready(option.map(Ok)),
            Poll::Pending => {
                if incoming_done && Arc::strong_count(&self.service) == 1 {
                    Poll::Ready(None)
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

impl Content {
    pub fn parse_feed(&self) -> Option<Feed> {
        Feed::parse(self.kind, &self.content)
    }
}

fn refresh_time(expires_at: u64) -> u64 {
    expires_at - RENEW.as_secs()
}
