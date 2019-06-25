use failure::{err_msg, Fallible};

use super::*;

pub async fn main(opt: &crate::Opt, subopt: Opt) -> Fallible<()> {
    Err(err_msg("`pipitor ctl` is not supported on your platform"))
}
