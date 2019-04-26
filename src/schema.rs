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

allow_tables_to_appear_in_same_query!(
    last_tweet,
    tweets,
    twitter_tokens,
);
