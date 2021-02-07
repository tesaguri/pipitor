use std::task::{Context, Poll};

use http::header::{HeaderValue, USER_AGENT};
use http::{Request, Response};
use tower_http::decompression::{self, Decompression};
use twitter_client::traits::HttpService;

/// A wrapper for `impl tower_service::Service` to adjust its behavior for our usage.
#[derive(Clone)]
pub struct Service<S> {
    inner: Decompression<IntoService<S>>,
}

#[derive(Clone)]
pub struct IntoService<S>(pub S);

#[allow(clippy::declare_interior_mutable_const)]
const USER_AGENT_PIPITOR: HeaderValue =
    HeaderValue::from_static(concat!("Pipitor/", env!("CARGO_PKG_VERSION")));

impl<S> Service<S> {
    pub fn new(inner: S) -> Self {
        Service {
            inner: Decompression::new(IntoService(inner)),
        }
    }

    pub fn get_ref(&self) -> &S {
        &self.inner.get_ref().0
    }
}

impl<S: HttpService<B>, B> tower_service::Service<Request<B>> for Service<S> {
    type Response = Response<decompression::DecompressionBody<S::ResponseBody>>;
    type Error = S::Error;
    type Future = decompression::ResponseFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        tower_service::Service::poll_ready(&mut self.inner, cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        request.headers_mut().insert(USER_AGENT, USER_AGENT_PIPITOR);
        tower_service::Service::call(&mut self.inner, request)
    }
}

impl<S, B> tower_service::Service<Request<B>> for IntoService<S>
where
    S: HttpService<B>,
{
    type Response = Response<S::ResponseBody>;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        HttpService::poll_ready(&mut self.0, cx)
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        HttpService::call(&mut self.0, request)
    }
}
