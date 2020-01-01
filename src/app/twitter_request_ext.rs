use crate::twitter;
use crate::util::HttpService;

use super::Core;

pub trait TwitterRequestExt: twitter::Request {
    fn send<S, B>(
        &self,
        core: &Core<S>,
        user: impl Into<Option<i64>>,
    ) -> twitter::ResponseFuture<Self::Data, S::Future>
    where
        S: HttpService<B> + Clone,
        B: Default + From<Vec<u8>>;
}

impl<T> TwitterRequestExt for T
where
    T: twitter::Request,
{
    fn send<S, B>(
        &self,
        core: &Core<S>,
        user: impl Into<Option<i64>>,
    ) -> twitter::ResponseFuture<Self::Data, S::Future>
    where
        S: HttpService<B> + Clone,
        B: Default + From<Vec<u8>>,
    {
        let user = user.into().unwrap_or_else(|| core.manifest().twitter.user);
        twitter::Request::send(
            self,
            core.credentials().twitter.client.as_ref(),
            core.twitter_token(user).unwrap(),
            core.http_client().clone(),
        )
    }
}
