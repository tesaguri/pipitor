use serde::de;
use twitter_client::request::RawRequest;

api_requests! {
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

#[derive(oauth1::Request)]
pub struct Show {
    id: i64,
    trim_user: bool,
    include_entities: bool,
}

impl Show {
    pub fn new(id: i64) -> Self {
        Show {
            id,
            trim_user: true,
            include_entities: false,
        }
    }
}

impl RawRequest for Show {
    fn method(&self) -> &http::Method {
        &http::Method::HEAD
    }

    fn uri(&self) -> &'static str {
        "https://api.twitter.com/1.1/statuses/show.json"
    }
}
