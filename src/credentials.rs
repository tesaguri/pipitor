use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
pub struct Credentials {
    pub twitter: Twitter,
    #[serde(skip)]
    _non_exhaustive: (),
}

#[derive(Clone, Debug, Deserialize)]
pub struct Twitter {
    #[serde(with = "CredentialsDef")]
    pub client: oauth1::Credentials<Box<str>>,
    #[serde(skip)]
    _non_exhaustive: (),
}

#[derive(Deserialize)]
#[serde(remote = "oauth1::Credentials")]
struct CredentialsDef<T> {
    #[serde(rename = "key")]
    identifier: T,
    secret: T,
}
