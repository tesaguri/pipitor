use std::borrow::Cow;
use std::num::NonZeroU64;
use std::path::Path;

use serde::Deserialize;

use crate::rules::RuleMap;

#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub credentials: Option<Box<str>>,
    #[serde(default)]
    pub database_url: Option<Box<str>>,
    pub rule: RuleMap,
    pub twitter: Twitter,
    #[serde(default)]
    pub skip_duplicate: bool,
    #[serde(skip)]
    _non_exhaustive: (),
}

#[derive(Clone, Debug, Deserialize)]
pub struct Twitter {
    pub user: i64,
    #[serde(default)]
    pub list: Option<NonZeroU64>,
    #[serde(skip)]
    _non_exhaustive: (),
}

impl Manifest {
    pub fn resolve_paths(&mut self, base: &str) {
        let resolve = |path: &mut Box<str>| {
            *path = Path::new(base)
                .join(&**path)
                .into_os_string()
                .into_string()
                .unwrap()
                .into();
        };
        self.credentials.as_mut().map(resolve);
        self.database_url.as_mut().map(resolve);
    }

    pub fn credentials_path(&self) -> &str {
        self.credentials
            .as_ref()
            .map(AsRef::as_ref)
            .unwrap_or("credentials.toml")
    }

    pub fn database_url(&self) -> Cow<'_, str> {
        if let Some(ref url) = self.database_url {
            Cow::Borrowed(url)
        } else if let Ok(url) = dotenv::var("DATABASE_URL") {
            Cow::Owned(url)
        } else {
            Cow::Borrowed("pipitor.sqlite3")
        }
    }
}
