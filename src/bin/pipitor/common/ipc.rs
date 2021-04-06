pub use imp::*;

use std::io;
use std::path::Path;

use futures::channel::mpsc;
use futures::{pin_mut, FutureExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::fmt::{self, Debug, Display, Formatter};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

#[cfg(unix)]
mod imp {
    use std::io;
    use std::path::Path;

    use tokio::net::UnixListener;
    use tokio_stream::wrappers::UnixListenerStream;

    pub fn bind(path: &Path) -> io::Result<UnixListenerStream> {
        let listener = UnixListener::bind(path)?;
        Ok(UnixListenerStream::new(listener))
    }
}

#[cfg(not(unix))]
mod imp {
    use std::io;
    use std::marker::Unpin;
    use std::path::Path;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

    pub enum Stream {}

    pub fn bind(
        _: &Path,
    ) -> io::Result<impl futures::Stream<Item = io::Result<impl AsyncRead + AsyncWrite + Unpin>>>
    {
        // TODO: replace this dummy impl. For Windows, named pipes or Unix sockets might be useful.
        // https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/
        Ok(futures::stream::pending::<Result<Stream, _>>())
    }

    impl AsyncRead for Stream {
        fn poll_read(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match *self {}
        }
    }

    impl AsyncWrite for Stream {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            _: &[u8],
        ) -> Poll<io::Result<usize>> {
            match *self {}
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            match *self {}
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
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

pub fn server<P>(path: &P) -> io::Result<impl futures::Stream<Item = (Request, impl AsyncWrite)>>
where
    P: AsRef<Path>,
{
    server_(path.as_ref())
}

fn server_(path: &Path) -> io::Result<impl futures::Stream<Item = (Request, impl AsyncWrite)>> {
    let listener = imp::bind(path)?;
    let (tx, rx) = mpsc::unbounded();

    let task = async move {
        pin_mut!(listener);
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
