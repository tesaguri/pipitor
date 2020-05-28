use std::io;
use std::mem::MaybeUninit;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::BufMut;
use tokio::io::{AsyncRead, AsyncWrite};

use super::Bind;

cfg_if::cfg_if! {
    if #[cfg(unix)] {
        use std::fs;

        use futures::StreamExt;

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
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Option<Self::Item>> {
                self.inner.poll_next_unpin(cx).map(|option| {
                    option.map(|result| result.map(|inner| MaybeUnixStream { inner }))
                })
            }
        }

        impl AsyncRead for MaybeUnixStream {
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut [u8],
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.inner).poll_read(cx, buf)
            }

            unsafe fn prepare_uninitialized_buffer(&self, buf: &mut [MaybeUninit<u8>]) -> bool {
                self.inner.prepare_uninitialized_buffer(buf)
            }

            fn poll_read_buf<B: BufMut>(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut B,
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.inner).poll_read_buf(cx, buf)
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

            fn poll_write_buf<B: bytes::Buf>(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut B,
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.inner).poll_write_buf(cx, buf)
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
                _buf: &mut [u8],
            ) -> Poll<io::Result<usize>> {
                match self.inner {}
            }

            unsafe fn prepare_uninitialized_buffer(&self, _buf: &mut [MaybeUninit<u8>]) -> bool {
                match self.inner {}
            }

            fn poll_read_buf<B: BufMut>(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut B,
            ) -> Poll<io::Result<usize>> {
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

            fn poll_write_buf<B: bytes::Buf>(
                self: Pin<&mut Self>,
                _cx: &mut Context<'_>,
                _buf: &mut B,
            ) -> Poll<io::Result<usize>> {
                match self.inner {}
            }
        }
    }
}
