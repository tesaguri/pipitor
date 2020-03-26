table! {
    last_tweet (id) {
        id -> BigInt,
        status_id -> BigInt,
    }
}

table! {
    tweets (id) {
        id -> BigInt,
        text -> Text,
        user_id -> BigInt,
        in_reply_to_status_id -> Nullable<BigInt>,
        quoted_status_id -> Nullable<BigInt>,
    }
}

table! {
    twitter_tokens (id) {
        id -> BigInt,
        access_token -> Text,
        access_token_secret -> Text,
    }
}

table! {
    websub_active_subscriptions (id) {
        id -> BigInt,
        expires_at -> BigInt,
    }
}

table! {
    websub_pending_subscriptions (id) {
        id -> BigInt,
        created_at -> BigInt,
    }
}

table! {
    websub_renewing_subscriptions (old, new) {
        old -> BigInt,
        new -> BigInt,
    }
}

table! {
    websub_subscriptions (id) {
        id -> BigInt,
        hub -> Text,
        topic -> Text,
        secret -> Text,
    }
}

joinable!(websub_active_subscriptions -> websub_subscriptions (id));
joinable!(websub_pending_subscriptions -> websub_subscriptions (id));
joinable!(websub_renewing_subscriptions -> websub_active_subscriptions (old));
joinable!(websub_renewing_subscriptions -> websub_pending_subscriptions (new));

allow_tables_to_appear_in_same_query!(
    last_tweet,
    tweets,
    twitter_tokens,
    websub_active_subscriptions,
    websub_pending_subscriptions,
    websub_renewing_subscriptions,
    websub_subscriptions,
);
