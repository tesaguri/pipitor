use diesel::dsl::*;
use diesel::prelude::*;
use diesel::query_builder::{AstPass, QueryFragment};
use diesel::sqlite::Sqlite;

use crate::schema::*;

#[derive(QueryId)]
pub struct PragmaForeignKeysOn;

impl QueryFragment<Sqlite> for PragmaForeignKeysOn {
    fn walk_ast(&self, mut out: AstPass<Sqlite>) -> QueryResult<()> {
        out.push_sql("PRAGMA foreign_keys = ON");
        Ok(())
    }
}

impl RunQueryDsl<SqliteConnection> for PragmaForeignKeysOn {}

pub fn expires_at() -> Order<
    Select<websub_active_subscriptions::table, websub_active_subscriptions::expires_at>,
    Asc<websub_active_subscriptions::expires_at>,
> {
    websub_active_subscriptions::table
        .select(websub_active_subscriptions::expires_at)
        .order(websub_active_subscriptions::expires_at.asc())
}

pub fn pragma_foreign_keys_on() -> PragmaForeignKeysOn {
    PragmaForeignKeysOn
}

pub fn renewing_subs(
) -> Select<websub_renewing_subscriptions::table, websub_renewing_subscriptions::old> {
    websub_renewing_subscriptions::table.select(websub_renewing_subscriptions::old)
}
