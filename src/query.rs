use diesel::dsl::*;
use diesel::prelude::*;

use crate::schema::*;

pub fn expires_at() -> Order<
    Select<websub_active_subscriptions::table, websub_active_subscriptions::expires_at>,
    Asc<websub_active_subscriptions::expires_at>,
> {
    websub_active_subscriptions::table
        .select(websub_active_subscriptions::expires_at)
        .order(websub_active_subscriptions::expires_at.asc())
}

pub fn renewing_subs(
) -> Select<websub_renewing_subscriptions::table, websub_renewing_subscriptions::old> {
    websub_renewing_subscriptions::table.select(websub_renewing_subscriptions::old)
}
