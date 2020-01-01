use std::error::Error;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::{Future, TryFuture};
use http::Response;
use http_body::Body;
use pin_project::pin_project;

pub trait HttpResponseFuture {
    type Body: Body;
    type Error: Error + Send + Sync + 'static;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Response<Self::Body>, Self::Error>>;

    fn into_future(self) -> IntoFuture<Self>
    where
        Self: Sized,
    {
        IntoFuture(self)
    }
}

#[pin_project]
pub struct IntoFuture<F>(#[pin] F);

impl<F: TryFuture<Ok = Response<B>>, B: Body> HttpResponseFuture for F
where
    F::Error: Error + Send + Sync + 'static,
{
    type Body = B;
    type Error = <Self as TryFuture>::Error;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<Response<B>, Self::Error>> {
        TryFuture::try_poll(self, cx)
    }
}

impl<F: HttpResponseFuture> Future for IntoFuture<F> {
    type Output = Result<Response<F::Body>, F::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        HttpResponseFuture::poll(self.project().0, cx)
    }
}
