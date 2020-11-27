use diesel::dsl::sql_query;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, CustomizeConnection, Pool};
use diesel::SqliteConnection;

#[derive(Debug)]
struct ConnectionCustomizer;

impl CustomizeConnection<SqliteConnection, diesel::r2d2::Error> for ConnectionCustomizer {
    fn on_acquire(&self, conn: &mut SqliteConnection) -> Result<(), diesel::r2d2::Error> {
        pragma_foreign_keys_on(conn).map_err(diesel::r2d2::Error::QueryError)
    }
}

pub fn new_pool(
    manager: ConnectionManager<SqliteConnection>,
) -> Result<Pool<ConnectionManager<SqliteConnection>>, diesel::r2d2::PoolError> {
    Pool::builder()
        .connection_customizer(Box::new(ConnectionCustomizer))
        .build(manager)
}

fn pragma_foreign_keys_on(conn: &SqliteConnection) -> QueryResult<()> {
    sql_query("PRAGMA foreign_keys = ON")
        .execute(conn)
        .map(|_| ())
}
