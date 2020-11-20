use std::fs::File;
use std::future;

use anyhow::Context;
use fs2::FileExt;
use futures::{pin_mut, stream, FutureExt, StreamExt, TryFutureExt};
use pipitor::App;

use crate::common::*;

#[derive(structopt::StructOpt)]
pub struct Opt {}

pub fn main(opt: &crate::Opt, _subopt: Opt) -> anyhow::Result<()> {
    let manifest = opt.open_manifest()?;

    let (lock, manifest_path) = opt
        .search_manifest(|path| File::open(path))
        .context("unable to access the manifest")?;
    lock.try_lock_exclusive()
        .context("failed to acquire a file lock")?;

    let mut runtime = tokio::runtime::Runtime::new().context("failed to start a Tokio runtime")?;

    let ipc_path = ipc_path(manifest_path);
    let opt = opt.clone();
    runtime.block_on(async move {
        let (ipc, _guard) = match ipc::server(&ipc_path)
            .with_context(|| format!("failed to create an IPC socket at {:?}", ipc_path))
        {
            Ok(ipc) => (ipc.left_stream().fuse(), Some(RmGuard(ipc_path))),
            Err(e) => {
                error!("{:?}", e);
                (stream::empty().right_stream().fuse(), None)
            }
        };
        pin_mut!(ipc);

        let mut signal = quit_signal().unwrap().fuse();

        let client = client();
        let app = App::<_, _>::with_http_client(client, manifest)
            .await
            .context("failed to initialize the application")?;
        pin_mut!(app);

        info!("initialized the application");

        loop {
            let mut app_fuse = (&mut app).fuse();
            futures::select! {
                result = app_fuse => {
                    match result {
                        Ok(()) => info!("disconnected from Twitter Streaming API"),
                        Err(e) => {
                            // TODO: do not retry immediately if the error is Too Many Requests or Forbidden
                            error!("{:?}", e);
                        }
                    }
                    info!("restarting the application");
                    app.as_mut().reset().await?;
                }
                _signal_id = signal => {
                    info!("shutdown requested via console");
                    app.as_mut().shutdown().await?;
                    info!("exiting normally");
                    return Ok(());
                }
                conn = ipc.select_next_some() => {
                    let (req, write) = conn;

                    match req {
                        ipc::Request::Reload {} => {
                            info!("reloading the manifest");

                            let result = future::ready(opt.open_manifest())
                                .and_then(|manifest| {
                                    app.replace_manifest(manifest).map_err(|(e, _)| e)
                                })
                                .await;
                            if let Err(e) = result {
                                let res = ipc::Response::new(
                                    ipc::ResponseCode::InternalError,
                                    "failed to reload the manifest".to_owned(),
                                );
                                tokio::spawn(ipc::respond(res, write));
                                return Err(e);
                            }

                            tokio::spawn(ipc::respond(ipc::Response::default(), write));
                        }
                        ipc::Request::Shutdown {} => {
                            info!("shutdown requested via IPC");

                            if let Err(e) = app.shutdown().await {
                                let res = ipc::Response::new(
                                    ipc::ResponseCode::InternalError,
                                    "error occured during shutdown".to_owned(),
                                );
                                ipc::respond(res, write).await;
                                return Err(e);
                            }

                            ipc::respond(ipc::Response::default(), write).await;

                            info!("exiting normally");
                            return Ok(());
                        }
                    }
                }
            }
        }
    })
}
