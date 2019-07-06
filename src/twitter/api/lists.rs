use std::num::NonZeroU64;

api_requests! {
    GET "https://api.twitter.com/1.1/lists/members.json" => super::super::ListsMembers;
    pub struct Members {
        list_id: NonZeroU64;
        #[oauth1(option)]
        count: Option<usize>,
        #[oauth1(option)]
        cursor: Option<u64>,
        include_entities: bool,
        skip_status: bool = true,
    }

    GET "https://api.twitter.com/1.1/lists/statuses.json" => Vec<super::super::Tweet>;
    pub struct Statuses {
        list_id: NonZeroU64;
        #[oauth1(option)]
        since_id: Option<i64>,
        #[oauth1(option)]
        max_id: Option<i64>,
        #[oauth1(option)]
        count: Option<usize> = Some(200),
        include_entities: bool,
        #[oauth1(option)]
        include_rts: Option<bool>,
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
