use std::env;
use std::ffi::OsString;
use std::fmt::{self, Debug, Display, Formatter};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use failure::{AsFail, Fail, Fallible, ResultExt};
use futures::compat::Stream01CompatExt;
use futures::future::{self, Future, FutureExt};
use futures::StreamExt;
use futures01::Stream as Stream01;
use pipitor::Manifest;
use serde::{Deserialize, Serialize};

pub struct DisplayFailChain<'a, F>(pub &'a F);

#[derive(Deserialize)]
pub enum IpcRequest {
    #[serde(rename = "reload")]
    Reload {},
    #[serde(rename = "shutdown")]
    Shutdown {},
}

#[derive(Default, Serialize)]
pub struct IpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub code: IpcResponseCode,
}

#[derive(PartialEq, Eq, Serialize)]
pub enum IpcResponseCode {
    #[serde(rename = "SUCCESS")]
    Success,
    #[serde(rename = "INTERNAL_ERROR")]
    InternalError,
    #[serde(rename = "REQUEST_UNRECOGNIZED")]
    RequestUnrecognized,
}

impl<'a, F: AsFail> Display for DisplayFailChain<'a, F> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let fail = self.0.as_fail();

        write!(f, "Error: {}", fail)?;

        for e in fail.iter_causes() {
            write!(f, "\nCaused by: {}", e)?;
        }

        if let Some(backtrace) = fail.backtrace() {
            let backtrace_enabled = match env::var_os("RUST_FAILURE_BACKTRACE") {
                Some(ref v) if v != "0" => true,
                Some(_) => false,
                _ => match env::var_os("RUST_BACKTRACE") {
                    Some(ref v) if v != "0" => true,
                    _ => false,
                },
            };
            if backtrace_enabled {
                write!(f, "\n{}", backtrace)?;
            }
        }

        Ok(())
    }
}

impl IpcResponse {
    pub fn new(code: IpcResponseCode, message: impl Into<Option<String>>) -> Self {
        IpcResponse {
            code,
            message: message.into(),
        }
    }
}

impl Default for IpcResponseCode {
    fn default() -> Self {
        IpcResponseCode::Success
    }
}

pub fn ipc_path<P: AsRef<Path>>(manifest_path: P) -> PathBuf {
    ipc_path_(manifest_path.as_ref())
}

fn ipc_path_(manifest_path: &Path) -> PathBuf {
    let name = manifest_path.file_name().unwrap();
    let mut sock = OsString::with_capacity(name.len() + 6);
    sock.push(".");
    sock.push(name);
    sock.push(".sock");
    manifest_path.with_file_name(sock)
}

pub fn open_manifest(opt: &crate::Opt) -> Fallible<Manifest> {
    let manifest = if let Some(ref manifest_path) = opt.manifest_path {
        let manifest = match fs::read(manifest_path) {
            Ok(f) => f,
            Err(e) => {
                return Err(e
                    .context(format!(
                        "could not open the manifest at `{}`",
                        manifest_path,
                    ))
                    .into())
            }
        };
        toml::from_slice(&manifest).context("failed to parse the manifest file")?
    } else {
        let manifest = match fs::read("Pipitor.toml") {
            Ok(f) => f,
            Err(e) => {
                return if e.kind() == io::ErrorKind::NotFound {
                    Err(failure::err_msg(
                        "could not find `Pipitor.toml` in the current directory",
                    ))
                } else {
                    Err(e.context("could not open `Pipitor.toml`").into())
                };
            }
        };
        toml::from_slice(&manifest).context("failed to parse `Pipitor.toml`")?
    };

    Ok(manifest)
}

pub use imp::*;

#[cfg(unix)]
mod imp {
    use std::io;
    use std::path::Path;

    use futures::compat::{AsyncWrite01CompatExt, Future01CompatExt, Stream01CompatExt};
    use futures::{AsyncWrite, Future, Stream, TryStreamExt};
    use tokio_signal::unix::{
        libc::{SIGINT, SIGTERM},
        Signal,
    };
    use tokio_uds::UnixListener;

    pub fn ipc_server<P>(
        path: P,
    ) -> io::Result<impl Stream<Item = io::Result<(Vec<u8>, impl AsyncWrite)>>>
    where
        P: AsRef<Path>,
    {
        ipc_server_(path.as_ref())
    }

    fn ipc_server_(
        path: &Path,
    ) -> io::Result<impl Stream<Item = io::Result<(Vec<u8>, impl AsyncWrite)>>> {
        Ok(UnixListener::bind(path)?
            .incoming()
            .compat()
            .and_then(|a| tokio::io::read_to_end(a, Vec::new()).compat())
            .map_ok(|(a, buf)| (buf, a.compat())))
    }

    pub async fn quit_signal() -> io::Result<impl Future<Output = ()>> {
        let (int, term) = (Signal::new(SIGINT).compat(), Signal::new(SIGTERM).compat());
        let (int, term) = futures::try_join!(int, term)?;
        Ok(super::merge_select(super::first(int), super::first(term)))
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::marker::Unpin;
    use std::path::Path;

    use futures::compat::Future01CompatExt;
    use futures::stream::{self, Stream};
    use futures::{AsyncRead, Future};
    use tokio_signal::windows::Event;

    pub fn ipc_server<P>(
        _: P,
    ) -> io::Result<impl Stream<Item = io::Result<(Vec<u8>, impl AsyncWrite)>>>
    where
        P: AsRef<Path>,
    {
        // TODO: replace this dummy impl
        // (perhaps with named pipe or Unix socket (https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/))
        Ok(stream::empty::<io::Result<(_, Vec<u8>)>>())
    }

    pub async fn quit_signal() -> io::Result<impl Future<Output = ()>> {
        let (cc, cb) = (Event::ctrl_c().compat(), Event::ctrl_break().compat());
        let (cc, cb) = futures::try_join!(cc, cb)?;
        Ok(super::merge_select(super::first(cc), super::first(cb)))
    }
}

fn first<S: Stream01>(s: S) -> impl Future<Output = ()>
where
    S::Error: Debug,
{
    s.compat().into_future().map(|(r, _)| {
        r.unwrap().unwrap();
    })
}

fn merge_select<A, B>(a: A, b: B) -> impl Future<Output = A::Output>
where
    A: Future + Unpin,
    B: Future<Output = A::Output> + Unpin,
{
    future::select(a, b).map(|either| either.factor_first().0)
}
