use dotenv::dotenv_iter;
use serde::Deserialize;

use crate::rules::RuleMap;
use crate::twitter::Credentials;

#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    pub database_url: Option<Box<str>>,
    pub rule: RuleMap,
    pub twitter: Twitter,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Twitter {
    pub client: Credentials<Box<str>>,
    pub user: i64,
    #[serde(default)]
    pub list: Option<u64>,
}

impl Manifest {
    pub fn database_url(&self) -> String {
        if let Some(ref url) = self.database_url {
            url.clone().into()
        } else if let Some((_, url)) = dotenv_iter()
            .into_iter()
            .flatten()
            .flatten()
            .find(|(k, _)| k == "DATABASE_URL")
        {
            url
        } else {
            "pipitor.sqlite3".to_owned()
        }
    }
}
