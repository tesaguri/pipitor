use mime::Mime;

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
            title: feed.title,
            id: feed.id,
            entries: feed.entries.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<rss::Channel> for Feed {
    fn from(channel: rss::Channel) -> Self {
        Feed {
            title: channel.title,
            id: channel.link,
            entries: channel.items.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<atom::Entry> for Entry {
    fn from(entry: atom::Entry) -> Self {
        let mut links = entry.links.into_iter();
        let link = links.next();
        let link = if link.as_ref().map_or(false, |l| l.rel == "alternate") {
            link
        } else {
            links.find(|l| l.rel == "alternate").or(link)
        }
        .map(|l| l.href);
        Entry {
            title: Some(entry.title),
            id: Some(entry.id),
            link,
            summary: entry.summary,
            content: entry.content.and_then(|c| c.value),
            updated: Some(entry.updated.timestamp()),
        }
    }
}

impl From<rss::Item> for Entry {
    fn from(item: rss::Item) -> Self {
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
            summary: item.description,
            content: item.content,
            updated: None,
        }
    }
}

impl RawFeed {
    pub fn parse(kind: MediaType, content: &[u8]) -> Option<Self> {
        match kind {
            MediaType::Atom => atom::Feed::read_from(&*content).ok().map(RawFeed::Atom),
            MediaType::Rss => rss::Channel::read_from(&*content).ok().map(RawFeed::Rss),
            MediaType::Xml => atom::Feed::read_from(&*content)
                .ok()
                .map(RawFeed::Atom)
                .or_else(|| rss::Channel::read_from(&*content).ok().map(RawFeed::Rss)),
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
