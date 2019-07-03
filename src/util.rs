use hyper::client::connect::Connect;
use serde::{de, Deserialize};

use crate::app::core::Core;
use crate::twitter;

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Maybe<T> {
    Just(T),
    Nothing(de::IgnoredAny),
}

pub trait TwitterRequestExt: twitter::Request {
    fn send<C>(
        &self,
        core: &Core<C>,
        user: impl Into<Option<i64>>,
    ) -> twitter::ResponseFuture<Self::Data>
    where
        C: Connect + Sync + 'static,
        C::Transport: 'static,
        C::Future: 'static;
}

impl<T> TwitterRequestExt for T
where
    T: twitter::Request,
{
    fn send<C>(
        &self,
        core: &Core<C>,
        user: impl Into<Option<i64>>,
    ) -> twitter::ResponseFuture<Self::Data>
    where
        C: Connect + Sync + 'static,
        C::Transport: 'static,
        C::Future: 'static,
    {
        let user = user.into().unwrap_or_else(|| core.manifest().twitter.user);
        twitter::Request::send(
            self,
            core.manifest().twitter.client.as_ref(),
            core.twitter_token(user).unwrap(),
            core.http_client(),
        )
    }
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
