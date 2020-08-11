use std::borrow::Cow;
use std::fmt;
use std::str;

use regex::Regex;
use serde::{de, de::Error as _, Deserialize};

/// A wrapper struct to override `serde::Deserialize` behavior of `T`.
pub struct Serde<T>(pub T);

impl<'de> Deserialize<'de> for Serde<Cow<'de, str>> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> de::Visitor<'de> for Visitor {
            type Value = Cow<'de, str>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a string")
            }

            fn visit_str<E>(self, s: &str) -> Result<Self::Value, E> {
                Ok(Cow::Owned(s.to_owned()))
            }

            fn visit_borrowed_str<E>(self, s: &'de str) -> Result<Self::Value, E> {
                Ok(Cow::Borrowed(s))
            }

            fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
                Ok(Cow::Owned(v))
            }

            serde_delegate!(visit_bytes visit_borrowed_bytes visit_byte_buf);
        }

        d.deserialize_string(Visitor).map(Serde)
    }
}

impl<'de> Deserialize<'de> for Serde<Regex> {
    fn deserialize<D: de::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = <Serde<Cow<'de, str>>>::deserialize(d)?.0;
        s.parse().map(Serde).map_err(D::Error::custom)
    }
}
