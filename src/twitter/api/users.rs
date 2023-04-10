use serde::de;

api_requests! {
    GET "https://api.twitter.com/2/users/me" => de::IgnoredAny;
    #[derive(Default)]
    pub struct Me {;}
}
