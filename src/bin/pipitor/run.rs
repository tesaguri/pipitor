use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

use failure::{Fail, Fallible, ResultExt};
use fs2::FileExt;
use futures::{future, stream};
use futures::{join, AsyncWriteExt, FutureExt, StreamExt, TryFutureExt};
use pipitor::App;

use crate::common::*;

#[derive(structopt::StructOpt)]
pub struct Opt {
    #[structopt(
        long = "twitter-dump",
        help = "File to dump Twitter Streaming API output"
    )]
    twitter_dump: Option<PathBuf>,
}

pub async fn main(opt: &crate::Opt, subopt: Opt) -> Fallible<()> {
    let manifest = open_manifest(opt)?;

    let manifest_path: &Path = opt
        .manifest_path
        .as_ref()
        .map(|s| &**s)
        .unwrap_or("Pipitor.toml")
        .as_ref();
    let lock = File::open(manifest_path)?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(e) => return Err(e.context("failed to acquire a file lock").into()),
    }

    let (signal, app) = (quit_signal(), App::new(manifest));

    let ipc_path = ipc_path(manifest_path);
    let mut ipc = ipc_server(&ipc_path)
        .with_context(|_| format!("failed to create an IPC socket at {:?}", ipc_path))
        .map(|ipc| ipc.left_stream())
        .unwrap_or_else(|e| {
            error!("{}", DisplayFailChain(&e));
            stream::empty().right_stream()
        })
        .fuse();

    let (signal, app) = join!(signal, app);
    let mut signal = signal.unwrap().fuse();
    let mut app = app.context("failed to initialize the application")?;

    if let Some(ref path) = subopt.twitter_dump {
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .open(path)
            .context(format!(
                "failed to open {:?}",
                subopt.twitter_dump.as_ref().unwrap(),
            ))?;
        app.set_twitter_dump(f).unwrap();
    };

    info!("initialized the application");

    loop {
        let mut app_fuse = (&mut app).fuse();
        futures::select! {
            result = app_fuse => {
                match result {
                    Ok(()) => info!("disconnected from Twitter Streaming API"),
                    Err(e) => {
                        // TODO: do not retry immediately if the error is Too Many Requests or Forbidden
                        error!("{}", DisplayFailChain(&e));
                    }
                }
                info!("restarting the application");
                app.reset().await?;
            }
            _signal_id = signal => {
                info!("shutdown requested via console");
                app.shutdown().await?;
                info!("exiting normally");
                return Ok(());
            }
            result = ipc.select_next_some() => {
                let (req, mut write) = match result {
                    Ok(x) => x,
                    Err(e) => {
                        warn!("error in the IPC socket: {}", e);
                        continue;
                    }
                };

                macro_rules! respond {
                    ($body:expr) => {
                        async {
                            write.write_all($body).await?;
                            write.flush().await?;
                            Ok(()) as Fallible<()>
                        }
                            .map_err(|e| warn!("failed to write IPC response: {}", e))
                    };
                }

                let req: IpcRequest = match json::from_slice(&req) {
                    Ok(x) => x,
                    Err(e) => {
                        info!(
                            "unrecognized IPC request: {:?}",
                            String::from_utf8_lossy(&req),
                        );
                        let res = json::to_vec(&IpcResponse::new(
                            IpcResponseCode::RequestUnrecognized,
                            "request unrecognized".to_owned(),
                        ))
                        .unwrap();
                        let _ = respond!(&res).await;
                        continue;
                    }
                };

                // Declaring IPC response body here to make borrowck happy
                let mut res = Vec::new();
                match req {
                    IpcRequest::Reload {} => {
                        info!("reloading the manifest");
                        future::ready(open_manifest(opt))
                            .and_then(|manifest| app.replace_manifest(manifest).map_err(|(e, _)| e))
                            .or_else(|e| {
                                res = json::to_vec(&IpcResponse::new(
                                    IpcResponseCode::InternalError,
                                    "failed to reload the manifest".to_owned(),
                                ))
                                .unwrap();
                                respond!(&res).then(|_| future::err(e))
                            })
                            .await?;

                        let res = json::to_vec(&IpcResponse::default()).unwrap();
                        let _ = respond!(&res).await;
                    }
                    IpcRequest::Shutdown {} => {
                        info!("shutdown requested via IPC");

                        app.shutdown()
                            .or_else(|e| {
                                res = json::to_vec(&IpcResponse::new(
                                    IpcResponseCode::InternalError,
                                    "error occured during shutdown".to_owned(),
                                ))
                                .unwrap();
                                respond!(&res).then(|_| future::err(e))
                            })
                            .await?;

                        let res = json::to_vec(&IpcResponse::default()).unwrap();
                        let _ = respond!(&res).await;

                        info!("exiting normally");
                        return Ok(());
                    }
                }
            }
        }
    }
}
