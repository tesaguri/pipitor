use std::collections::HashMap;
use std::fmt::{self, Debug, Formatter};
use std::ops::DerefMut;
use std::sync::Arc;

use smallvec::SmallVec;

use crate::feed::Entry;
use crate::manifest::{Manifest, Outbox, Route, Rule, TopicId};
use crate::twitter::Tweet;

#[derive(Clone, Default)]
pub struct Router {
    map: HashMap<TopicId<'static>, SmallVec<[Arc<Route>; 1]>>,
}

impl Router {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn from_manifest(manifest: &Manifest) -> Self {
        let topics_count = manifest.topics().count();
        let map = HashMap::with_capacity(topics_count);
        let mut ret = Router { map };

        for rule in &*manifest.rule {
            let &Rule {
                ref topics,
                ref route,
                ..
            } = rule;
            for topic in &**topics {
                ret.append(topic.clone(), route.clone());
            }
        }

        ret
    }

    pub fn with_capacity(cap: usize) -> Self {
        Router {
            map: HashMap::with_capacity(cap),
        }
    }

    pub fn append(&mut self, topic: TopicId<'static>, route: Arc<Route>) {
        self.map
            .entry(topic)
            .or_insert_with(SmallVec::new)
            .push(route);
    }

    pub fn get<'a>(&'a self, topic: &TopicId<'a>) -> Option<&[Arc<Route>]> {
        self.map.get(topic).map(|vec| &**vec)
    }

    pub fn get_mut(
        &mut self,
        topic: &TopicId<'static>, // FIXME: This should be `TopicId<'_>`
    ) -> Option<&mut (impl Extend<Arc<Route>> + DerefMut<Target = [Arc<Route>]>)> {
        self.map.get_mut(topic)
    }

    pub fn remove(
        &mut self,
        topic: &TopicId<'static>, // FIXME: This should be `TopicId<'_>`
    ) -> Option<impl IntoIterator<Item = Arc<Route>> + DerefMut<Target = [Arc<Route>]>> {
        self.map.remove(topic)
    }

    pub fn route_entry<'a>(
        &'a self,
        topic: &'a str,
        entry: &'a Entry,
    ) -> impl Iterator<Item = &Outbox> + 'a {
        self.get(&TopicId::Feed(topic.into()))
            .into_iter()
            .flatten()
            .filter(move |r| {
                r.filter.as_ref().map_or(true, |f| f.matches_entry(entry))
                    && r.exclude.as_ref().map_or(true, |e| !e.matches_entry(entry))
            })
            .flat_map(|r| r.outbox())
    }

    pub fn route_tweet<'a>(&'a self, tweet: &'a Tweet) -> impl Iterator<Item = &'a Outbox> {
        self.get(&TopicId::Twitter(tweet.user.id))
            .into_iter()
            .flatten()
            .filter(move |r| {
                r.filter.as_ref().map_or(true, |f| f.matches_tweet(tweet))
                    && r.exclude.as_ref().map_or(true, |e| !e.matches_tweet(tweet))
            })
            .flat_map(|r| r.outbox())
    }
}

impl Debug for Router {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Debug::fmt(&self.map, f)
    }
}
