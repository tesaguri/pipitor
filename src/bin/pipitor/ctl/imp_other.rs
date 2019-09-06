use failure::{err_msg, Fallible};
use futures::Future;

use super::*;

pub fn main(_: &crate::Opt, _: Opt) -> impl Future<Output = Fallible<()>> {
    futures::future::err(err_msg("`pipitor ctl` is not supported on your platform"))
}
