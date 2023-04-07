use serde::de;

api_requests! {
    POST "https://api.twitter.com/1.1/statuses/update.json" => de::IgnoredAny;
    pub struct Update<'a> {
        status: &'a str;
        trim_user: bool = true,
    }
}
