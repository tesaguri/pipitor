use std::borrow::Cow;
use std::fmt;
use std::num::NonZeroU64;
use std::ops::DerefMut;
use std::path::Path;
use std::str;
use std::sync::Arc;
use std::time::Duration;

use http::Uri;
use oauth_credentials::Credentials;
use regex::Regex;
use serde::{de, Deserialize};
use smallvec::SmallVec;

use crate::feed::Entry;
use crate::socket;
use crate::twitter::Tweet;
use crate::util::{MapAccessDeserializer, SeqAccessDeserializer, Serde};

#[non_exhaustive]
#[derive(Clone, Debug, Default)]
pub struct Manifest {
    pub database_url: Option<Box<str>>,
    pub rule: Box<[Rule]>,
    pub websub: Option<WebSub>,
    pub twitter: Option<Twitter>,
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
pub struct WebSub {
    #[serde(deserialize_with = "de_callback")]
    pub callback: Uri,
    pub bind: Option<socket::Addr>,
    #[serde(default = "one_hour")]
    #[serde(deserialize_with = "de_duration_from_secs")]
    pub renewal_margin: Duration,
}

#[non_exhaustive]
#[derive(Clone, Debug, Deserialize)]
pub struct Twitter {
    #[serde(with = "CredentialsDef")]
    pub client: Credentials<Box<str>>,
    pub user: i64,
    #[serde(default = "default_true")]
    pub stream: bool,
    #[serde(default)]
    pub list: Option<TwitterList>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TwitterList {
    pub id: NonZeroU64,
    /// The default value is based on the rate limit of GET lists/statuses API
    /// (900 reqs/15-min window = 1 req/sec).
    #[serde(default = "one_sec")]
    #[serde(deserialize_with = "de_duration_from_secs")]
    pub interval: Duration,
    #[serde(default = "one_sec")]
    #[serde(deserialize_with = "de_duration_from_secs")]
    pub delay: Duration,
}

#[derive(Deserialize)]
#[serde(remote = "Credentials")]
struct CredentialsDef<T> {
    identifier: T,
    secret: T,
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
    pub fn new() -> Self {
        Default::default()
    }

    pub fn resolve_paths(&mut self, base: &str) {
        if let Some(new) = self
            .database_url
            .as_ref()
            .and_then(|path| resolve_database_uri(path, base))
        {
            self.database_url = Some(new);
        }
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

    pub fn twitter_topics(&self) -> impl Iterator<Item = i64> + '_ {
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

    /// Validates the manifest.
    ///
    /// This is automatically done on deserialization so you do not need to call this method
    /// unless you have manually modified the struct.
    pub fn validate(&self) -> anyhow::Result<()> {
        if let Some(e) = self.validate_() {
            anyhow::bail!(e);
        }
        Ok(())
    }

    fn validate_(&self) -> Option<&'static str> {
        if self.twitter.is_none() {
            if self.twitter_topics().next().is_some() {
                return Some(
                    "the manifest has Twitter topics, but API credentials are not provided",
                );
            } else if self.twitter_outboxes().next().is_some() {
                return Some(
                    "the manifest has Twitter outboxes, but API credentials are not provided",
                );
            }
        }
        None
    }
}

impl<'de> Deserialize<'de> for Manifest {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(remote = "self::Manifest")]
        pub struct Manifest {
            #[serde(default)]
            pub database_url: Option<Box<str>>,
            pub rule: Box<[Rule]>,
            #[serde(default)]
            pub websub: Option<WebSub>,
            #[serde(default)]
            pub twitter: Option<Twitter>,
            #[serde(default)]
            pub skip_duplicate: bool,
        }

        let ret = Manifest::deserialize(d)?;
        if let Some(e) = ret.validate_() {
            return Err(de::Error::custom(e));
        }

        Ok(ret)
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
        struct Rule {
            #[serde(default)]
            filter: Option<Filter>,
            #[serde(default)]
            exclude: Option<Filter>,
            #[serde(deserialize_with = "de_outbox")]
            outbox: SmallVec<[Outbox; 1]>,
            topics: Box<[TopicId<'static>]>,
        }

        Rule::deserialize(d).map(|r| {
            let route = Arc::new(Route {
                filter: r.filter,
                exclude: r.exclude,
                outbox: r.outbox,
            });
            Self {
                route,
                topics: r.topics,
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
                    .any(|c| t.is_match(c))
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
                pub struct Filter {
                    title: Serde<Regex>,
                    #[serde(default)]
                    text: Option<Serde<Regex>>,
                }

                Filter::deserialize(MapAccessDeserializer(a)).map(|f| self::Filter {
                    title: f.title.0,
                    text: f.text.map(|s| s.0),
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
                    i64::deserialize(de::IntoDeserializer::into_deserializer(id))
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
                    i64::deserialize(de::IntoDeserializer::into_deserializer(id))
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
            i64::deserialize(de::IntoDeserializer::into_deserializer(id))
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

fn de_callback<'de, D: de::Deserializer<'de>>(d: D) -> Result<Uri, D::Error> {
    http_serde::uri::deserialize(d).and_then(|uri| {
        if uri.scheme().is_none() {
            Err(de::Error::custom("missing URI scheme"))
        } else if uri.query().is_some() {
            Err(de::Error::custom(
                "`websub.callback` must not have a query part",
            ))
        } else {
            Ok(uri)
        }
    })
}

fn de_duration_from_secs<'de, D: de::Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
    struct Visitor;

    impl<'de> de::Visitor<'de> for Visitor {
        type Value = Duration;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("struct Duration")
        }

        fn visit_seq<A: de::SeqAccess<'de>>(self, a: A) -> Result<Duration, A::Error> {
            Duration::deserialize(SeqAccessDeserializer(a))
        }

        fn visit_map<A: de::MapAccess<'de>>(self, a: A) -> Result<Duration, A::Error> {
            Duration::deserialize(MapAccessDeserializer(a))
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<Duration, E> {
            u64::deserialize(de::IntoDeserializer::into_deserializer(v))
                .and_then(|v| self.visit_u64(v))
        }

        fn visit_u64<E>(self, v: u64) -> Result<Duration, E> {
            Ok(Duration::from_secs(v))
        }
    }

    d.deserialize_struct("Duration", &["secs", "nanos"], Visitor)
}

fn one_hour() -> Duration {
    Duration::from_secs(60 * 60)
}

fn one_sec() -> Duration {
    Duration::from_secs(1)
}

fn default_true() -> bool {
    true
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
        || (uri
            .strip_prefix("file:/")
            .map_or(false, |s| !s.starts_with('/')))
        || uri == ":memory:"
    {
        // Absolute URI filename or in-memory database.
        None
    } else if uri
        .strip_prefix("file:")
        .map_or(false, |s| !s.starts_with("//"))
    {
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
