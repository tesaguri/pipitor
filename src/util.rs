macro_rules! serde_delegate {
    (visit_str $($rest:tt)*) => {
        fn visit_str<E: de::Error>(self, s: &str) -> Result<Self::Value, E> {
            self.visit_bytes(s.as_bytes())
        }
    };
    (visit_bytes $($rest:tt)*) => {
        fn visit_bytes<E: de::Error>(self, v: &[u8]) -> Result<Self::Value, E> {
            std::str::from_utf8(v).map_err(E::custom).and_then(|s| self.visit_str(s))
        }
        serde_delegate!($($rest)*);
    };
    (visit_borrowed_bytes $($rest:tt)*) => {
        fn visit_borrowed_bytes<E: de::Error>(self, v: &'de [u8]) -> Result<Self::Value, E> {
            std::str::from_utf8(v).map_err(E::custom).and_then(|s| self.visit_borrowed_str(s))
        }
        serde_delegate!($($rest)*);
    };
    (visit_byte_buf $($rest:tt)*) => {
        fn visit_byte_buf<E: de::Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
            String::from_utf8(v).map_err(E::custom).and_then(|s| self.visit_string(s))
        }
        serde_delegate!($($rest)*);
    };
    () => {};
}

pub mod consts {
    pub const APPLICATION_WWW_FORM_URLENCODED: &str = "application/x-www-form-urlencoded";
    #[cfg(test)]
    pub const APPLICATION_ATOM_XML: &str = "application/atom+xml";
    pub const HUB_SIGNATURE: &str = "x-hub-signature";
    pub const NS_ATOM: &str = "http://www.w3.org/2005/Atom";
    pub const RATE_LIMIT_LIMIT: &str = "x-rate-limit-limit";
    pub const RATE_LIMIT_REMAINING: &str = "x-rate-limit-remaining";
    pub const RATE_LIMIT_RESET: &str = "x-rate-limit-reset";
}

#[cfg(test)]
pub mod connection;
pub mod http_service;
pub mod r2d2;
pub mod time;

mod serde_wrapper;

use std::convert::TryInto;
use std::error::Error;
use std::fmt::{self, Display};
use std::fs;
use std::io;
use std::marker::PhantomData;
use std::mem;

use std::pin::Pin;
use std::str;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};

use anyhow::Context as _;
use bytes::Buf;
use futures::{ready, Future};
use http_body::Body;
use pin_project::pin_project;
use serde::{de, Deserialize};
use tower_service::Service;

use crate::Credentials;

#[cfg(test)]
pub use self::connection::connection;
pub use self::http_service::HttpService;
pub use self::serde_wrapper::{MapAccessDeserializer, SeqAccessDeserializer, Serde};
pub use self::time::{instant_from_unix, instant_now, now_unix, system_time_now};
#[cfg(test)]
pub use self::time::{FutureTimeoutExt, Sleep};

pub struct ArcService<S>(pub Arc<S>);

#[pin_project]
pub struct ConcatBody<B> {
    #[pin]
    body: B,
    accum: Vec<u8>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Maybe<T> {
    Just(T),
    Nothing(de::IgnoredAny),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Hash)]
pub enum Never {}

macro_rules! trace_fn {
    (@heading $path:path) => {
        concat!(file!(), ':', line!(), ':', column!(), ' ', stringify!($path))
    };
    ($path:path) => {{
        // Ensure that at least the symbol `$path` exists.
        #[allow(path_statements)] { $path; }
        trace!(trace_fn!(@heading $path));
    }};
    ($path:path, $dbg1:expr $(, $dbg:expr)*) => {{
        #[allow(path_statements)] { $path; }
        trace!(
            concat!(
                trace_fn!(@heading $path), "; ",
                stringify!($dbg1), "={:?}",
                $(", ", stringify!($dbg), "={:?}",)*
            ),
            $dbg1, $($dbg,)*
        );
    }};
}

impl<S, T, R, E, F> Service<T> for ArcService<S>
where
    for<'a> &'a S: Service<T, Response = R, Error = E, Future = F>,
    F: Future<Output = Result<R, E>>,
{
    type Response = R;
    type Error = E;
    type Future = F;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        (&*self.0).poll_ready(cx)
    }

    fn call(&mut self, req: T) -> Self::Future {
        (&*self.0).call(req)
    }
}

impl<B: Body> ConcatBody<B> {
    pub fn new(body: B) -> Self {
        let cap = body.size_hint().lower();
        ConcatBody {
            body,
            accum: Vec::with_capacity(cap.try_into().unwrap_or(0)),
        }
    }
}

impl<B: Body> Future for ConcatBody<B> {
    type Output = Result<Vec<u8>, B::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        loop {
            match ready!(this.body.as_mut().poll_data(cx)?) {
                Some(buf) => this.accum.extend(buf.bytes()),
                None => return Poll::Ready(Ok(mem::take(this.accum))),
            }
        }
    }
}

impl Display for Never {
    fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {}
    }
}

impl Error for Never {}

impl tokio::io::AsyncRead for Never {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        match *self {}
    }
}

impl tokio::io::AsyncWrite for Never {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
        match *self {}
    }

    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        match *self {}
    }

    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        match *self {}
    }
}

pub fn deserialize_from_str<'de, T, D>(d: D) -> Result<T, D::Error>
where
    T: FromStr,
    D: de::Deserializer<'de>,
{
    struct Visitor<T>(PhantomData<T>);

    impl<'de, T> de::Visitor<'de> for Visitor<T>
    where
        T: FromStr,
    {
        type Value = T;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(std::any::type_name::<T>())
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
            v.parse()
                .map_err(|_| E::invalid_value(de::Unexpected::Str(v), &self))
        }

        serde_delegate!(visit_bytes);
    }

    d.deserialize_str(Visitor::<T>(PhantomData))
}

pub fn open_credentials(path: &str) -> anyhow::Result<Credentials> {
    let ret = fs::read(path)
        .context("failed to open the credentials file")
        .and_then(|buf| toml::from_slice(&buf).context("failed to parse the credentials file"))?;
    Ok(ret)
}
