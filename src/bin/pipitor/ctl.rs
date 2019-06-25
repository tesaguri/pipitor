#[cfg(unix)]
#[path = "ctl/imp_unix.rs"]
mod imp;

#[cfg(not(unix))]
#[path = "ctl/imp_other.rs"]
mod imp;

pub use imp::*;

use structopt::StructOpt;

#[derive(StructOpt)]
pub struct Opt {
    #[structopt(subcommand)]
    cmd: Cmd,
}

#[derive(StructOpt)]
enum Cmd {
    #[structopt(name = "reload", about = "Reloads the manifest")]
    Reload,
    #[structopt(name = "shutdown", about = "Shuts down the bot")]
    Shutdown,
}
