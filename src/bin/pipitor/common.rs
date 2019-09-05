use std::ffi::OsString;
use std::fmt::{self, Debug, Display, Formatter};
use std::marker::Unpin;
use std::path::{Path, PathBuf};
use std::{env, fs, io};

use failure::{AsFail, Fail, Fallible, ResultExt};
use futures::future::{self, Future, FutureExt};
use futures::{Stream, StreamExt};
use pipitor::{Credentials, Manifest};
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
    let path: &str;
    let mut manifest: Manifest = if let Some(ref p) = opt.manifest_path {
        path = p;
        let buf = fs::read(path)
            .with_context(|_| format!("could not open the manifest at `{}`", path))?;
        toml::from_slice(&buf).context("failed to parse the manifest file")?
    } else {
        path = "Pipitor.toml";
        let buf = fs::read(path).with_context(|e| match e.kind() {
            io::ErrorKind::NotFound => "could not find `Pipitor.toml` in the current directory",
            _ => "could not open `Pipitor.toml`",
        })?;
        toml::from_slice(&buf).context("failed to parse `Pipitor.toml`")?
    };
    manifest.resolve_paths(path);
    Ok(manifest)
}

pub fn open_credentials(opt: &Opt, manifest: &Manifest) -> Fallible<Credentials> {
    if opt.manifest_path.is_none() && manifest.credentials.is_none() {
        let buf = fs::read("credentials.toml").with_context(|e| match e.kind() {
            io::ErrorKind::NotFound => "could not find `credentials.toml` in the current directory",
            _ => "could not open `credentials.toml`",
        })?;
        return Ok(toml::from_slice(&buf).context("failed to parse `credentials.toml`")?);
    }

    let path = manifest.credentials_path();
    let buf =
        fs::read(path).with_context(|_| format!("could not open the credentials at {}", path))?;
    Ok(toml::from_slice(&buf).context("failed to parse the credentials file")?)
}

cfg_if::cfg_if! {
    if #[cfg(feature = "rustls")] {
        use hyper::client::HttpConnector;
        use hyper_rustls::HttpsConnector;

        pub fn https_connector() -> failure::Fallible<HttpsConnector<HttpConnector>> {
            let mut h = HttpConnector::new();
            h.enforce_http(false);

            let mut c = rustls_pkg::ClientConfig::new();
            c.root_store
                .add_server_trust_anchors(&webpki_roots::TLS_SERVER_ROOTS);
            c.alpn_protocols.push(b"h2".to_vec());
            c.ct_logs = Some(&ct_logs::LOGS);

            Ok(HttpsConnector::from((h, c)))
        }
    } else if #[cfg(feature = "native-tls")] {
        use hyper::client::HttpConnector;
        use hyper_tls::HttpsConnector;

        pub fn https_connector() -> failure::Fallible<HttpsConnector<HttpConnector>> {
            Ok(HttpsConnector::new().context("failed to initialize TLS client")?)
        }
    } else {
        compile_error!("Either `native-tls` or `rustls` feature is required");

        pub fn https_connector() -> failure::Fallible<hyper::client::HttpConnector> {
            unimplemented!();
        }
    }
}

pub use imp::*;

#[cfg(unix)]
mod imp {
    use std::io;
    use std::path::Path;

    use futures::{Future, Stream, TryStreamExt};
    use tokio::io::{AsyncReadExt, AsyncWrite};
    use tokio_net::signal::ctrl_c;
    use tokio_net::signal::unix::{signal, SignalKind};
    use tokio_net::uds::UnixListener;

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
        Ok(UnixListener::bind(path)?.incoming().and_then(|mut a| {
            async move {
                let mut buf = Vec::new();
                a.read_to_end(&mut buf).await?;
                Ok((buf, a))
            }
        }))
    }

    pub fn quit_signal() -> io::Result<impl Future<Output = ()>> {
        let (int, term) = (ctrl_c()?, signal(SignalKind::terminate())?);
        Ok(super::merge_select(super::first(int), super::first(term)))
    }
}

#[cfg(windows)]
mod imp {
    use std::io;
    use std::path::Path;

    use futures::stream;
    use futures::{Future, Stream};
    use tokio::io::AsyncWrite;
    use tokio_net::signal::{ctrl_c, windows::ctrl_break};

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

    pub fn quit_signal() -> io::Result<impl Future<Output = ()>> {
        let (cc, cb) = (ctrl_c()?, ctrl_break()?);
        Ok(super::merge_select(super::first(cc), super::first(cb)))
    }
}

fn first<S: Stream<Item = ()> + Unpin>(s: S) -> impl Future<Output = ()> {
    s.into_future().map(|(opt, _)| opt.unwrap())
}

fn merge_select<A, B>(a: A, b: B) -> impl Future<Output = A::Output>
where
    A: Future + Unpin,
    B: Future<Output = A::Output> + Unpin,
{
    future::select(a, b).map(|either| either.factor_first().0)
}
