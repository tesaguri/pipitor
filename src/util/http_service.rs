use std::error::Error;
use std::task::{Context, Poll};

use http::{Request, Response};
use http_body::Body;
use tower_service::Service;

use super::{http_response_future::IntoFuture, HttpResponseFuture};

pub trait HttpService<B> {
    type ResponseBody: Body;
    type Error: Error + Send + Sync + 'static;
    type Future: HttpResponseFuture<Body = Self::ResponseBody, Error = Self::Error>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>>;
    fn call(&mut self, request: Request<B>) -> Self::Future;

    fn into_service(self) -> IntoService<Self>
    where
        Self: Sized,
    {
        IntoService(self)
    }
}

pub struct IntoService<S>(S);

impl<S, ReqB, ResB: Body> HttpService<ReqB> for S
where
    S: Service<Request<ReqB>, Response = Response<ResB>>,
    S::Error: Error + Send + Sync + 'static,
{
    type ResponseBody = ResB;
    type Error = S::Error;
    type Future = S::Future;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        Service::poll_ready(self, cx)
    }

    fn call(&mut self, request: Request<ReqB>) -> Self::Future {
        Service::call(self, request)
    }
}

impl<S, B> Service<Request<B>> for IntoService<S>
where
    S: HttpService<B>,
{
    type Response = Response<S::ResponseBody>;
    type Error = S::Error;
    type Future = IntoFuture<S::Future>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), S::Error>> {
        HttpService::poll_ready(&mut self.0, cx)
    }

    fn call(&mut self, request: Request<B>) -> Self::Future {
        HttpService::call(&mut self.0, request).into_future()
    }
}
