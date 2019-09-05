use std::fmt::{self, Display, Formatter};
use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use super::super::models;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ListsMembers {
    pub users: Vec<models::User>,
    pub next_cursor: u64,
}

api_requests! {
    GET "https://api.twitter.com/1.1/lists/members.json" => ListsMembers;
    pub struct Members {
        list_id: NonZeroU64;
        count: Option<usize>,
        cursor: Option<u64>,
        include_entities: bool,
        skip_status: bool = true,
    }

    GET "https://api.twitter.com/1.1/lists/statuses.json" => Vec<models::Tweet>;
    pub struct Statuses {
        list_id: NonZeroU64;
        since_id: Option<i64>,
        max_id: Option<i64>,
        count: Option<usize> = Some(200),
        include_entities: bool,
        include_rts: Option<bool>,
        tweet_mode: Option<TweetMode> = Some(TweetMode::Extended),
    }
}

pub enum TweetMode {
    Extended,
}

impl Display for TweetMode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match *self {
            TweetMode::Extended => f.write_str("extended"),
        }
    }
}

pub mod members {
    use std::num::NonZeroU64;

    use serde::de;

    api_requests! {
        POST "https://api.twitter.com/1.1/lists/members/create.json" => de::IgnoredAny;
        pub struct Create {
            list_id: NonZeroU64,
            user_id: i64;
        }

        POST "https://api.twitter.com/1.1/lists/members/destroy.json" => de::IgnoredAny;
        pub struct Destroy {
            list_id: NonZeroU64,
            user_id: i64;
        }
    }
}
