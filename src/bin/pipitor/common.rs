use std::env;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::io;

use failure::{AsFail, Fail, Fallible, ResultExt};
use pipitor::Manifest;

pub struct DisplayFailChain<'a, F>(pub &'a F);

impl<'a, F: AsFail> Display for DisplayFailChain<'a, F> {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let fail = self.0.as_fail();

        write!(f, "Error: {}", fail)?;

        for e in fail.iter_causes() {
            write!(f, "\nCaused by: {}", e)?;
        }

        if let Some(backtrace) = fail.backtrace() {
            let backtrace_enabled = match env::var_os("RUST_FAILURE_BACKTRACE") {
                Some(ref v) if v != "0" => true,
                Some(_) => false,
                _ => match env::var_os("RUST_BACKTRACE") {
                    Some(ref v) if v != "0" => true,
                    _ => false,
                },
            };
            if backtrace_enabled {
                write!(f, "\n{}", backtrace)?;
            }
        }

        Ok(())
    }
}

pub fn open_manifest(opt: &crate::Opt) -> Fallible<Manifest> {
    let manifest = if let Some(ref manifest_path) = opt.manifest_path {
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
