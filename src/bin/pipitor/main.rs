#![recursion_limit = "512"]
// <https://github.com/rust-lang/rust-clippy/issues/5594>
#![allow(clippy::redundant_closure)]

#[macro_use]
extern crate log;

mod common;
mod ctl;
mod migration;
mod run;
mod setup;
mod twitter_list_sync;
mod twitter_login;
mod websub;

use std::process;

use structopt::StructOpt;

use common::Opt;

#[derive(StructOpt)]
struct Args {
    #[structopt(flatten)]
    opt: Opt,
    #[structopt(subcommand)]
    cmd: Cmd,
}

#[derive(StructOpt)]
enum Cmd {
    #[structopt(name = "ctl", about = "Controls a currently running Pipitor instance")]
    Ctl(ctl::Opt),
    #[structopt(name = "migration", about = "Run database migrations")]
    Migration(migration::Opt),
    #[structopt(name = "run", about = "Start running the bot")]
    Run(run::Opt),
    #[structopt(name = "setup")]
    Setup(setup::Opt),
    #[structopt(name = "twitter-list-sync")]
    TwitterListSync(twitter_list_sync::Opt),
    #[structopt(name = "twitter-login")]
    TwitterLogin(twitter_login::Opt),
    #[structopt(name = "websub")]
    WebSub(websub::Opt),
}

fn main() {
    env_logger::init();

    if let Err(e) = run() {
        error!("{:?}", e);
        info!("exiting abnormally");
        process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let Args { opt, cmd } = Args::from_args();

    match cmd {
        Cmd::Migration(subopt) => migration::main(&opt, subopt),
        Cmd::Run(subopt) => run::main(&opt, subopt),
        cmd => tokio::runtime::Runtime::new()?.block_on(async move {
            match cmd {
                Cmd::Ctl(subopt) => ctl::main(&opt, subopt).await,
                Cmd::Setup(subopt) => setup::main(&opt, subopt).await,
                Cmd::TwitterListSync(subopt) => twitter_list_sync::main(&opt, subopt).await,
                Cmd::TwitterLogin(subopt) => twitter_login::main(&opt, subopt).await,
                Cmd::WebSub(subopt) => websub::main(&opt, subopt).await,
                Cmd::Migration(_) | Cmd::Run(_) => unreachable!(),
            }
        }),
    }
}
