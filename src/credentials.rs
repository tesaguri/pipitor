use std::convert::TryFrom;
use std::fmt::{self, Formatter};

use http::Uri;
use serde::{de, Deserialize};

use crate::socket;

#[non_exhaustive]
#[derive(Clone, Debug, Deserialize)]
pub struct Credentials {
    pub twitter: Twitter,
    pub websub: Option<WebSub>,
}

#[non_exhaustive]
#[derive(Clone, Debug, Deserialize)]
pub struct Twitter {
    #[serde(with = "CredentialsDef")]
    pub client: oauth1::Credentials<Box<str>>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct WebSub {
    #[serde(deserialize_with = "deserialize_uri")]
    pub host: Uri,
    pub bind: Option<socket::Addr>,
}

#[derive(Deserialize)]
#[serde(remote = "oauth1::Credentials")]
struct CredentialsDef<T> {
    #[serde(rename = "key")]
    identifier: T,
    secret: T,
}

fn deserialize_uri<'de, D: de::Deserializer<'de>>(d: D) -> Result<Uri, D::Error> {
    struct Visitor;

    impl<'de> de::Visitor<'de> for Visitor {
        type Value = Uri;

        fn expecting(&self, f: &mut Formatter<'_>) -> fmt::Result {
            write!(f, "an URI string")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Uri, E> {
            Uri::try_from(v).map_err(E::custom)
        }

        fn visit_string<E: de::Error>(self, v: String) -> Result<Uri, E> {
            Uri::try_from(v).map_err(E::custom)
        }

        serde_delegate!(visit_bytes visit_byte_buf);
    }

    d.deserialize_string(Visitor)
}
