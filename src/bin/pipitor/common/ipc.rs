pub use imp::*;

use std::io;
use std::path::Path;

use futures::channel::mpsc;
use futures::{FutureExt, Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::fmt::{self, Debug, Display, Formatter};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(unix)]
mod imp {
    use std::io;
    use std::marker::Unpin;
    use std::path::Path;

    use futures::Stream;
    use tokio::io::{AsyncRead, AsyncWrite};
    use tokio::net::UnixListener;

    pub fn bind(
        path: &Path,
    ) -> io::Result<impl Stream<Item = io::Result<impl AsyncRead + AsyncWrite + Unpin>>> {
        UnixListener::bind(path)
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::marker::Unpin;
    use std::path::Path;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use futures::stream::{self, Stream};
    use tokio::io::{AsyncRead, AsyncWrite};

    pub fn bind(
        _: &Path,
    ) -> io::Result<impl Stream<Item = io::Result<impl AsyncRead + AsyncWrite + Unpin>>> {
        // TODO: replace this dummy impl. For Windows, named pipes or Unix sockets might be useful.
        // https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/
        Ok(stream::pending::<Result<Dummy, _>>())
    }

    enum Dummy {}

    impl AsyncRead for Dummy {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context,
            _: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            match *self {}
        }
    }

    impl AsyncWrite for Dummy {
        fn poll_write(self: Pin<&mut Self>, _: &mut Context, _: &[u8]) -> Poll<io::Result<usize>> {
            match *self {}
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context) -> Poll<io::Result<()>> {
            match *self {}
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context) -> Poll<io::Result<()>> {
            match *self {}
        }
    }
}

#[derive(Deserialize, Serialize)]
pub enum Request {
    #[serde(rename = "reload")]
    Reload {},
    #[serde(rename = "shutdown")]
    Shutdown {},
}

#[derive(Debug, Default, Deserialize, Serialize, thiserror::Error)]
pub struct Response {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub code: ResponseCode,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum ResponseCode {
    #[serde(rename = "SUCCESS")]
    Success,
    #[serde(rename = "INTERNAL_ERROR")]
    InternalError,
    #[serde(rename = "REQUEST_UNRECOGNIZED")]
    RequestUnrecognized,
}

impl Response {
    pub fn new(code: ResponseCode, message: impl Into<Option<String>>) -> Self {
        Response {
            code,
            message: message.into(),
        }
    }

    pub fn result(self) -> Result<Self, Self> {
        if let ResponseCode::Success = self.code {
            Ok(self)
        } else {
            Err(self)
        }
    }
}

impl Display for Response {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if let Some(ref msg) = self.message {
            f.write_str(msg)
        } else {
            f.write_str(self.code.as_str())
        }
    }
}

impl ResponseCode {
    pub fn as_str(&self) -> &'static str {
        match *self {
            ResponseCode::Success => "SUCCESS",
            ResponseCode::InternalError => "INTERNAL_ERROR",
            ResponseCode::RequestUnrecognized => "REQUEST_UNRECOGNIZED",
        }
    }
}

impl Default for ResponseCode {
    fn default() -> Self {
        ResponseCode::Success
    }
}

pub async fn respond<W: AsyncWrite + Unpin>(res: Response, mut w: W) {
    let fut = async move {
        let body = json::to_vec(&res).unwrap();
        w.write_all(&body).await?;
        w.shutdown().await?;
        Ok(()) as std::io::Result<_>
    };
    if let Err(e) = fut.await {
        warn!("failed to write IPC response: {}", e)
    }
}

pub fn server<P>(path: &P) -> io::Result<impl Stream<Item = (Request, impl AsyncWrite)>>
where
    P: AsRef<Path>,
{
    server_(path.as_ref())
}

fn server_(path: &Path) -> io::Result<impl Stream<Item = (Request, impl AsyncWrite)>> {
    let mut listener = imp::bind(path)?;
    let (tx, rx) = mpsc::unbounded();

    let task = async move {
        loop {
            let mut socket = match listener.next().await {
                Some(Ok(socket)) => socket,
                Some(Err(e)) => {
                    warn!("error while accepting an IPC connection: {}", e);
                    continue;
                }
                None => return Ok(()),
            };

            let mut buf = Vec::new();
            if let Err(e) = socket.read_to_end(&mut buf).await {
                warn!("error while reading an IPC request: {}", e);
                continue;
            }

            let req: Request = if let Ok(x) = json::from_slice(&buf) {
                x
            } else {
                info!(
                    "unrecognized IPC request: {:?}",
                    String::from_utf8_lossy(&buf),
                );
                let res = Response::new(
                    ResponseCode::RequestUnrecognized,
                    "request unrecognized".to_owned(),
                );
                tokio::spawn(respond(res, socket));
                continue;
            };

            tx.unbounded_send((req, socket))?;
        }
    };
    tokio::spawn(task.map(|_: Result<(), mpsc::TrySendError<_>>| ()));

    Ok(rx)
}