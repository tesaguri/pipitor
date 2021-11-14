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
    use http::header::HeaderValue;

    // <https://github.com/rust-lang/rust-clippy/issues/5812>
    #[allow(clippy::declare_interior_mutable_const)]
    pub const APPLICATION_WWW_FORM_URLENCODED: HeaderValue =
        HeaderValue::from_static("application/x-www-form-urlencoded");
    #[cfg(test)]
    #[allow(clippy::declare_interior_mutable_const)]
    pub const APPLICATION_ATOM_XML: HeaderValue = HeaderValue::from_static("application/atom+xml");
    pub const HUB_SIGNATURE: &str = "x-hub-signature";
    pub const NS_ATOM: &str = "http://www.w3.org/2005/Atom";
    pub const NS_MRSS: &str = "http://search.yahoo.com/mrss/";
}

#[cfg(test)]
pub mod connection;
pub mod http_service;
pub mod r2d2;
pub mod time;

mod concat_body;
mod first;
mod serde_wrapper;

use std::convert::TryFrom;
use std::error::Error;
use std::fmt::{self, Display};
use std::io;
use std::marker::PhantomData;
use std::ops::Range;
use std::pin::Pin;
use std::str;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cfg_if::cfg_if;
use futures::Future;
use serde::{de, Deserialize};
use tokio::io::ReadBuf;

pub use self::concat_body::ConcatBody;
#[cfg(test)]
pub use self::connection::connection;
pub use self::first::{first, First};
pub use self::http_service::Service;
pub use self::serde_wrapper::{MapAccessDeserializer, SeqAccessDeserializer, Serde};
pub use self::time::{instant_from_unix, instant_now, now_unix, system_time_now};
#[cfg(test)]
pub use self::time::{FutureTimeoutExt, Sleep};

pub struct ArcService<S>(pub Arc<S>);

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
        let _ = &$path;
        trace!(trace_fn!(@heading $path));
    }};
    ($path:path, $dbg1:expr $(, $dbg:expr)*) => {{
        let _ = &$path;
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

impl<S, T, R, E, F> tower_service::Service<T> for ArcService<S>
where
    for<'a> &'a S: tower_service::Service<T, Response = R, Error = E, Future = F>,
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
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
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

pub fn replace_char_range(
    s: &mut String,
    base_byte_offset: usize,
    range: Range<usize>,
    replace_with: &str,
) -> Option<usize> {
    let start = if let Some(i) = char_index_to_byte_index(&s[base_byte_offset..], range.start) {
        base_byte_offset + i
    } else {
        return None;
    };
    let end = if let Some(i) = char_index_to_byte_index(&s[start..], range.end - range.start) {
        start + i
    } else {
        return None;
    };

    s.replace_range(start..end, replace_with);

    Some(start)
}

pub fn char_index_to_byte_index(s: &str, char_index: usize) -> Option<usize> {
    if char_index == 0 {
        return Some(0);
    }

    let (mut bi, mut ci) = (0, 0);
    for c in s.chars() {
        bi += c.len_utf8();
        ci += 1;
        if ci == char_index {
            return Some(bi);
        }
    }

    None
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

const TWEPOCH: u128 = 1288834974657;

pub fn snowflake_to_system_time(id: u64) -> SystemTime {
    // timestamp_ms = (snowflake >> 22) + TWEPOCH
    let snowflake_time_ms = id >> 22;
    let timestamp = Duration::new(
        snowflake_time_ms / 1_000 + (TWEPOCH / 1000) as u64,
        ((snowflake_time_ms % 1_000) as u32 + (TWEPOCH % 1000) as u32) * 1_000 * 1_000,
    );
    UNIX_EPOCH + timestamp
}

pub fn system_time_to_snowflake(t: SystemTime) -> u64 {
    let unix = t.duration_since(UNIX_EPOCH).unwrap();
    u64::try_from((unix.as_millis() - TWEPOCH) << 22).unwrap()
}

cfg_if! {
    if #[cfg(test)] {
        use futures::future;

        pub trait EitherUnwrapExt {
            type Left;
            type Right;

            fn unwrap_left(self) -> Self::Left;
        }

        impl<A, B> EitherUnwrapExt for future::Either<A, B> {
            type Left = A;
            type Right = B;

            #[track_caller]
            fn unwrap_left(self) -> A {
                match self {
                    future::Either::Left(a) => a,
                    future::Either::Right(_) => {
                        panic!("called `Either::unwrap_left()` on a `Right` value");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace() {
        let mut s = String::from("Pipitor the Twitter bot");
        let start = replace_char_range(&mut s, 0, 12..19, "social media");
        assert_eq!(s, "Pipitor the social media bot");
        assert_eq!(start, Some(12));

        s = String::from("メロスは激怒した。");
        let start = replace_char_range(&mut s, 0, 5..8, "おこ");
        assert_eq!(s, "メロスは激おこ。");
        assert_eq!(start, Some(15));

        let start = replace_char_range(&mut s, 15, 0..2, "怒");
        assert_eq!(s, "メロスは激怒。");
        assert_eq!(start, Some(15));
    }
}
