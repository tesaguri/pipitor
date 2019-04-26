use failure::{Fallible, ResultExt};
use pipitor::App;

use crate::common::open_manifest;

#[derive(structopt::StructOpt)]
pub struct Opt {}

pub async fn main(opt: crate::Opt, _subopt: Opt) -> Fallible<()> {
    let manifest = open_manifest(&opt)?;
    let app = await!(App::new(manifest)).context("failed to initialize the application")?;
    await!(app)
}
