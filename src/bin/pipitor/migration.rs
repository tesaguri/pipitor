use std::io;

use diesel::connection::Connection;
use diesel::SqliteConnection;
use failure::Fallible;

use crate::common::open_manifest;

#[derive(Default, structopt::StructOpt)]
pub struct Opt {}

pub fn main(opt: &crate::Opt, _subopt: Opt) -> Fallible<()> {
    let manifest = open_manifest(opt)?;

    let conn = SqliteConnection::establish(&manifest.database_url())?;

    let stdout = io::stdout();
    crate::embedded_migrations::run_with_output(&conn, &mut stdout.lock())?;

    Ok(())
}
