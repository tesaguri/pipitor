mod api;
mod models;

pub use models::*;

pub(crate) use api::*;

use std::borrow::Borrow;

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Credentials<T = String> {
    pub key: T,
    pub secret: T,
}

impl<T> Credentials<T> {
    pub fn map<U, F>(self, mut f: F) -> Credentials<U>
    where
        U: Borrow<str>,
        F: FnMut(T) -> U,
    {
        Credentials {
            key: f(self.key),
            secret: f(self.secret),
        }
    }
}

impl<T: Borrow<str>> Credentials<T> {
    pub fn as_ref(&self) -> Credentials<&str> {
        Credentials {
            key: self.key.borrow(),
            secret: self.secret.borrow(),
        }
    }
}
