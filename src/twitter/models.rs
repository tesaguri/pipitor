use serde::{de, Deserialize};

#[derive(Clone, Debug)]
pub struct Tweet {
    pub id: i64,
    pub text: Box<str>,
    pub in_reply_to_status_id: Option<i64>,
    pub in_reply_to_user_id: Option<i64>,
    pub user: User,
    pub quoted_status: Option<QuotedStatus>,
    pub retweeted_status: Option<de::IgnoredAny>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ListsMembers {
    pub users: Vec<User>,
    pub next_cursor: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct User {
    pub id: i64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QuotedStatus {
    pub id: i64,
}

impl<'de> Deserialize<'de> for Tweet {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Prototype {
            id: i64,
            text: Box<str>,
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
        }

        #[derive(Deserialize)]
        struct ExtendedTweet {
            full_text: Box<str>,
        }

        Prototype::deserialize(d).map(|p| Tweet {
            id: p.id,
            text: p.extended_tweet.map_or(p.text, |e| e.full_text),
            in_reply_to_status_id: p.in_reply_to_status_id,
            in_reply_to_user_id: p.in_reply_to_user_id,
            user: p.user,
            quoted_status: p.quoted_status,
            retweeted_status: p.retweeted_status,
        })
    }
}
