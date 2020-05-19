use serde::de;

api_requests! {
    // XXX: This would be HEAD request if we could skip deserialization of the response
    GET "https://api.twitter.com/1.1/statuses/show.json" => de::IgnoredAny;
    pub struct Show {
        id: i64;
        trim_user: bool = true,
        include_entities: bool,
    }

    POST "https://api.twitter.com/1.1/statuses/retweet.json" => de::IgnoredAny;
    pub struct Retweet {
        id: i64;
    }

    POST "https://api.twitter.com/1.1/statuses/update.json" => de::IgnoredAny;
    pub struct Update<'a> {
        status: &'a str;
        trim_user: bool = true,
    }
}
