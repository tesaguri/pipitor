use crate::twitter;
use crate::util::HttpService;

use super::Core;

pub trait TwitterRequestExt: twitter::Request {
    fn send<S>(
        &self,
        core: &Core<S>,
        user: impl Into<Option<i64>>,
    ) -> twitter::ResponseFuture<Self::Data, S::Future>
    where
        S: HttpService<hyper::Body> + Clone;
}

impl<T> TwitterRequestExt for T
where
    T: twitter::Request,
{
    fn send<S>(
        &self,
        core: &Core<S>,
        user: impl Into<Option<i64>>,
    ) -> twitter::ResponseFuture<Self::Data, S::Future>
    where
        S: HttpService<hyper::Body> + Clone,
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
