use serde::de;

api_requests! {
    POST "https://api.twitter.com/1.1/statuses/retweet.json" => de::IgnoredAny;
    pub struct Retweet {
        id: i64;
    }
}
