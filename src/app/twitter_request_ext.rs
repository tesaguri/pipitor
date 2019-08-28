use hyper::client::connect::Connect;

use crate::twitter;

use super::Core;

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
            core.credentials().twitter.client.as_ref(),
            core.twitter_token(user).unwrap(),
            core.http_client(),
        )
    }
}
