#[cfg(unix)]
mod imp {
    use std::path::Path;

    use anyhow::Context;
    use tokio::io::{AsyncRead, AsyncWrite};
    use tokio::net::UnixStream;

    pub async fn connect(path: &Path) -> anyhow::Result<impl AsyncRead + AsyncWrite> {
        UnixStream::connect(&path)
            .await
            .with_context(|| format!("failed to open the IPC socket at {:?}", path))
    }
}

#[cfg(not(unix))]
mod imp {
    use std::path::Path;
    use tokio::io::{AsyncRead, AsyncWrite};

    pub async fn connect(_path: &Path) -> anyhow::Result<impl AsyncRead + AsyncWrite> {
        Err::<crate::common::ipc::Stream, _>(anyhow::anyhow!(
            "`pipitor ctl` is not supported on your platform"
        ))
    }
}

use std::fs;

use anyhow::Context;
use structopt::StructOpt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::common::{ipc, ipc_path};

use imp::*;

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

pub async fn main(opt: &crate::Opt, subopt: Opt) -> anyhow::Result<()> {
    let manifest_path = opt
        .search_manifest(|path| fs::metadata(path))
        .context("unable to access the manifest")?
        .1;
    let ipc_path = ipc_path(&manifest_path);
    let mut ipc = connect(&ipc_path).await?;

    let req = match subopt.cmd {
        Cmd::Reload => ipc::Request::Reload {},
        Cmd::Shutdown => ipc::Request::Shutdown {},
    };
    let req = json::to_vec(&req).unwrap();

    ipc.write_all(&req).await?;
    ipc.flush().await?;
    ipc.shutdown().await?;

    let mut res = Vec::new();
    ipc.read_to_end(&mut res).await?;
    let res = json::from_slice::<ipc::Response>(&res)?;

    if let Some(msg) = res.result()?.message {
        println!("{}", msg);
    }

    Ok(())
}
