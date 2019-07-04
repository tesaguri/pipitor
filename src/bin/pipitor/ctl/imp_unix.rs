use std::net::Shutdown;

use failure::{Fallible, ResultExt};
use futures::compat::{AsyncWrite01CompatExt, Future01CompatExt};
use futures::{AsyncReadExt, AsyncWriteExt};
use tokio_uds::UnixStream;

use crate::common::{ipc_path, IpcRequest, IpcResponse};

use super::*;

pub async fn main(opt: &crate::Opt, subopt: Opt) -> Fallible<()> {
    let manifest_path = opt.manifest_path();
    let ipc_path = ipc_path(&manifest_path);
    let ipc = UnixStream::connect(&ipc_path).compat();

    let req = match subopt.cmd {
        Cmd::Reload => IpcRequest::Reload {},
        Cmd::Shutdown => IpcRequest::Shutdown {},
    };
    let req = json::to_vec(&req).unwrap();

    let mut ipc = ipc
        .await
        .with_context(|_| format!("failed to open the IPC socket at {:?}", ipc_path))?
        .compat();

    ipc.write_all(&req).await?;
    ipc.flush().await?;
    ipc.get_ref().shutdown(Shutdown::Write)?;

    let mut res = Vec::new();
    ipc.read_to_end(&mut res).await?;
    let res = json::from_slice::<IpcResponse>(&res)?;

    if let Some(msg) = res.result()?.message {
        println!("{}", msg);
    }

    Ok(())
}