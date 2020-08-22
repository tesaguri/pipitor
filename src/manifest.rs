use std::borrow::Cow;
use std::convert::TryFrom;
use std::fmt;
use std::num::NonZeroU64;
use std::ops::DerefMut;
use std::path::Path;
use std::str;
use std::sync::Arc;
use std::time::Duration;

use regex::Regex;
use serde::{de, Deserialize};
use smallvec::SmallVec;

use crate::feed::Entry;
use crate::twitter::Tweet;
use crate::util::{MapAccessDeserializer, Serde};

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
#[derive(Clone, Debug)]
pub struct Rule {
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
}

#[non_exhaustive]
#[derive(Clone, Debug)]
pub struct Filter {
    pub title: Regex,
    pub text: Option<Regex>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
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
    #[serde(deserialize_with = "de_delay")]
    pub delay: Duration,
}

/// Implements `serde::de::Visitor` methods for "union" types,
/// which ignores enum variants and forwards to `deserialize_any`.
macro_rules! union_visitor {
    ($(
        fn $method:ident$(<$($T:ident $(: $Bound:path)?),*>)?($self:ident $(, $arg:ident: $t:ty)*)
            -> $Ret:ty
        {
            $($body:tt)*
        }
    )*) => {
        $(
            fn $method$(<$($T $(: $Bound)?),*>)?($self $(, $arg: $t)*) -> $Ret {
                $($body)*
            }
        )*
        fn visit_enum<A: de::EnumAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
            /// Provides a custom `Deserialize` impl which disables `visit_enum`.
            struct Wrap(<Visitor as de::Visitor<'static>>::Value);
            impl<'de> de::Deserialize<'de> for Wrap {
                fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                    struct WrapVisitor;
                    impl<'de> de::Visitor<'de> for WrapVisitor {
                        type Value = <Visitor as de::Visitor<'de>>::Value;
                        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                            Visitor.expecting(f)
                        }
                        $(
                            fn $method$(<$($T $(: $Bound)?),*>)?($self $(, $arg: $t)*) -> $Ret {
                                Visitor.$method($($arg),*)
                            }
                        )*
                    }
                    d.deserialize_any(WrapVisitor).map(Wrap)
                }
            }
            de::VariantAccess::newtype_variant::<Wrap>(a.variant::<de::IgnoredAny>()?.1)
                .map(|w| w.0)
        }
    };
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
        self.outboxes().map(|outbox| match *outbox {
            Outbox::Twitter(user) => user,
        })
    }
}

impl<'de> Deserialize<'de> for Rule {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // This implementation is almost the same as the following, but since `#[serde(flatten)]`
        // cannot deserialize a map with an enum value, we have to implement it manually.
        //
        // ```
        // #[derive(Deserialize)]
        // struct Rule {
        //     #[serde(flatten)] route: Arc<Route>,
        //     topics: Box<[TopicId<'static>]>
        // }
        // ```

        #[derive(Deserialize)]
        struct Prototype {
            #[serde(default)]
            filter: Option<Filter>,
            #[serde(default)]
            exclude: Option<Filter>,
            #[serde(deserialize_with = "de_outbox")]
            outbox: SmallVec<[Outbox; 1]>,
            topics: Box<[TopicId<'static>]>,
        }

        Prototype::deserialize(d).map(|p| {
            let route = Arc::new(Route {
                filter: p.filter,
                exclude: p.exclude,
                outbox: p.outbox,
            });
            Rule {
                route,
                topics: p.topics,
            }
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
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Filter;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string or map")
            }

            fn visit_map<A: de::MapAccess<'de>>(self, a: A) -> Result<Filter, A::Error> {
                #[derive(Deserialize)]
                pub struct FilterPrototype {
                    title: Serde<Regex>,
                    #[serde(default)]
                    text: Option<Serde<Regex>>,
                }

                FilterPrototype::deserialize(MapAccessDeserializer(a)).map(|p| Filter {
                    title: p.title.0,
                    text: p.text.map(|s| s.0),
                })
            }

            fn visit_str<E: de::Error>(self, s: &str) -> Result<Filter, E> {
                s.parse().map(Filter::from_title).map_err(E::custom)
            }

            serde_delegate!(visit_bytes);
        }

        d.deserialize_any(Visitor)
    }
}

impl<'a, 'de> Deserialize<'de> for TopicId<'a> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = TopicId<'static>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string or integer")
            }

            union_visitor! {
                fn visit_i64<E: de::Error>(self, id: i64) -> Result<Self::Value, E> {
                    Ok(TopicId::Twitter(id))
                }

                fn visit_u64<E: de::Error>(self, id: u64) -> Result<Self::Value, E> {
                    i64::try_from(id)
                        .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(id), &self))
                        .and_then(|id| self.visit_i64(id))
                }

                fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
                    Ok(TopicId::Feed(Cow::Owned(s.to_owned())))
                }

                fn visit_string<E: de::Error>(self, s: String) -> Result<Self::Value, E> {
                    Ok(TopicId::Feed(Cow::Owned(s)))
                }

                fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
                    str::from_utf8(v).map_err(E::custom).and_then(|s| self.visit_str(s))
                }

                fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
                    String::from_utf8(v).map_err(E::custom).and_then(|s| self.visit_string(s))
                }
            }
        }

        d.deserialize_any(Visitor)
    }
}

impl<'de> Deserialize<'de> for Outbox {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Outbox;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an integer")
            }

            union_visitor! {
                fn visit_i64<E: de::Error>(self, id: i64) -> Result<Self::Value, E> {
                    Ok(Outbox::Twitter(id))
                }

                fn visit_u64<E: de::Error>(self, id: u64) -> Result<Self::Value, E> {
                    i64::try_from(id)
                        .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(id), &self))
                        .and_then(|id| self.visit_i64(id))
                }
            }
        }

        d.deserialize_any(Visitor)
    }
}

fn de_outbox<'de, D: de::Deserializer<'de>>(d: D) -> Result<SmallVec<[Outbox; 1]>, D::Error> {
    struct Visitor;

    impl<'de> de::Visitor<'de> for Visitor {
        type Value = SmallVec<[Outbox; 1]>;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("an integer or array of integers")
        }

        fn visit_i64<E: de::Error>(self, id: i64) -> Result<Self::Value, E> {
            Ok(SmallVec::from_buf([Outbox::Twitter(id)]))
        }

        fn visit_u64<E: de::Error>(self, id: u64) -> Result<Self::Value, E> {
            i64::try_from(id)
                .map_err(|_| E::invalid_value(de::Unexpected::Unsigned(id), &self))
                .and_then(|id| self.visit_i64(id))
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, a: A) -> Result<Self::Value, A::Error> {
            let mut a = a;
            let mut ret = SmallVec::with_capacity(a.size_hint().unwrap_or(0));
            while let Some(outbox) = a.next_element()? {
                ret.push(outbox);
            }
            Ok(ret)
        }
    }

    d.deserialize_any(Visitor)
}

fn de_delay<'de, D: de::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    u64::deserialize(d).map(Duration::from_millis)
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

// Disable the tests until we figure out the correct behavior on Windows.
#[cfg(not(windows))]
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
