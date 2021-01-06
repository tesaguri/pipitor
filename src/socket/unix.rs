use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use super::Bind;

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        use std::convert::{TryFrom, TryInto};
        use std::fs;

        use super::Listener;

        pub struct MaybeUnixListener {
            inner: tokio::net::UnixListener,
        }

        pub struct MaybeUnixStream {
            inner: tokio::net::UnixStream,
        }

        impl Bind<Path> for MaybeUnixListener {
            type Error = io::Error;

            fn bind(path: &Path) -> io::Result<Self> {
                Ok(MaybeUnixListener {
                    inner: Bind::bind(path)?,
                })
            }
        }

        impl futures::Stream for MaybeUnixListener {
            type Item = Result<MaybeUnixStream, io::Error>;

            fn poll_next(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Option<Self::Item>> {
                self.inner
                    .poll_accept(cx)
                    .map(|result| Some(result.map(|(inner, _)| MaybeUnixStream { inner })))
            }
        }

        impl TryFrom<std::os::unix::net::UnixListener> for MaybeUnixListener {
            type Error = io::Error;

            fn try_from(listener: std::os::unix::net::UnixListener) -> io::Result<Self> {
                listener.try_into().map(|inner| MaybeUnixListener { inner })
            }
        }

        impl From<tokio::net::UnixListener> for MaybeUnixListener {
            fn from(listener: tokio::net::UnixListener) -> Self {
                MaybeUnixListener { inner: listener }
            }
        }

        impl<T, U> TryFrom<std::os::unix::net::UnixListener> for Listener<T, U>
        where
            std::os::unix::net::UnixListener: TryInto<U>,
        {
            type Error = <std::os::unix::net::UnixListener as TryInto<U>>::Error;

            fn try_from(listener: std::os::unix::net::UnixListener) -> Result<Self, Self::Error> {
                listener.try_into().map(Listener::Unix)
            }
        }

        impl<T, U> TryFrom<tokio::net::UnixListener> for Listener<T, U>
        where
            tokio::net::UnixListener: TryInto<U>,
        {
            type Error = <tokio::net::UnixListener as TryInto<U>>::Error;

            fn try_from(listener: tokio::net::UnixListener) -> Result<Self, Self::Error> {
                listener.try_into().map(Listener::Unix)
            }
        }

        impl AsyncRead for MaybeUnixStream {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.inner).poll_read(cx, buf)
            }
        }

        impl AsyncWrite for MaybeUnixStream {
            fn poll_write(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<Result<usize, io::Error>> {
                Pin::new(&mut self.inner).poll_write(cx, buf)
            }

            fn poll_flush(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Result<(), io::Error>> {
                Pin::new(&mut self.inner).poll_flush(cx)
            }

            fn poll_shutdown(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Result<(), io::Error>> {
                Pin::new(&mut self.inner).poll_shutdown(cx)
            }

            fn poll_write_vectored(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                bufs: &[io::IoSlice<'_>],
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
            }

            fn is_write_vectored(&self) -> bool {
                self.inner.is_write_vectored()
            }
        }

        impl Bind<Path> for std::os::unix::net::UnixListener {
            type Error = io::Error;

            fn bind(path: &Path) -> io::Result<Self> {
                let _ = fs::remove_file(path);
                Self::bind(path)
            }
        }

        impl Bind<Path> for tokio::net::UnixListener {
            type Error = io::Error;

            fn bind(path: &Path) -> io::Result<Self> {
                let _ = fs::remove_file(path);
                Self::bind(path)
            }
        }
    } else {
        use crate::util::Never;

        pub struct MaybeUnixListener {
            inner: Never,
        }

        pub struct MaybeUnixStream {
            inner: Never,
        }

        impl Bind<Path> for MaybeUnixListener {
            type Error = io::Error;

            fn bind(_path: &Path) -> io::Result<Self> {
                Err(io::Error::new(
                    io::ErrorKind::Other,
                    "Unix domain socket is not supported on this platform",
                ))
            }
        }

        impl futures::Stream for MaybeUnixListener {
            type Item = Result<MaybeUnixStream, io::Error>;

            fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
                match self.inner {}
            }
        }

        impl AsyncRead for MaybeUnixStream {
            fn poll_read(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                match self.inner {}
            }
        }

        impl AsyncWrite for MaybeUnixStream {
            fn poll_write(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &[u8],
            ) -> Poll<Result<usize, io::Error>> {
                match self.inner {}
            }

            fn poll_flush(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), io::Error>> {
                match self.inner {}
            }

            fn poll_shutdown(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
            ) -> Poll<Result<(), io::Error>> {
                match self.inner {}
            }

            fn poll_write_vectored(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _bufs: &[io::IoSlice<'_>],
            ) -> Poll<io::Result<usize>> {
                match self.inner {}
            }

            fn is_write_vectored(&self) -> bool {
                match self.inner {}
            }
        }
    }
}
