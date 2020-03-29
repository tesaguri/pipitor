pub mod http_service;

use std::convert::TryInto;
use std::error::Error;
use std::fmt::{self, Display};
use std::fs;
use std::io;
use std::marker::{PhantomData, Unpin};
use std::mem;
use std::ops::Range;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use bytes::Buf;
use futures::{ready, Future};
use http_body::Body;
use pin_project::{pin_project, project};
use serde::{de, Deserialize};
use tower_service::Service;

use crate::Credentials;

pub use self::http_service::HttpService;

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

/// A future that resolves to `(F::Output, T).
#[pin_project]
pub struct ResolveWith<F, T> {
    #[pin]
    future: F,
    value: Option<T>,
}

macro_rules! trace_fn {
    (@heading $path:path) => {
        concat!(file!(), ':', line!(), ':', column!(), ' ', stringify!($path));
    };
    ($path:path) => {{
        // Ensure that at least the symbol `$path` exists.
        #[allow(path_statements)] { $path; }
        trace!(trace_fn!(@heading $path));
    }};
    ($path:path, $fmt:tt $($arg:tt)*) => {{
        #[allow(path_statements)] { $path; }
        trace!(concat!(trace_fn!(@heading $path), "; ", $fmt) $($arg)*);
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

impl<F, T> ResolveWith<F, T>
where
    F: Future,
    T: Unpin,
{
    pub fn new(future: F, value: T) -> Self {
        ResolveWith {
            future,
            value: Some(value),
        }
    }
}

impl<F, T> Future for ResolveWith<F, T>
where
    F: Future,
    T: Unpin,
{
    type Output = (F::Output, T);

    #[project]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        #[project]
        let ResolveWith { future, value } = self.project();

        future.poll(cx).map(|x| {
            let y = value.take().expect("polled `ResolveWith` after completion");
            (x, y)
        })
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
    D: serde::Deserializer<'de>,
{
    struct Visitor<T>(PhantomData<T>);

    impl<'de, T> serde::de::Visitor<'de> for Visitor<T>
    where
        T: FromStr,
    {
        type Value = T;

        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(std::any::type_name::<T>())
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
            v.parse()
                .map_err(|_| E::invalid_value(serde::de::Unexpected::Str(v), &self))
        }
    }

    d.deserialize_str(Visitor::<T>(PhantomData))
}

pub fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub fn open_credentials(path: &str) -> anyhow::Result<Credentials> {
    let ret = fs::read(path)
        .context("failed to open the credentials file")
        .and_then(|buf| toml::from_slice(&buf).context("failed to parse the credentials file"))?;
    Ok(ret)
}

pub fn snowflake_to_system_time(id: u64) -> SystemTime {
    // timestamp_ms = (snowflake >> 22) + 1_288_834_974_657
    let snowflake_time_ms = id >> 22;
    let timestamp = Duration::new(
        snowflake_time_ms / 1_000 + 1_288_834_974,
        (snowflake_time_ms as u32 % 1_000 + 657) * 1_000 * 1_000,
    );
    UNIX_EPOCH + timestamp
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
