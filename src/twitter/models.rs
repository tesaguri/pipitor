use serde::{de, Deserialize};

#[derive(Clone, Debug, Deserialize)]
pub struct Tweet {
    pub id: i64,
    pub text: Box<str>,
    #[serde(default)]
    pub in_reply_to_status_id: Option<i64>,
    #[serde(default)]
    pub in_reply_to_user_id: Option<i64>,
    pub user: User,
    #[serde(default)]
    pub quoted_status: Option<QuotedStatus>,
    #[serde(default)]
    pub retweeted_status: Option<de::IgnoredAny>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub id: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QuotedStatus {
    pub id: i64,
}
