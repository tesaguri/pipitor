use std::fmt::{self, Formatter};
use std::io;
use std::mem::MaybeUninit;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BufMut;
use futures::TryStream;
use pin_project::{pin_project, project};
use tokio::io::{AsyncRead, AsyncWrite};

use serde::de;

#[derive(Clone, Debug)]
pub enum Addr {
    Tcp(SocketAddr),
    Unix(PathBuf),
}

#[pin_project]
pub enum Listener<T = tokio::net::TcpListener, U = tokio::net::UnixListener> {
    Tcp(#[pin] T),
    Unix(#[pin] U),
}

#[pin_project]
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

    #[project]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        #[project]
        match self.project() {
            Listener::Tcp(l) => l
                .try_poll_next(cx)
                .map(|result| result.map(|opt| opt.map(Stream::Tcp))),
            Listener::Unix(l) => l
                .try_poll_next(cx)
                .map(|result| result.map(|opt| opt.map(Stream::Unix))),
        }
    }
}

impl<T, U> AsyncRead for Stream<T, U>
where
    T: AsyncRead,
    U: AsyncRead,
{
    #[project]
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        #[project]
        match self.project() {
            Stream::Tcp(s) => s.poll_read(cx, buf),
            Stream::Unix(s) => s.poll_read(cx, buf),
        }
    }

    unsafe fn prepare_uninitialized_buffer(&self, buf: &mut [MaybeUninit<u8>]) -> bool {
        match *self {
            Stream::Tcp(ref s) => s.prepare_uninitialized_buffer(buf),
            Stream::Unix(ref s) => s.prepare_uninitialized_buffer(buf),
        }
    }

    #[project]
    fn poll_read_buf<B: BufMut>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<io::Result<usize>>
    where
        Self: Sized,
    {
        #[project]
        match self.project() {
            Stream::Tcp(s) => s.poll_read_buf(cx, buf),
            Stream::Unix(s) => s.poll_read_buf(cx, buf),
        }
    }
}

impl<T, U> AsyncWrite for Stream<T, U>
where
    T: AsyncWrite,
    U: AsyncWrite,
{
    #[project]
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        #[project]
        match self.project() {
            Stream::Tcp(s) => s.poll_write(cx, buf),
            Stream::Unix(s) => s.poll_write(cx, buf),
        }
    }

    #[project]
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        #[project]
        match self.project() {
            Stream::Tcp(s) => s.poll_flush(cx),
            Stream::Unix(s) => s.poll_flush(cx),
        }
    }

    #[project]
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        #[project]
        match self.project() {
            Stream::Tcp(s) => s.poll_shutdown(cx),
            Stream::Unix(s) => s.poll_shutdown(cx),
        }
    }

    #[project]
    fn poll_write_buf<B: bytes::Buf>(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut B,
    ) -> Poll<io::Result<usize>>
    where
        Self: Sized,
    {
        #[project]
        match self.project() {
            Stream::Tcp(s) => s.poll_write_buf(cx, buf),
            Stream::Unix(s) => s.poll_write_buf(cx, buf),
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

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        impl Bind<Path> for std::os::unix::net::UnixListener {
            type Error = io::Error;

            fn bind(path: &Path) -> io::Result<Self> {
                Self::bind(path)
            }
        }

        impl Bind<Path> for tokio::net::UnixListener {
            type Error = io::Error;

            fn bind(path: &Path) -> io::Result<Self> {
                Self::bind(path)
            }
        }
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
                if v.starts_with("tcp://") {
                    v[6..].parse().map(Addr::Tcp).map_err(E::custom)
                } else if v.starts_with("unix://") {
                    Ok(Addr::Unix(PathBuf::from(&v[7..])))
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
        }

        d.deserialize_string(Visitor)
    }
}
