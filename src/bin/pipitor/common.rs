pub mod ipc;

use std::ffi::{OsStr, OsString};
use std::fs::{self};
use std::io;
use std::marker::Unpin;
use std::path::{Path, PathBuf};

use anyhow::Context;
use futures::future::{self, Future, FutureExt};
use futures::{Stream, StreamExt};
use hyper::client::{connect::Connect, Client};
use pipitor::{Credentials, Manifest};

#[derive(Clone, structopt::StructOpt)]
pub struct Opt {
    #[structopt(long = "manifest-path", help = "Path to the manifest file")]
    manifest_path: Option<String>,
}

pub struct RmGuard<P: AsRef<Path>>(pub P);

const TOML: &str = "Pipitor.toml";
const JSON: &str = "Pipitor.json";
#[cfg(feature = "dhall")]
const DHALL: &str = "Pipitor.dhall";

impl Opt {
    pub fn search_manifest<F, T>(&self, f: F) -> io::Result<(T, &str)>
    where
        F: Fn(&Path) -> io::Result<T>,
    {
        if let Some(ref path) = self.manifest_path {
            f(path.as_ref()).map(|t| (t, &**path))
        } else {
            match f(TOML.as_ref()) {
                Ok(t) => Ok((t, TOML)),
                Err(e) if e.kind() == io::ErrorKind::NotFound => match f(JSON.as_ref()) {
                    Ok(t) => Ok((t, JSON)),
                    #[cfg(feature = "dhall")]
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {
                        f(DHALL.as_ref()).map(|t| (t, DHALL))
                    }
                    Err(e) => Err(e),
                },
                Err(e) => Err(e),
            }
        }
    }

    pub fn open_manifest(&self) -> anyhow::Result<Manifest> {
        // TODO: Open the file in `search_manifest` to avoid TOCTOU race condition
        // if and when `serde_dhall` exposes the underlying `io::Error`
        // or allows specifying the base directory of imports.
        let path = self
            .search_manifest(|path| fs::metadata(path))
            .context("unable to access the manifest")?
            .1;
        let mut manifest: Manifest = match Path::new(path).extension().and_then(OsStr::to_str) {
            Some("json") => {
                // `from_slice` is faster than `from_reader`.
                // See <https://github.com/serde-rs/json/issues/160>.
                let buf = fs::read(path).context("could not open the manifest")?;
                json::from_slice(&buf).context("failed to parse the manifest")?
            }
            #[cfg(feature = "dhall")]
            Some("dhall") => serde_dhall::from_file(path)
                .parse()
                .context("failed to parse the manifest")?,
            _ => {
                let buf = fs::read(path).context("could not open the manifest")?;
                toml::from_slice(&buf).context("failed to parse the manifest")?
            }
        };
        manifest.resolve_paths(path);
        Ok(manifest)
    }
}

impl<P: AsRef<Path>> Drop for RmGuard<P> {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub fn client() -> Client<impl Connect + Clone + Send + Sync> {
    Client::builder().build(https_connector())
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

pub fn open_credentials(opt: &Opt, manifest: &Manifest) -> anyhow::Result<Credentials> {
    if opt.manifest_path.is_none() && manifest.credentials.is_none() {
        let buf = fs::read("credentials.toml").context("could not open `credentials.toml`")?;
        return toml::from_slice(&buf).context("failed to parse `credentials.toml`");
    }

    let path = manifest.credentials_path();
    let buf =
        fs::read(path).with_context(|| format!("could not open the credentials at {}", path))?;
    toml::from_slice(&buf).context("failed to parse the credentials file")
}

cfg_if::cfg_if! {
    if #[cfg(feature = "rustls")] {
        use hyper::client::HttpConnector;
        use hyper_rustls::HttpsConnector;

        pub fn https_connector() -> HttpsConnector<HttpConnector> {
            let mut h = HttpConnector::new();
            h.enforce_http(false);

            let mut c = rustls_pkg::ClientConfig::new();
            c.root_store
                .add_server_trust_anchors(&webpki_roots::TLS_SERVER_ROOTS);
            c.alpn_protocols.push(b"h2".to_vec());
            c.ct_logs = Some(&ct_logs::LOGS);

            HttpsConnector::from((h, c))
        }
    } else if #[cfg(feature = "native-tls")] {
        use hyper::client::HttpConnector;
        use hyper_tls::HttpsConnector;

        pub fn https_connector() -> HttpsConnector<HttpConnector> {
            HttpsConnector::new()
        }
    } else {
        compile_error!("Either `native-tls` or `rustls` feature is required");

        pub fn https_connector() -> hyper::client::HttpConnector {
            unimplemented!();
        }
    }
}

#[cfg(unix)]
pub fn quit_signal() -> io::Result<impl Future<Output = ()>> {
    use tokio::signal::unix::{signal, SignalKind};

    let int = signal(SignalKind::interrupt())?;
    let term = signal(SignalKind::terminate())?;
    Ok(merge_select(first(int), first(term)))
}

#[cfg(windows)]
pub fn quit_signal() -> io::Result<impl Future<Output = ()>> {
    use tokio::signal::{ctrl_c, windows::ctrl_break};

    let cc = Box::pin(ctrl_c()).map(|result| result.unwrap());
    let cb = ctrl_break()?;
    Ok(merge_select(cc, first(cb)))
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
