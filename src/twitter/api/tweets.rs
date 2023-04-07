use serde::de;

api_requests! {
    POST "https://api.twitter.com/2/tweets" => de::IgnoredAny;
    pub struct Post<'a> {
        text: &'a str;
    }
}
