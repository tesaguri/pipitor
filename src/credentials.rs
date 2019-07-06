use serde::Deserialize;

use crate::twitter;

#[derive(Clone, Debug, Deserialize)]
pub struct Credentials {
    pub twitter: Twitter,
    #[serde(skip)]
    _non_exhaustive: (),
}

#[derive(Clone, Debug, Deserialize)]
pub struct Twitter {
    pub client: twitter::Credentials<Box<str>>,
    #[serde(skip)]
    _non_exhaustive: (),
}
