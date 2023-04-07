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

impl From<TwitterToken> for oauth_credentials::Credentials<Box<str>> {
    fn from(token: TwitterToken) -> Self {
        oauth_credentials::Credentials::new(token.access_token, token.access_token_secret)
            .map(Into::into)
    }
}

impl<'a> From<&'a TwitterToken> for oauth_credentials::Credentials<&'a str> {
    fn from(token: &'a TwitterToken) -> Self {
        Self::new(&token.access_token, &token.access_token_secret)
    }
}
