use crate::twitter;
use crate::util::HttpService;

use super::Core;

pub trait TwitterRequestExt: twitter::Request {
    fn send<S, B>(
        &self,
        core: &Core<S>,
        user: impl Into<Option<i64>>,
    ) -> twitter::ResponseFuture<Self::Data, S, B>
    where
        S: HttpService<B> + Clone,
        B: Default + From<Vec<u8>>;
}

impl<T> TwitterRequestExt for T
where
    T: twitter::Request,
{
    /// # Panics
    ///
    /// Panics if the Twitter API credentials for the client or the outbox user is unavailable,
    /// which should not occur given that the manifest has been `validate()`-d and the outbox user
    /// is taken from it.
    fn send<S, B>(
        &self,
        core: &Core<S>,
        user: impl Into<Option<i64>>,
    ) -> twitter::ResponseFuture<Self::Data, S, B>
    where
        S: HttpService<B> + Clone,
        B: Default + From<Vec<u8>>,
    {
        let config = core.manifest().twitter.as_ref().unwrap();
        let user = user.into().unwrap_or(config.user);
        twitter::Request::send(
            self,
            &config.client,
            &core.twitter_token(user).unwrap(),
            core.http_client().clone(),
        )
    }
}
