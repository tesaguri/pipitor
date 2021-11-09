use diesel::prelude::*;
use diesel::r2d2::{self, ConnectionManager, CustomizeConnection, Pool};

use crate::query;

#[derive(Debug)]
struct ConnectionCustomizer;

impl CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for ConnectionCustomizer {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), diesel::r2d2::Error> {
        (|| {
            // The value of `5000` ms is taken from `rusqlite`'s default.
            // <https://github.com/diesel-rs/diesel/issues/2365#issuecomment-719467312>
            // <https://github.com/rusqlite/rusqlite/commit/05b03ae2cec9f9f630095d5c0e89682da334f4a4>
            query::pragma_busy_timeout(5000).execute(conn)?;
            query::pragma_foreign_keys_on().execute(conn)?;
            Ok(())
        })()
        .map_err(diesel::r2d2::Error::QueryError)
    }
}

pub fn new_pool(
    manager: ConnectionManager<SqliteConnection>,
) -> Result<Pool<ConnectionManager<SqliteConnection>>, diesel::r2d2::PoolError> {
    pool_with_builder(Pool::builder(), manager)
}

pub fn pool_with_builder(
    builder: r2d2::Builder<ConnectionManager<SqliteConnection>>,
    manager: ConnectionManager<SqliteConnection>,
) -> Result<Pool<ConnectionManager<SqliteConnection>>, diesel::r2d2::PoolError> {
    builder
        .connection_customizer(Box::new(ConnectionCustomizer))
        .build(manager)
}
