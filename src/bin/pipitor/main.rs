#![feature(async_await, await_macro, futures_api)]

use std::fs;
use std::io;

use failure::{Fail, Fallible, ResultExt};
use pipitor::{App, Manifest};

#[runtime::main(runtime_tokio::Tokio)]
async fn main() -> Fallible<()> {
    env_logger::init();

    let manifest = match fs::read("Pipitor.toml") {
        Ok(f) => f,
        Err(e) => {
            return if e.kind() == io::ErrorKind::NotFound {
                Err(failure::err_msg(
                    "could not find `Pipitor.toml` in the current directory",
                ))
            } else {
                Err(e.context("could not open `Pipitor.toml`").into())
            };
        }
    };

    let manifest: Manifest =
        toml::from_slice(&manifest).context("failed to parse `Pipitor.toml`")?;

    let app = await!(App::new(manifest)).context("failed to initialize the application")?;

    await!(app)
}
