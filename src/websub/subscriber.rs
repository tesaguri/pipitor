mod service;

use std::marker::{PhantomData, Unpin};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use futures::channel::mpsc;
use futures::{Stream, StreamExt, TryStream};
use http::Uri;
use hyper::server::conn::Http;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::feed::{self, Feed};
use crate::util::{ArcService, HttpService};

use service::Service;

/// A WebSub subscriber server.
#[pin_project]
pub struct Subscriber<I, S, B> {
    #[pin]
    incoming: I,
    server: Http,
    rx: mpsc::UnboundedReceiver<(String, Content)>,
    service: Arc<Service<S, B>>,
}

pub struct Content {
    kind: feed::MediaType,
    content: Vec<u8>,
}

impl<I, S, B> Subscriber<I, S, B>
where
    I: TryStream,
    I::Ok: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    pub fn new(
        incoming: I,
        host: Uri,
        client: S,
        pool: Pool<ConnectionManager<SqliteConnection>>,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded();

        let service = Arc::new(service::Service {
            host,
            client,
            pool,
            tx,
            _marker: PhantomData,
        });

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

impl<I, S, B> Subscriber<I, S, B> {
    // XXX: We expose the `Service` type rather than exposing its methods through `Subscriber`
    // to prevent the return types of the methods from being bound by `I`
    // (https://github.com/rust-lang/rust/issues/42940).
    pub fn service(&self) -> &Service<S, B> {
        &self.service
    }
}

/// The `Stream` impl yields topic updates that the server has received.
impl<I, S, B> Stream for Subscriber<I, S, B>
where
    I: TryStream,
    I::Ok: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    B: Default + From<Vec<u8>> + Send + 'static,
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
