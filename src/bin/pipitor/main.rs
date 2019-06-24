#![feature(async_await)]
#![recursion_limit = "512"]

#[macro_use]
extern crate diesel_migrations;
#[macro_use]
extern crate log;

embed_migrations!();

mod common;
mod migration;
mod run;
mod setup;
mod twitter_list_sync;
mod twitter_login;

use std::process;

use common::DisplayFailChain;
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
    #[structopt(name = "migration", about = "Run database migrations")]
    Migration(migration::Opt),
    #[structopt(name = "run", about = "Start running the bot")]
    Run(run::Opt),
    #[structopt(name = "setup", about = "")]
    Setup(setup::Opt),
    #[structopt(name = "twitter-list-sync", about = "")]
    TwitterListSync(twitter_list_sync::Opt),
    #[structopt(name = "twitter-login", about = "")]
    TwitterLogin(twitter_login::Opt),
}

#[runtime::main(runtime_tokio::Tokio)]
async fn main() {
    env_logger::init();

    if let Err(e) = run().await {
        error!("{}", DisplayFailChain(&e));
        info!("exiting abnormally");
        process::exit(1);
    }
}

async fn run() -> Fallible<()> {
    let Args { opt, cmd } = Args::from_args();

    match cmd {
        Cmd::Migration(subopt) => migration::main(&opt, subopt),
        Cmd::Run(subopt) => run::main(&opt, subopt).await,
        Cmd::Setup(subopt) => setup::main(&opt, subopt).await,
        Cmd::TwitterListSync(subopt) => twitter_list_sync::main(&opt, subopt).await,
        Cmd::TwitterLogin(subopt) => twitter_login::main(&opt, subopt).await,
    }
}
