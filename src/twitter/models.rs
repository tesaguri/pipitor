use serde::ser::{SerializeMap, Serializer};
use serde::{de, Deserialize, Serialize};

use crate::util::replace_char_range;

#[derive(Clone, Debug, Serialize)]
pub struct Tweet {
    pub id: i64,
    pub text: Box<str>,
    pub in_reply_to_status_id: Option<i64>,
    pub in_reply_to_user_id: Option<i64>,
    pub user: User,
    pub quoted_status: Option<QuotedStatus>,
    #[serde(serialize_with = "ser_retweeted_status")]
    pub retweeted_status: Option<de::IgnoredAny>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ListsMembers {
    pub users: Vec<User>,
    pub next_cursor: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct User {
    pub id: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct QuotedStatus {
    pub id: i64,
}

#[derive(Deserialize)]
struct Entities {
    #[serde(default)]
    urls: Vec<Url>,
    #[serde(default)]
    media: Vec<Url>,
}

#[derive(Deserialize)]
struct Url {
    expanded_url: String,
    indices: (usize, usize),
}

impl<'de> Deserialize<'de> for Tweet {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Prototype {
            id: i64,
            text: String,
            #[serde(default)]
            in_reply_to_status_id: Option<i64>,
            #[serde(default)]
            in_reply_to_user_id: Option<i64>,
            user: User,
            extended_tweet: Option<ExtendedTweet>,
            #[serde(default)]
            quoted_status: Option<QuotedStatus>,
            #[serde(default)]
            retweeted_status: Option<de::IgnoredAny>,
            #[serde(default)]
            entities: Option<Entities>,
        }

        #[derive(Deserialize)]
        struct ExtendedTweet {
            full_text: String,
            #[serde(default)]
            entities: Option<Entities>,
        }

        Prototype::deserialize(d).map(|p| {
            let (text, entities) = (p.text, p.entities);
            let text = p.extended_tweet.map_or_else(
                || expand_urls(text, entities),
                |e| expand_urls(e.full_text, e.entities),
            )?;
            Ok(Tweet {
                id: p.id,
                text,
                in_reply_to_status_id: p.in_reply_to_status_id,
                in_reply_to_user_id: p.in_reply_to_user_id,
                user: p.user,
                quoted_status: p.quoted_status,
                retweeted_status: p.retweeted_status,
            })
        })?
    }
}

fn ser_retweeted_status<S>(rt: &Option<de::IgnoredAny>, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    if rt.is_some() {
        let map = s.serialize_map(Some(0))?;
        map.end()
    } else {
        s.serialize_unit()
    }
}

fn expand_urls<E>(mut text: String, entities: Option<Entities>) -> Result<Box<str>, E>
where
    E: de::Error,
{
    let mut e = if let Some(e) = entities {
        e
    } else {
        return Ok(text.into_boxed_str());
    };

    let mut urls = if e.urls.len() > e.media.len() {
        e.urls.extend(e.media);
        e.urls
    } else {
        e.media.extend(e.urls);
        e.media
    };
    urls.sort_by_key(|u| u.indices.0);

    let (mut byte_offset, mut char_offset) = (0, 0);
    for url in urls {
        let Url {
            indices: (s, e),
            expanded_url,
        } = url;

        if s > e {
            return Err(E::custom("invalid indices (start > end)"));
        }

        if e < byte_offset {
            return Err(E::custom("url indices overlap"));
        }

        let range = (s - char_offset)..(e - char_offset);
        if let Some(i) = replace_char_range(&mut text, byte_offset, range, &expanded_url) {
            byte_offset = i + expanded_url.len();
            char_offset = e;
        } else {
            return Err(E::custom("url indices out of range"));
        }
    }

    Ok(text.into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn de_tweet() {
        let tweet: Tweet =
            json::from_str(include_str!("testcases/status_912783930431905797.json")).unwrap();
        let expected_text = "Can’t fit your Tweet into 140 characters? 🤔\n\nWe’re trying something new with a small group, and increasing the character limit to 280! Excited about the possibilities? Read our blog to find out how it all adds up. 👇\nhttps://cards.twitter.com/cards/gsby/4ubsj";
        match tweet {
            Tweet {
                id: 912783930431905797,
                ref text,
                in_reply_to_status_id: None,
                in_reply_to_user_id: None,
                user: User { id: 783214 },
                quoted_status: None,
                retweeted_status: None,
            } if **text == *expected_text => {}
            _ => panic!("tweet: {:#?}", tweet),
        }
    }
}
