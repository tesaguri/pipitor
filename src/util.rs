use serde::{de, Deserialize};

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Maybe<T> {
    Just(T),
    Nothing(de::IgnoredAny),
}
