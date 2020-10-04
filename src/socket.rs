pub mod unix;

use std::convert::{TryFrom, TryInto};
use std::fmt::{self, Formatter};
use std::io;
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BufMut;
use futures::TryStream;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use serde::de;

#[derive(Clone, Debug)]
pub enum Addr {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

#[pin_project(project = ListenerProj)]
pub enum Listener<T = tokio::net::TcpListener, U = unix::MaybeUnixListener> {
    Tcp(#[pin] T),
    Unix(#[pin] U),
}

#[pin_project(project = StreamProj)]
pub enum Stream<T, U> {
    Tcp(#[pin] T),
    Unix(#[pin] U),
}

pub trait Bind<A>: Sized
where
    A: ?Sized,
{
    type Error;

    fn bind(addr: &A) -> Result<Self, Self::Error>;
}

impl<T, U> Bind<Addr> for Listener<T, U>
where
    T: Bind<SocketAddr>,
    U: Bind<Path, Error = T::Error>,
{
    type Error = T::Error;

    fn bind(addr: &Addr) -> Result<Self, Self::Error> {
        match *addr {
            Addr::Tcp(ref addr) => T::bind(addr).map(Listener::Tcp),
            Addr::Unix(ref path) => U::bind(path).map(Listener::Unix),
        }
    }
}

impl<T, U> futures::Stream for Listener<T, U>
where
    T: TryStream,
    U: TryStream<Error = T::Error>,
{
    type Item = Result<Stream<T::Ok, U::Ok>, T::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project() {
            ListenerProj::Tcp(l) => l
                .try_poll_next(cx)
                .map(|result| result.map(|opt| opt.map(Stream::Tcp))),
            ListenerProj::Unix(l) => l
                .try_poll_next(cx)
                .map(|result| result.map(|opt| opt.map(Stream::Unix))),
        }
    }
}

impl<T, U> TryFrom<std::net::TcpListener> for Listener<T, U>
where
    std::net::TcpListener: TryInto<T>,
{
    type Error = <std::net::TcpListener as TryInto<T>>::Error;

    fn try_from(listener: std::net::TcpListener) -> Result<Self, Self::Error> {
        listener.try_into().map(Listener::Tcp)
    }
}

impl<T, U> TryFrom<tokio::net::TcpListener> for Listener<T, U>
where
    tokio::net::TcpListener: TryInto<T>,
{
    type Error = <tokio::net::TcpListener as TryInto<T>>::Error;

    fn try_from(listener: tokio::net::TcpListener) -> Result<Self, Self::Error> {
        listener.try_into().map(Listener::Tcp)
    }
}

impl<T, U> AsyncRead for Stream<T, U>
where
    T: AsyncRead,
    U: AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        match self.project() {
            StreamProj::Tcp(s) => s.poll_read(cx, buf),
            StreamProj::Unix(s) => s.poll_read(cx, buf),
        }
    }

    unsafe fn prepare_uninitialized_buffer(&self, buf: &mut [MaybeUninit<u8>]) -> bool {
        match *self {
            Stream::Tcp(ref s) => s.prepare_uninitialized_buffer(buf),
            Stream::Unix(ref s) => s.prepare_uninitialized_buffer(buf),
        }
    }

    fn poll_read_buf<B: BufMut>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<io::Result<usize>> {
        match self.project() {
            StreamProj::Tcp(s) => s.poll_read_buf(cx, buf),
            StreamProj::Unix(s) => s.poll_read_buf(cx, buf),
        }
    }
}

impl<T, U> AsyncWrite for Stream<T, U>
where
    T: AsyncWrite,
    U: AsyncWrite,
{
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match self.project() {
            StreamProj::Tcp(s) => s.poll_write(cx, buf),
            StreamProj::Unix(s) => s.poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            StreamProj::Tcp(s) => s.poll_flush(cx),
            StreamProj::Unix(s) => s.poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.project() {
            StreamProj::Tcp(s) => s.poll_shutdown(cx),
            StreamProj::Unix(s) => s.poll_shutdown(cx),
        }
    }

    fn poll_write_buf<B: bytes::Buf>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<io::Result<usize>> {
        match self.project() {
            StreamProj::Tcp(s) => s.poll_write_buf(cx, buf),
            StreamProj::Unix(s) => s.poll_write_buf(cx, buf),
        }
    }
}

impl Bind<SocketAddr> for std::net::TcpListener {
    type Error = io::Error;

    fn bind(addr: &SocketAddr) -> io::Result<Self> {
        Self::bind(*addr)
    }
}

impl Bind<SocketAddr> for tokio::net::TcpListener {
    type Error = io::Error;

    fn bind(addr: &SocketAddr) -> io::Result<Self> {
        std::net::TcpListener::bind(*addr).and_then(Self::from_std)
    }
}

impl<'de> de::Deserialize<'de> for Addr {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Addr;

            fn expecting(&self, f: &mut Formatter<'_>) -> fmt::Result {
                write!(f, "an IP address (tcp://xxx.xxx.xxx.xxx) or a UNIX domain socket path (unix://...)")
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Addr, E> {
                if let Some(addr) = v.strip_prefix("tcp://") {
                    addr.parse().map(Addr::Tcp).map_err(E::custom)
                } else if let Some(path) = v.strip_prefix("unix://") {
                    Ok(Addr::Unix(PathBuf::from(path)))
                } else {
                    Err(E::custom("unknown bind address type"))
                }
            }

            fn visit_string<E: de::Error>(self, mut v: String) -> Result<Addr, E> {
                if v.starts_with("unix://") {
                    v.drain(..7);
                    Ok(Addr::Unix(PathBuf::from(v)))
                } else {
                    self.visit_str(&v)
                }
            }

            serde_delegate!(visit_bytes visit_byte_buf);
        }

        d.deserialize_string(Visitor)
    }
}
