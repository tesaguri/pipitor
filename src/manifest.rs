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
    #[serde(skip)]
    _non_exhaustive: (),
}

#[derive(Clone, Debug, Deserialize)]
pub struct Rule {
    #[serde(flatten)]
    pub route: Arc<Route>,
    pub topics: Box<[TopicId<'static>]>,
    #[serde(skip)]
    _non_exhaustive: (),
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

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Outbox {
    Twitter(i64),
    None,
    #[doc(hidden)]
    _NonExhaustive(crate::util::Never),
}

#[derive(Clone, Debug)]
pub struct Filter {
    pub title: Regex,
    pub text: Option<Regex>,
    _non_exhaustive: (),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum TopicId<'a> {
    Feed(Cow<'a, str>),
    Twitter(i64),
    #[doc(hidden)]
    _NonExhaustive(crate::util::Never),
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

    pub fn has_topic(&self, topic: &TopicId<'_>) -> bool {
        self.topics().any(|t| t == topic)
    }

    pub fn topics(&self) -> impl Iterator<Item = &TopicId<'_>> {
        self.rule.iter().flat_map(|rule| &*rule.topics)
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
        Filter {
            title,
            text: None,
            _non_exhaustive: (),
        }
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
            Prototype::Composite { title, text } => Filter {
                title,
                text,
                _non_exhaustive: (),
            },
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
