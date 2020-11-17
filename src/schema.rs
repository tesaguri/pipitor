table! {
    entries (topic, id) {
        topic -> Text,
        id -> Nullable<Text>,
        link -> Text,
        title -> Nullable<Text>,
        summary -> Nullable<Text>,
        content -> Nullable<Text>,
        updated -> Nullable<BigInt>,
    }
}

table! {
    last_tweet (id) {
        id -> BigInt,
        status_id -> BigInt,
    }
}

table! {
    ongoing_retweets (id, user) {
        id -> BigInt,
        user -> BigInt,
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

joinable!(ongoing_retweets -> tweets (id));
joinable!(ongoing_retweets -> twitter_tokens (user));
joinable!(websub_active_subscriptions -> websub_subscriptions (id));
joinable!(websub_pending_subscriptions -> websub_subscriptions (id));
joinable!(websub_renewing_subscriptions -> websub_active_subscriptions (old));
joinable!(websub_renewing_subscriptions -> websub_pending_subscriptions (new));

allow_tables_to_appear_in_same_query!(
    entries,
    last_tweet,
    ongoing_retweets,
    tweets,
    twitter_tokens,
    websub_active_subscriptions,
    websub_pending_subscriptions,
    websub_renewing_subscriptions,
    websub_subscriptions,
);
