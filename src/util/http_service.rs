use std::error::Error;
use std::future::Future;
use std::task::{Context, Poll};

use http::header::{HeaderValue, USER_AGENT};
use http::{Request, Response};
use http_body::Body;

/// A wrapper for `impl tower_service::Service` to adjust its behavior for our usage.
#[derive(Clone)]
pub struct Service<S> {
    inner: S,
}

pub struct IntoService<S>(S);

pub trait HttpService<B> {
    type ResponseBody: Body;
    type Error: Error + Send + Sync + 'static;
    type Future: Future<Output = Result<Response<Self::ResponseBody>, Self::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;
    fn call(&mut self, request: Request<B>) -> Self::Future;

    fn into_service(self) -> IntoService<Self>
    where
        Self: Sized,
    {
        IntoService(self)
    }
}

const USER_AGENT_PIPITOR: &str = concat!("Pipitor/", env!("CARGO_PKG_VERSION"));

impl<S> Service<S> {
    pub fn new(inner: S) -> Self {
        Service { inner }
    }

    pub fn get_ref(&self) -> &S {
        &self.inner
    }
}

impl<S: HttpService<B>, B> tower_service::Service<Request<B>> for Service<S> {
    type Response = Response<S::ResponseBody>;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, mut request: Request<B>) -> Self::Future {
        let ua = HeaderValue::from_static(USER_AGENT_PIPITOR);
        request.headers_mut().insert(USER_AGENT, ua);
        self.inner.call(request)
    }
}

impl<S, ReqB, ResB: Body> HttpService<ReqB> for S
where
    S: tower_service::Service<Request<ReqB>, Response = Response<ResB>>,
    S::Error: Error + Send + Sync + 'static,
{
    type ResponseBody = ResB;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        tower_service::Service::poll_ready(self, cx)
    }

    fn call(&mut self, request: Request<ReqB>) -> Self::Future {
        tower_service::Service::call(self, request)
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
