use std::borrow::Cow;
use std::num::NonZeroU64;
use std::ops::DerefMut;
use std::path::Path;
use std::sync::Arc;

use regex::Regex;
use serde::{de, Deserialize};
use smallvec::{smallvec, SmallVec};

use crate::feed::Entry;
use crate::twitter::Tweet;

#[non_exhaustive]
#[derive(Clone, Debug, Deserialize)]
pub struct Manifest {
    #[serde(default)]
    pub credentials: Option<Box<str>>,
    #[serde(default)]
    pub database_url: Option<Box<str>>,
    pub rule: Box<[Rule]>,
    pub twitter: Twitter,
    #[serde(default)]
    pub skip_duplicate: bool,
}

#[non_exhaustive]
#[derive(Clone, Debug, Deserialize)]
pub struct Rule {
    #[serde(flatten)]
    pub route: Arc<Route>,
    pub topics: Box<[TopicId<'static>]>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Route {
    #[serde(default)]
    pub filter: Option<Filter>,
    #[serde(default)]
    pub exclude: Option<Filter>,
    #[serde(deserialize_with = "de_outbox")]
    outbox: SmallVec<[Outbox; 1]>,
}

#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Outbox {
    Twitter(i64),
    None,
}

#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Filter {
    pub title: Regex,
    pub text: Option<Regex>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum TopicId<'a> {
    Feed(Cow<'a, str>),
    Twitter(i64),
    #[doc(hidden)]
    _NonExhaustive(crate::util::Never),
}

#[non_exhaustive]
#[derive(Clone, Debug, Deserialize)]
pub struct Twitter {
    pub user: i64,
    #[serde(default)]
    pub list: Option<TwitterList>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TwitterList {
    pub id: NonZeroU64,
    #[serde(default)]
    pub delay: u64,
}

impl Manifest {
    pub fn resolve_paths(&mut self, base: &str) {
        if let Some(new) = self
            .credentials
            .as_ref()
            .and_then(|path| resolve_path(path, base))
        {
            self.credentials = Some(new);
        }
        if let Some(new) = self
            .database_url
            .as_ref()
            .and_then(|path| resolve_database_uri(path, base))
        {
            self.database_url = Some(new);
        }
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

    pub fn has_topic(&self, topic: &TopicId<'_>) -> bool {
        self.topics().any(|t| t == topic)
    }

    pub fn topics(&self) -> impl Iterator<Item = &TopicId<'_>> {
        self.rule.iter().flat_map(|rule| &*rule.topics)
    }

    pub fn feed_topics(&self) -> impl Iterator<Item = &str> {
        self.topics().filter_map(|topic| match *topic {
            TopicId::Feed(ref topic) => Some(&**topic),
            _ => None,
        })
    }

    pub fn twitter_topics<'a>(&'a self) -> impl Iterator<Item = i64> + 'a {
        self.topics().filter_map(|topic| match *topic {
            TopicId::Twitter(user) => Some(user),
            _ => None,
        })
    }

    pub fn outboxes(&self) -> impl Iterator<Item = &Outbox> {
        self.rule.iter().flat_map(|rule| &rule.route.outbox)
    }

    pub fn twitter_outboxes(&self) -> impl Iterator<Item = i64> + '_ {
        self.outboxes().filter_map(|outbox| match *outbox {
            Outbox::Twitter(user) => Some(user),
            _ => None,
        })
    }
}

impl Route {
    pub fn outbox(&self) -> &[Outbox] {
        &self.outbox
    }

    pub fn outbox_mut(
        &mut self,
    ) -> &mut (impl Default + Extend<Outbox> + DerefMut<Target = [Outbox]>) {
        &mut self.outbox
    }
}

impl Filter {
    pub fn from_title(title: Regex) -> Self {
        Filter { title, text: None }
    }

    pub fn matches_entry(&self, entry: &Entry) -> bool {
        entry
            .title
            .as_ref()
            .map_or(false, |t| self.title.is_match(t))
            || self.text.as_ref().map_or(false, |t| {
                entry
                    .content
                    .as_ref()
                    .into_iter()
                    .chain(entry.summary.as_ref())
                    .any(|c| t.is_match(&c))
            })
    }

    pub fn matches_tweet(&self, tweet: &Tweet) -> bool {
        self.title.is_match(&tweet.text)
            || self
                .text
                .as_ref()
                .map_or(false, |t| t.is_match(&tweet.text))
    }
}

impl<'de> Deserialize<'de> for Filter {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Prototype {
            Title(#[serde(with = "serde_regex")] Regex),
            Composite {
                #[serde(with = "serde_regex")]
                title: Regex,
                #[serde(default)]
                #[serde(with = "serde_regex")]
                text: Option<Regex>,
            },
        }

        Prototype::deserialize(d).map(|p| match p {
            Prototype::Title(title) => Filter::from_title(title),
            Prototype::Composite { title, text } => Filter { title, text },
        })
    }
}

impl<'de> Deserialize<'de> for Outbox {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        i64::deserialize(d).map(|id| {
            if id == 0 {
                Outbox::None
            } else {
                Outbox::Twitter(id)
            }
        })
    }
}

fn de_outbox<'de, D: de::Deserializer<'de>>(d: D) -> Result<SmallVec<[Outbox; 1]>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Prototype {
        One(Outbox),
        Seq(SmallVec<[Outbox; 1]>),
    }

    Prototype::deserialize(d).map(|p| match p {
        Prototype::One(o) => smallvec![o],
        Prototype::Seq(v) => v,
    })
}

fn resolve_path(path: &str, base: &str) -> Option<Box<str>> {
    if Path::new(path).is_absolute() {
        None
    } else {
        let new = Path::new(base)
            .join(&path)
            .into_os_string()
            .into_string()
            .unwrap()
            .into();
        Some(new)
    }
}

fn resolve_database_uri(uri: &str, base: &str) -> Option<Box<str>> {
    // <https://sqlite.org/c3ref/open.html>.
    if uri.starts_with("file:///")
        || uri.starts_with("file://localhost/")
        || (uri.starts_with("file:/") && !uri[6..].starts_with('/'))
        || uri == ":memory:"
    {
        // Absolute URI filename or in-memory database.
        None
    } else if uri.starts_with("file:") && !uri[5..].starts_with("//") {
        // Relative URI filename
        let path = &uri[5..];
        let i = path
            .find('?')
            .or_else(|| path.find('#'))
            .unwrap_or_else(|| path.len());
        let (path, query_and_fragment) = path.split_at(i);
        resolve_path(path, base).map(|new| {
            ["file:", &new, query_and_fragment]
                .iter()
                .copied()
                .collect::<String>()
                .into()
        })
    } else {
        // Ordinary filename
        resolve_path(uri, base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_uri() {
        assert_eq!(resolve_database_uri("file:///absolute", "/base"), None);
        assert_eq!(
            resolve_database_uri("file://localhost/absolute", "/base"),
            None,
        );
        assert_eq!(resolve_database_uri("file:/absolute", "/base"), None);
        assert_eq!(
            resolve_database_uri("file:relative", "/base").as_deref(),
            Some("file:/base/relative"),
        );
        assert_eq!(
            resolve_database_uri("file:relative?k=v", "base").as_deref(),
            Some("file:base/relative?k=v"),
        );
        assert_eq!(
            resolve_database_uri("file:relative#f", "/base").as_deref(),
            Some("file:/base/relative#f"),
        );
    }
}
