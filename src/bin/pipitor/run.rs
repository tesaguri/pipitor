use failure::{Fallible, ResultExt};
use pipitor::App;

use crate::common::open_manifest;

#[derive(structopt::StructOpt)]
pub struct Opt {}

pub async fn main(opt: &crate::Opt, _subopt: Opt) -> Fallible<()> {
    let manifest = open_manifest(&opt)?;
    let mut app = await!(App::new(manifest)).context("failed to initialize the application")?;
    loop {
        match await!(&mut app) {
            Ok(()) => info!("disconnected from Twitter Streaming API"),
            Err(e) => {
                // TODO: do not retry immediately if the error is Too Many Requests or Forbidden
                error!("{}", e);
            }
        }
        await!(app.reset())?;
    }
}
