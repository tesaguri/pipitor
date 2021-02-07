pub mod ipc;

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use futures::future::{Future, FutureExt};
use hyper::client::{connect::Connect, Client};
use pipitor::{private::util, Manifest};

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

cfg_if::cfg_if! {
    if #[cfg(feature = "rustls")] {
        use hyper::client::HttpConnector;
        use hyper_rustls::HttpsConnector;

        pub fn https_connector() -> HttpsConnector<HttpConnector> {
            HttpsConnector::with_native_roots()
        }
    } else if #[cfg(feature = "native-tls")] {
        use std::io::IoSlice;
        use std::pin::Pin;
        use std::task::{Context, Poll};

        use hyper::client::connect::{Connected, Connection};
        use hyper::client::HttpConnector;
        use hyper_tls::{HttpsConnector, MaybeHttpsStream};
        use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
        use tower::ServiceExt;

        pub fn https_connector() -> impl Connect + Clone + Send + Sync {
            let mut h = HttpConnector::new();
            h.enforce_http(false);
            let c = native_tls_pkg::TlsConnector::builder()
                .request_alpns(&["h2", "http/1.1"])
                .build()
                .unwrap();
            HttpsConnector::from((h, c.into())).map_response(H2Stream)
        }

        struct H2Stream<T>(MaybeHttpsStream<T>);

        impl<T: AsyncRead + AsyncWrite + Unpin> AsyncRead for H2Stream<T> {
            #[inline]
            fn poll_read(
                mut self: Pin<&mut Self>,
                cx: &mut Context,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_read(cx, buf)
            }
        }

        impl<T: AsyncWrite + AsyncRead + Unpin> AsyncWrite for H2Stream<T> {
            fn poll_write(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.0).poll_write(cx, buf)
            }

            fn poll_write_vectored(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                bufs: &[IoSlice<'_>],
            ) -> Poll<io::Result<usize>> {
                Pin::new(&mut self.0).poll_write_vectored(cx, bufs)
            }

            fn is_write_vectored(&self) -> bool {
                self.0.is_write_vectored()
            }

            fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_flush(cx)
            }

            fn poll_shutdown(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<io::Result<()>> {
                Pin::new(&mut self.0).poll_shutdown(cx)
            }
        }

        impl<T: AsyncRead + AsyncWrite + Connection + Unpin> Connection for H2Stream<T> {
            fn connected(&self) -> Connected {
                match self.0 {
                    MaybeHttpsStream::Http(ref s) => s.connected(),
                    MaybeHttpsStream::Https(ref s) => {
                        let s = s.get_ref();
                        if s.negotiated_alpn().ok().flatten().as_deref() == Some(b"h2") {
                            s.get_ref().get_ref().connected().negotiated_h2()
                        } else {
                            s.get_ref().get_ref().connected()
                        }
                    }
                }
            }
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

    let mut int = signal(SignalKind::interrupt())?;
    let int = async move { int.recv().await.unwrap() };
    let mut term = signal(SignalKind::terminate())?;
    let term = async move { term.recv().await.unwrap() };
    Ok(merge_select(int, term))
}

#[cfg(windows)]
pub fn quit_signal() -> io::Result<impl Future<Output = ()>> {
    use tokio::signal::{ctrl_c, windows::ctrl_break};

    let cc = async { ctrl_c().await.unwrap() };
    let mut cb = ctrl_break()?;
    let cb = async move { cb.recv().await.unwrap() };
    Ok(merge_select(cc, cb))
}

fn merge_select<A, B>(a: A, b: B) -> impl Future<Output = A::Output>
where
    A: Future,
    B: Future<Output = A::Output>,
{
    util::first(a, b).map(|either| either.into_inner())
}
