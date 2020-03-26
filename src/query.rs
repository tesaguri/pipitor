use diesel::dsl::*;
use diesel::prelude::*;

use crate::schema::*;

pub fn renewing_subs(
) -> Select<websub_renewing_subscriptions::table, websub_renewing_subscriptions::old> {
    websub_renewing_subscriptions::table.select(websub_renewing_subscriptions::old)
}
