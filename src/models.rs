use crate::feed::Entry;
use crate::schema::*;

#[derive(Clone, Debug, Insertable)]
#[table_name = "entries"]
pub struct NewEntry<'a> {
    pub topic: &'a str,
    pub id: Option<&'a str>,
    pub link: &'a str,
    pub title: Option<&'a str>,
    pub summary: Option<&'a str>,
    pub content: Option<&'a str>,
    pub updated: Option<i64>,
}

#[derive(Clone, Debug, Identifiable, Queryable)]
#[table_name = "last_tweet"]
pub struct LastTweet {
    pub id: i64,
    pub status_id: i64,
}

#[derive(Clone, Debug, Insertable)]
#[table_name = "last_tweet"]
pub struct NewLastTweet {
    pub id: i64,
    pub status_id: i64,
}

#[derive(Clone, Debug, Identifiable, Queryable)]
pub struct Tweet {
    pub id: i64,
    pub text: String,
    pub user_id: i64,
    pub in_reply_to_status_id: Option<i64>,
    pub quoted_status_id: Option<i64>,
    pub quoted_status_text: Option<String>,
}

#[derive(Clone, Debug, Insertable)]
#[table_name = "tweets"]
pub struct NewTweet<'a> {
    pub id: i64,
    pub text: &'a str,
    pub user_id: i64,
    pub in_reply_to_status_id: Option<i64>,
    pub quoted_status_id: Option<i64>,
}

#[derive(Clone, Debug, Identifiable, Queryable)]
pub struct TwitterToken {
    pub id: i64,
    pub access_token: String,
    pub access_token_secret: String,
}

#[derive(Clone, Debug, Insertable)]
#[table_name = "twitter_tokens"]
pub struct NewTwitterTokens<'a> {
    pub id: i64,
    pub access_token: &'a str,
    pub access_token_secret: &'a str,
}

impl<'a> NewEntry<'a> {
    pub fn new(topic: &'a str, entry: &'a Entry) -> Option<Self> {
        let link = entry.link.as_deref()?;
        Some(NewEntry {
            topic,
            id: entry.id.as_deref(),
            link,
            title: entry.title.as_deref(),
            summary: entry.summary.as_deref(),
            content: entry.content.as_deref(),
            updated: entry.updated,
        })
    }
}

impl From<TwitterToken> for oauth1::Credentials<Box<str>> {
    fn from(token: TwitterToken) -> Self {
        Self {
            identifier: token.access_token.into(),
            secret: token.access_token_secret.into(),
        }
    }
}

impl From<TwitterToken> for oauth1::Credentials {
    fn from(token: TwitterToken) -> Self {
        Self {
            identifier: token.access_token,
            secret: token.access_token_secret,
        }
    }
}

impl<'a> From<&'a TwitterToken> for oauth1::Credentials<&'a str> {
    fn from(token: &'a TwitterToken) -> Self {
        Self {
            identifier: &token.access_token,
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
