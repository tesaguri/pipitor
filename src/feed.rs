use std::collections::BTreeMap;

use mime::Mime;

use crate::util;

#[derive(Debug, PartialEq)]
pub struct Feed {
    pub title: String,
    pub id: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug, PartialEq)]
pub struct Entry {
    pub title: Option<String>,
    pub id: Option<String>,
    pub link: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub updated: Option<i64>,
}

#[allow(clippy::large_enum_variant)]
pub enum RawFeed {
    Atom(atom::Feed),
    Rss(rss::Channel),
}

// MIME type for syndication formats.
#[derive(Copy, Clone)]
pub enum MediaType {
    Atom,
    Rss,
    Xml,
}

impl Feed {
    pub fn parse(kind: MediaType, content: &[u8]) -> Option<Self> {
        RawFeed::parse(kind, content).map(Into::into)
    }
}

impl From<RawFeed> for Feed {
    fn from(raw: RawFeed) -> Self {
        match raw {
            RawFeed::Atom(feed) => feed.into(),
            RawFeed::Rss(channel) => channel.into(),
        }
    }
}

impl From<atom::Feed> for Feed {
    fn from(feed: atom::Feed) -> Self {
        Feed {
            title: feed.title.value,
            id: feed.id,
            entries: feed
                .entries
                .into_iter()
                .map(|entry| Entry::from_atom(entry, &feed.namespaces))
                .collect(),
        }
    }
}

impl From<rss::Channel> for Feed {
    fn from(channel: rss::Channel) -> Self {
        Feed {
            title: channel.title,
            id: channel.link,
            entries: channel
                .items
                .into_iter()
                .map(|item| Entry::from_rss(item, &channel.namespaces))
                .collect(),
        }
    }
}

impl Entry {
    fn from_atom(mut entry: atom::Entry, namespaces: &BTreeMap<String, String>) -> Self {
        let mut links = entry.links.into_iter();
        let link = links.next();
        let link = if link.as_ref().map_or(false, |l| l.rel == "alternate") {
            link
        } else {
            links.find(|l| l.rel == "alternate").or(link)
        }
        .map(|l| l.href);
        Entry {
            title: Some(entry.title.value),
            id: Some(entry.id),
            link,
            summary: entry
                .summary
                .map(|text| text.value)
                .or_else(|| take_media_description_atom(&mut entry.extensions, namespaces)),
            content: entry.content.and_then(|c| c.value),
            updated: Some(entry.updated.timestamp()),
        }
    }

    fn from_rss(mut item: rss::Item, namespaces: &BTreeMap<String, String>) -> Self {
        let guid = item.guid;
        let link = item.link.or_else(|| {
            guid.as_ref()
                .filter(|g| g.is_permalink())
                .map(rss::Guid::value)
                .map(str::to_owned)
        });
        Entry {
            title: item.title,
            id: guid.map(|g| g.value),
            link,
            summary: item
                .description
                .or_else(|| take_media_description_rss(&mut item.extensions, namespaces)),
            content: item.content,
            updated: None,
        }
    }
}

impl RawFeed {
    pub fn parse(kind: MediaType, content: &[u8]) -> Option<Self> {
        match kind {
            MediaType::Atom => atom::Feed::read_from(content).ok().map(RawFeed::Atom),
            MediaType::Rss => rss::Channel::read_from(content).ok().map(RawFeed::Rss),
            MediaType::Xml => atom::Feed::read_from(content)
                .ok()
                .map(RawFeed::Atom)
                .or_else(|| rss::Channel::read_from(content).ok().map(RawFeed::Rss)),
        }
    }
}

impl std::str::FromStr for MediaType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, ()> {
        let mime = if let Ok(m) = s.parse::<Mime>() {
            m
        } else {
            return Err(());
        };

        if mime.type_() == mime::APPLICATION
            && mime.subtype() == "atom"
            && mime.suffix() == Some(mime::XML)
        {
            Ok(MediaType::Atom)
        } else if mime.type_() == mime::APPLICATION
            && (mime.subtype() == "rss" || mime.subtype() == "rdf")
            && mime.suffix() == Some(mime::XML)
        {
            Ok(MediaType::Rss)
        } else if (mime.type_() == mime::APPLICATION || mime.type_() == mime::TEXT)
            && mime.subtype() == mime::XML
        {
            Ok(MediaType::Xml)
        } else {
            Err(())
        }
    }
}

macro_rules! def_take_media_description {
    ($name:ident, $Extension:ty) => {
        /// Takes `media:description` string from the entry/item.
        ///
        /// <https://www.rssboard.org/media-rss#media-description>
        fn $name(
            extensions: &mut BTreeMap<String, BTreeMap<String, Vec<$Extension>>>,
            namespaces: &BTreeMap<String, String>,
        ) -> Option<String> {
            fn take_first_value(elms: &mut [$Extension]) -> Option<String> {
                elms.iter_mut().flat_map(|elm| elm.value.take()).next()
            }

            fn take_content_description(elms: &mut Vec<$Extension>) -> Option<String> {
                for elm in elms {
                    for (name, elms) in &mut elm.children {
                        if name == "description" {
                            return take_first_value(elms);
                        }
                    }
                }
                None
            }

            let mut entry_description = None;
            let mut group_description = None;

            for (prefix, map) in extensions {
                if namespaces
                    .get(prefix)
                    .map_or(false, |s| s == util::consts::NS_MRSS)
                {
                    for (name, elms) in map {
                        match &name[..] {
                            "description" => {
                                if let Some(v) = take_first_value(elms) {
                                    entry_description.get_or_insert(v);
                                }
                            }
                            "content" => {
                                if let Some(v) = take_content_description(elms) {
                                    // `media:content`'s description takes the strongest precedence.
                                    return Some(v);
                                }
                            }
                            "group" => {
                                for elm in elms {
                                    for (name, elms) in &mut elm.children {
                                        match &name[..] {
                                            "description" => {
                                                if let Some(v) = take_first_value(elms) {
                                                    group_description.get_or_insert(v);
                                                }
                                            }
                                            "content" => {
                                                if let Some(v) = take_content_description(elms) {
                                                    return Some(v);
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }

            group_description.or(entry_description)
        }
    };
}

def_take_media_description!(take_media_description_atom, atom::extension::Extension);
def_take_media_description!(take_media_description_rss, rss::extension::Extension);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_description() {
        let feed = atom::Feed::read_from(&include_bytes!("feed/testcases/videos.xml")[..]).unwrap();
        let feed = Feed::from(feed);
        let expected =
            atom::Feed::read_from(&include_bytes!("feed/testcases/expected.xml")[..]).unwrap();
        let expected = Feed::from(expected);
        assert_eq!(feed, expected);
    }
}
