use diesel::prelude::*;
use diesel::r2d2::{self, ConnectionManager, CustomizeConnection, Pool};

use crate::query;

#[derive(Debug)]
struct ConnectionCustomizer;

impl CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for ConnectionCustomizer {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), diesel::r2d2::Error> {
        query::pragma_foreign_keys_on()
            .execute(conn)
            .map(|_| ())
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
