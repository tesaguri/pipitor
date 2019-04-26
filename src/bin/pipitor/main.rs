#![feature(async_await, await_macro, futures_api)]
#![recursion_limit = "128"]

mod common;
mod run;
mod twitter_list_sync;
mod twitter_login;

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
    #[structopt(name = "twitter-list-sync", about = "")]
    TwitterListSync(twitter_list_sync::Opt),
    #[structopt(name = "twitter-login", about = "")]
    TwitterLogin(twitter_login::Opt),
}

#[runtime::main(runtime_tokio::Tokio)]
async fn main() -> Fallible<()> {
    env_logger::init();

    let Args { opt, cmd } = Args::from_args();

    match cmd {
        Cmd::Run(subopt) => await!(run::main(opt, subopt)),
        Cmd::TwitterListSync(subopt) => await!(twitter_list_sync::main(opt, subopt)),
        Cmd::TwitterLogin(subopt) => await!(twitter_login::main(opt, subopt)),
    }
}
