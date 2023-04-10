use oauth1::Credentials;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct User {
    #[serde(deserialize_with = "crate::util::deserialize_from_str")]
    pub id: i64,
}

#[derive(serde::Deserialize)]
pub struct AccessToken {
    pub credentials: Credentials,
    pub user_id: i64,
}

impl<'a> From<&'a AccessToken> for crate::models::NewTwitterTokens<'a> {
    fn from(t: &'a AccessToken) -> Self {
        crate::models::NewTwitterTokens {
            id: t.user_id,
            access_token: &t.credentials.identifier,
            access_token_secret: &t.credentials.secret,
        }
    }
}
