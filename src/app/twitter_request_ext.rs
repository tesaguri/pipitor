use twitter_client::traits::HttpService;

use super::Core;

pub trait TwitterRequestExt: twitter_client::Request {
    fn send<S, B>(
        &self,
        core: &Core<S>,
        user: impl Into<Option<i64>>,
    ) -> twitter_client::response::ResponseFuture<Self::Data, S::Future>
    where
        S: HttpService<B> + Clone,
        B: Default + From<Vec<u8>>;
}

impl<T> TwitterRequestExt for T
where
    T: twitter_client::Request,
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
    ) -> twitter_client::response::ResponseFuture<Self::Data, S::Future>
    where
        S: HttpService<B> + Clone,
        B: Default + From<Vec<u8>>,
    {
        let token = core.twitter_token(user.into()).unwrap();
        twitter_client::Request::send(self, &token, &mut core.http_client().clone())
    }
}
