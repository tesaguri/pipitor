use std::fs::File;

use failure::{Fail, Fallible, ResultExt};
use fs2::FileExt;
use futures::compat::Stream01CompatExt;
use futures::future::FutureExt;
use futures::stream::StreamExt;
use pipitor::App;
use signal_hook::iterator::Signals;

use crate::common::open_manifest;

#[derive(structopt::StructOpt)]
pub struct Opt {}

pub async fn main(opt: &crate::Opt, _subopt: Opt) -> Fallible<()> {
    let manifest = open_manifest(opt)?;

    let lock = File::open(
        opt.manifest_path
            .as_ref()
            .map(|s| &**s)
            .unwrap_or("Pipitor.toml"),
    )?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(e) => return Err(e.context("failed to acquire a file lock").into()),
    }

    let mut signals = Signals::new(&[signal_hook::SIGTERM, signal_hook::SIGINT])
        .unwrap()
        .into_async()
        .unwrap()
        .compat();
    let mut signal = signals.next().fuse();

    let mut app = App::new(manifest)
        .await
        .context("failed to initialize the application")?;

    loop {
        let mut app_fuse = (&mut app).fuse();
        futures::select! {
            result = app_fuse => {
                match result {
                    Ok(()) => info!("disconnected from Twitter Streaming API"),
                    Err(e) => {
                        // TODO: do not retry immediately if the error is Too Many Requests or Forbidden
                        error!("{}", e);
                    }
                }
                app.reset().await?;
            }
            option = signal => {
                let _signal_id = option.unwrap_or_else(|| unreachable!())?;
                info!("shutdown requested");
                app.shutdown().await?;
                return Ok(());
            }
        }
    }
}
