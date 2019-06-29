use std::ffi::OsString;
use std::fmt::{self, Debug, Display, Formatter};
use std::path::{Path, PathBuf};
use std::{env, fs, io};

use failure::{AsFail, Fail, Fallible, ResultExt};
use futures::compat::Stream01CompatExt;
use futures::future::{self, Future, FutureExt};
use futures::StreamExt;
use futures01::Stream as Stream01;
use pipitor::Manifest;
use serde::{Deserialize, Serialize};

pub struct DisplayFailChain<'a, F>(pub &'a F);

#[derive(Deserialize, Serialize)]
pub enum IpcRequest {
    #[serde(rename = "reload")]
    Reload {},
    #[serde(rename = "shutdown")]
    Shutdown {},
}

#[derive(Debug, Default, Deserialize, Fail, Serialize)]
pub struct IpcResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub code: IpcResponseCode,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum IpcResponseCode {
    #[serde(rename = "SUCCESS")]
    Success,
    #[serde(rename = "INTERNAL_ERROR")]
    InternalError,
    #[serde(rename = "REQUEST_UNRECOGNIZED")]
    RequestUnrecognized,
}

#[derive(Clone, structopt::StructOpt)]
pub struct Opt {
    #[structopt(long = "manifest-path", help = "Path to Pipitor.toml")]
    manifest_path: Option<String>,
}

pub struct RmGuard<P: AsRef<Path>>(pub P);

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

    pub fn result(self) -> Result<Self, Self> {
        if let IpcResponseCode::Success = self.code {
            Ok(self)
        } else {
            Err(self)
        }
    }
}

impl Display for IpcResponse {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        if let Some(ref msg) = self.message {
            f.write_str(msg)
        } else {
            f.write_str(self.code.as_str())
        }
    }
}

impl IpcResponseCode {
    pub fn as_str(&self) -> &'static str {
        match *self {
            IpcResponseCode::Success => "SUCCESS",
            IpcResponseCode::InternalError => "INTERNAL_ERROR",
            IpcResponseCode::RequestUnrecognized => "REQUEST_UNRECOGNIZED",
        }
    }
}

impl Default for IpcResponseCode {
    fn default() -> Self {
        IpcResponseCode::Success
    }
}

impl Opt {
    pub fn manifest_path(&self) -> &Path {
        self.manifest_path
            .as_ref()
            .map(AsRef::as_ref)
            .unwrap_or("Pipitor.toml".as_ref())
    }
}

impl<P: AsRef<Path>> Drop for RmGuard<P> {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
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

pub fn open_manifest(opt: &Opt) -> Fallible<Manifest> {
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
    use futures::future;
    use futures::{AsyncWrite, Future, Stream, TryFutureExt, TryStreamExt};
    use tokio_signal::unix::{
        libc::{SIGINT, SIGTERM},
        Signal,
    };
    use tokio_uds::UnixListener;

    pub fn ipc_server<P>(
        path: &P,
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

    pub fn quit_signal() -> impl Future<Output = io::Result<impl Future<Output = ()>>> {
        future::try_join(Signal::new(SIGINT).compat(), Signal::new(SIGTERM).compat())
            .map_ok(|(int, term)| super::merge_select(super::first(int), super::first(term)))
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::path::Path;

    use futures::compat::Future01CompatExt;
    use futures::{future, stream};
    use futures::{AsyncWrite, Future, Stream, TryFutureExt};
    use tokio_signal::windows::Event;

    pub fn ipc_server<P>(
        _: &P,
    ) -> io::Result<impl Stream<Item = io::Result<(Vec<u8>, impl AsyncWrite)>>>
    where
        P: AsRef<Path>,
    {
        // TODO: replace this dummy impl
        // (perhaps with named pipe or Unix socket (https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/))
        Ok(stream::empty::<io::Result<(_, Vec<u8>)>>())
    }

    pub fn quit_signal() -> impl Future<Output = io::Result<impl Future<Output = ()>>> {
        future::try_join(Event::ctrl_c().compat(), Event::ctrl_break().compat())
            .map_ok(|(cc, cb)| super::merge_select(super::first(cc), super::first(cb)))
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
