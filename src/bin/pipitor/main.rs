#![feature(async_await, await_macro, futures_api)]
#![recursion_limit = "128"]

mod common;
mod run;

use failure::Fallible;
use structopt::StructOpt;

#[derive(StructOpt)]
#[structopt(author = "")]
struct Args {
    #[structopt(flatten)]
    opt: Opt,
    #[structopt(subcommand)]
    cmd: Cmd,
}

#[derive(StructOpt)]
pub struct Opt {
    #[structopt(long = "manifest-path", help = "Path to Pipitor.toml")]
    manifest_path: Option<String>,
}

#[derive(StructOpt)]
enum Cmd {
    #[structopt(name = "run", about = "Start running the bot")]
    Run(run::Opt),
}

#[runtime::main(runtime_tokio::Tokio)]
async fn main() -> Fallible<()> {
    env_logger::init();

    let Args { opt, cmd } = Args::from_args();

    match cmd {
        Cmd::Run(subopt) => await!(run::main(opt, subopt)),
    }
}
