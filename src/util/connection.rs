use std::convert::Infallible;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::{future, io};

use futures::channel::mpsc;
use http::Uri;
use hyper::client::connect::{Connected, Connection};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream};
use tower_service::Service;

#[derive(Clone)]
pub struct Connector {
    tx: mpsc::UnboundedSender<Stream>,
}

pub struct Listener {
    rx: mpsc::UnboundedReceiver<Stream>,
}

pub struct Stream {
    inner: DuplexStream,
}

impl Service<Uri> for Connector {
    type Response = Stream;
    type Error = mpsc::SendError;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Uri) -> Self::Future {
        let (tx, rx) = tokio::io::duplex(4096);
        let (tx, rx) = (Stream { inner: tx }, Stream { inner: rx });
        let ret = self
            .tx
            .unbounded_send(rx)
            .and(Ok(tx))
            .map_err(|e| e.into_send_error());
        future::ready(ret)
    }
}

impl AsyncRead for Stream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn poll_write_buf<B: bytes::Buf>(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_buf(cx, buf)
    }
}

impl Connection for Stream {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

impl futures::Stream for Listener {
    type Item = Result<Stream, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        (Pin::new(&mut self.rx))
            .poll_next(cx)
            .map(|option| option.map(Ok))
    }
}

pub fn connection() -> (Connector, Listener) {
    let (tx, rx) = mpsc::unbounded();
    (Connector { tx }, Listener { rx })
}
