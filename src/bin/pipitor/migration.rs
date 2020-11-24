use std::io;

use anyhow::Context;
use diesel::connection::Connection;
use diesel::SqliteConnection;

#[derive(Default, structopt::StructOpt)]
pub struct Opt {}

pub fn main(opt: &crate::Opt, _subopt: Opt) -> anyhow::Result<()> {
    let manifest = opt.open_manifest()?;

    let conn = SqliteConnection::establish(&manifest.database_url())
        .context("failed to connect to the database")?;

    let stdout = io::stdout();
    pipitor::migrations::run_with_output(&conn, &mut stdout.lock()).context("migration failed")?;

    Ok(())
}
