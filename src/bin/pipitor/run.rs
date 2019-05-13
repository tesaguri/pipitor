use std::fs::File;

use failure::{Fail, Fallible, ResultExt};
use fs2::FileExt;
use pipitor::App;

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

    let mut app = App::new(manifest)
        .await
        .context("failed to initialize the application")?;
    loop {
        match (&mut app).await {
            Ok(()) => info!("disconnected from Twitter Streaming API"),
            Err(e) => {
                // TODO: do not retry immediately if the error is Too Many Requests or Forbidden
                error!("{}", e);
            }
        }
        app.reset().await?;
    }
}
