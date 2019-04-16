use crate::schema::*;

#[derive(Identifiable, Queryable)]
pub struct Tweet {
    pub id: i64,
    pub text: String,
    pub user_id: i64,
    pub in_reply_to_status_id: Option<i64>,
    pub quoted_status_id: Option<i64>,
    pub quoted_status_text: Option<String>,
}

#[derive(Insertable)]
#[table_name = "tweets"]
pub struct NewTweet<'a> {
    pub id: i64,
    pub text: &'a str,
    pub user_id: i64,
    pub in_reply_to_status_id: Option<i64>,
    pub quoted_status_id: Option<i64>,
}

#[derive(Identifiable, Queryable)]
pub struct TwitterToken {
    pub id: i64,
    pub access_token: String,
    pub access_token_secret: String,
}

#[derive(Insertable)]
#[table_name = "twitter_tokens"]
pub struct NewTwitterTokens<'a> {
    pub id: i64,
    pub access_token: &'a str,
    pub access_token_secret: &'a str,
}

impl<T: From<String>> From<TwitterToken> for crate::twitter::Credentials<T> {
    fn from(token: TwitterToken) -> Self {
        Self {
            key: token.access_token.into(),
            secret: token.access_token_secret.into(),
        }
    }
}

impl<'a> From<&'a TwitterToken> for crate::twitter::Credentials<&'a str> {
    fn from(token: &'a TwitterToken) -> Self {
        Self {
            key: &token.access_token,
            secret: &token.access_token_secret,
        }
    }
}

impl<'a> From<&'a crate::twitter::Tweet> for NewTweet<'a> {
    fn from(tweet: &'a crate::twitter::Tweet) -> Self {
        NewTweet {
            id: tweet.id,
            text: &tweet.text,
            user_id: tweet.user.id,
            in_reply_to_status_id: tweet.in_reply_to_status_id,
            quoted_status_id: tweet.quoted_status.as_ref().map(|q| q.id),
        }
    }
}
