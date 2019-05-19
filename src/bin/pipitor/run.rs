use std::fmt::Debug;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use failure::{Fail, Fallible, ResultExt};
use fs2::FileExt;
use futures::compat::Stream01CompatExt;
use futures::future::{self, Future, FutureExt};
use futures::join;
use futures::stream::StreamExt;
use futures01::Stream as Stream01;
use pipitor::App;

use crate::common::{open_manifest, DisplayFailChain};

#[derive(structopt::StructOpt)]
pub struct Opt {
    twitter_dump: Option<PathBuf>,
}

pub async fn main(opt: &crate::Opt, subopt: Opt) -> Fallible<()> {
    let manifest = open_manifest(opt)?;

    let lock = File::open(
        opt.manifest_path
            .as_ref()
            .map(|s| &**s)
            .unwrap_or("Pipitor.toml"),
    )?;
    match lock.try_lock_exclusive() {
        Ok(()) => {}
        Err(e) => return Err(e.context("failed to acquire a file lock").into()),
    }

    let (signal, app) = (signal::quit(), App::new(manifest));
    let (signal, app) = join!(signal, app);
    let mut signal = signal.unwrap().fuse();
    let mut app = app.context("failed to initialize the application")?;

    if let Some(ref path) = subopt.twitter_dump {
        let f = OpenOptions::new()
            .write(true)
            .create(true)
            .open(path)
            .context(format!(
                "failed to open {:?}",
                subopt.twitter_dump.as_ref().unwrap(),
            ))?;
        app.set_twitter_dump(f).unwrap();
    };

    info!("initialized the application");

    loop {
        let mut app_fuse = (&mut app).fuse();
        futures::select! {
            result = app_fuse => {
                match result {
                    Ok(()) => info!("disconnected from Twitter Streaming API"),
                    Err(e) => {
                        // TODO: do not retry immediately if the error is Too Many Requests or Forbidden
                        error!("{}", DisplayFailChain(&e));
                    }
                }
                info!("restarting the application");
                app.reset().await?;
            }
            _signal_id = signal => {
                info!("shutdown requested");
                app.shutdown().await?;
                info!("exiting normally");
                return Ok(());
            }
        }
    }
}

#[cfg(unix)]
mod signal {
    use std::io;

    use futures::compat::Future01CompatExt;
    use futures::future::Future;
    use tokio_signal::unix::{
        libc::{SIGINT, SIGTERM},
        Signal,
    };

    pub async fn quit() -> io::Result<impl Future<Output = ()>> {
        let (int, term) = (Signal::new(SIGINT).compat(), Signal::new(SIGTERM).compat());
        let (int, term) = futures::try_join!(int, term)?;
        Ok(super::merge_select(super::first(int), super::first(term)))
    }
}

#[cfg(windows)]
mod signal {
    use std::io;

    use futures::compat::Future01CompatExt;
    use futures::future::Future;
    use tokio_signal::windows::Event;

    pub async fn quit() -> io::Result<impl Future<Output = ()>> {
        let (cc, cb) = (Event::ctrl_c().compat(), Event::ctrl_break().compat());
        let (cc, cb) = futures::try_join!(cc, cb)?;
        Ok(super::merge_select(super::first(cc), super::first(cb)))
    }
}

fn first<S: Stream01>(s: S) -> impl Future<Output = ()>
where
    S::Error: Debug,
{
    s.compat().into_future().map(|(r, _)| {
        r.unwrap().unwrap();
    })
}

fn merge_select<A, B>(a: A, b: B) -> impl Future<Output = A::Output>
where
    A: Future + Unpin,
    B: Future<Output = A::Output> + Unpin,
{
    future::select(a, b).map(|either| either.factor_first().0)
}
