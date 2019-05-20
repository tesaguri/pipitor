use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::ops::DerefMut;

use regex::Regex;
use serde::{de, Deserialize};
use smallvec::{smallvec, SmallVec};

use crate::twitter::Tweet;

mod private {
    #[derive(Clone, Debug, serde::Deserialize, PartialEq, Eq, Hash)]
    pub enum Never {}
}

#[derive(Clone, Default)]
pub struct RuleMap {
    map: HashMap<TopicId, SmallVec<[Rule; 1]>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(untagged)]
pub enum TopicId {
    Twitter(i64),
    #[doc(hidden)]
    _NonExhaustive(private::Never),
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Outbox {
    Twitter(i64),
    None,
    #[doc(hidden)]
    _NonExhaustive(private::Never),
}

#[derive(Clone, Debug, Deserialize)]
pub struct Rule {
    #[serde(default)]
    filter: Filter,
    #[serde(deserialize_with = "de_outbox")]
    outbox: SmallVec<[Outbox; 1]>,
}

#[derive(Clone, Debug, Default)]
pub struct Filter {
    inner: Option<FilterInner>,
}

#[derive(Clone, Debug)]
struct FilterInner {
    title: Regex,
    text: Option<Regex>,
}

impl RuleMap {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn with_capacity(cap: usize) -> Self {
        RuleMap {
            map: HashMap::with_capacity(cap),
        }
    }

    pub fn insert(&mut self, topic: TopicId, rule: Rule) {
        self.map
            .entry(topic)
            .or_insert_with(SmallVec::new)
            .push(rule);
    }

    pub fn get(&self, topic: &TopicId) -> Option<&[Rule]> {
        self.map.get(topic).map(|vec| &**vec)
    }

    pub fn get_mut(
        &mut self,
        topic: &TopicId,
    ) -> Option<&mut (impl Extend<Rule> + DerefMut<Target = [Rule]>)> {
        self.map.get_mut(topic)
    }

    pub fn contains_topic(&self, topic: &TopicId) -> bool {
        self.map.contains_key(topic)
    }

    pub fn remove(
        &mut self,
        topic: &TopicId,
    ) -> Option<impl IntoIterator<Item = Rule> + DerefMut<Target = [Rule]>> {
        self.map.remove(topic)
    }

    pub fn topics(&self) -> impl Iterator<Item = &TopicId> {
        self.map.keys()
    }

    pub fn twitter_topics<'a>(&'a self) -> impl Iterator<Item = i64> + 'a {
        self.topics().filter_map(|topic| match *topic {
            TopicId::Twitter(user) => Some(user),
            _ => None,
        })
    }

    pub fn rules(&self) -> impl Iterator<Item = &Rule> {
        self.map.values().flatten()
    }

    pub fn outboxes(&self) -> impl Iterator<Item = &Outbox> {
        self.rules().flat_map(|rule| &rule.outbox)
    }

    pub fn twitter_outboxes<'a>(&'a self) -> impl Iterator<Item = i64> + 'a {
        self.outboxes().filter_map(|outbox| match *outbox {
            Outbox::Twitter(user) => Some(user),
            _ => None,
        })
    }

    pub fn route_tweet<'a>(&'a self, tweet: &'a Tweet) -> impl Iterator<Item = &'a Outbox> {
        self.get(&TopicId::Twitter(tweet.user.id))
            .into_iter()
            .flatten()
            .filter(move |r| r.filter.matches_tweet(tweet))
            .flat_map(|r| &r.outbox)
    }
}

impl Debug for RuleMap {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.map, f)
    }
}

impl<'de> Deserialize<'de> for RuleMap {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = RuleMap;

            fn expecting(&self, f: &mut Formatter<'_>) -> fmt::Result {
                f.write_str("a sequence")
            }

            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let map = HashMap::with_capacity(seq.size_hint().unwrap_or(0));
                let mut ret = RuleMap { map };

                #[derive(Deserialize)]
                struct RulePrototype {
                    topics: Vec<TopicId>,
                    #[serde(flatten)]
                    rule: Rule,
                }

                while let Some(RulePrototype { mut topics, rule }) = seq.next_element()? {
                    if let Some(last) = topics.pop() {
                        for topic in topics {
                            ret.insert(topic, rule.clone());
                        }
                        ret.insert(last, rule);
                    }
                }

                Ok(ret)
            }
        }

        d.deserialize_seq(Visitor)
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

impl Rule {
    pub fn new(filter: Filter) -> Self {
        Rule {
            filter,
            outbox: SmallVec::new(),
        }
    }

    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    pub fn filter_mut(&mut self) -> &mut Filter {
        &mut self.filter
    }

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
            inner: Some(FilterInner { title, text: None }),
        }
    }

    pub fn matches_tweet(&self, tweet: &Tweet) -> bool {
        self.inner.as_ref().map_or(true, |inner| {
            inner.title.is_match(&tweet.text)
                || inner
                    .text
                    .as_ref()
                    .map_or(false, |t| t.is_match(&tweet.text))
        })
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

        Option::<Prototype>::deserialize(d).map(|o| Filter {
            inner: o.map(|m| match m {
                Prototype::Title(title) => FilterInner { title, text: None },
                Prototype::Composite { title, text } => FilterInner { title, text },
            }),
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
