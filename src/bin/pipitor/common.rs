use std::fs;
use std::io;

use failure::{Fail, Fallible, ResultExt};
use pipitor::Manifest;

pub fn open_manifest(manifest_path: Option<&str>) -> Fallible<Manifest> {
    let manifest = if let Some(ref manifest_path) = manifest_path {
        let manifest = match fs::read(manifest_path) {
            Ok(f) => f,
            Err(e) => {
                return Err(e
                    .context(format!(
                        "could not open the manifest at `{}`",
                        manifest_path,
                    ))
                    .into())
            }
        };
        toml::from_slice(&manifest).context("failed to parse the manifest file")?
    } else {
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
        toml::from_slice(&manifest).context("failed to parse `Pipitor.toml`")?
    };

    Ok(manifest)
}
