use futures::Future;

use super::*;

pub fn main(_: &crate::Opt, _: Opt) -> impl Future<Output = anyhow::Result<()>> {
    futures::future::err(anyhow::anyhow!(
        "`pipitor ctl` is not supported on your platform"
    ))
}
