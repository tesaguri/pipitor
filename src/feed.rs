#[derive(Debug)]
pub struct Feed {
    pub title: String,
    pub id: String,
    pub entries: Vec<Entry>,
}

#[derive(Debug)]
pub struct Entry {
    pub title: Option<String>,
    pub id: Option<String>,
    pub link: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub updated: Option<i64>,
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
            title: channel.title().to_owned(),
            id: channel.link().to_owned(),
            entries: channel.into_items().into_iter().map(Into::into).collect(),
        }
    }
}

impl From<atom::Entry> for Entry {
    fn from(entry: atom::Entry) -> Self {
        let links = entry.links();
        let link = links
            .iter()
            .find(|l| l.rel() == "alternate")
            .or(links.first())
            .map(|l| l.href().to_owned());
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
        let link = item
            .link()
            .or_else(|| {
                item.guid()
                    .filter(|guid| guid.is_permalink())
                    .map(rss::Guid::value)
            })
            .map(str::to_owned);
        Entry {
            title: item.title().map(str::to_owned),
            id: item.guid().map(rss::Guid::value).map(str::to_owned),
            link,
            summary: item.description().map(String::from),
            content: item.content().map(String::from),
            updated: None,
        }
    }
}
