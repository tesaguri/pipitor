macro_rules! api_requests {
    (POST $($rest:tt)+) => { api_requests! { @inner true; POST $($rest)+ } };
    ($method:ident $($rest:tt)+) => { api_requests! { @inner false; $method $($rest)+ } };
    (
        @inner $form:expr; $method:ident $uri:expr => $Data:ty;
        $(#[$attr:meta])*
        pub struct $Name:ident {
            $($(#[$req_attr:meta])* $required:ident: $req_ty:ty),*;
            $($(#[$opt_attr:meta])* $optional:ident: $opt_ty:ty $(= $default:expr)?),* $(,)?
        }
        $($rest:tt)*
    ) => {
        $(#[$attr])*
        #[derive(oauth1_request_derive::OAuth1Authorize)]
        pub struct $Name {
            $($(#[$req_attr])* $required: $req_ty,)*
            $($(#[$opt_attr])* $optional: $opt_ty,)*
        }

        impl $Name {
            pub fn new($($required: $req_ty),*) -> Self {
                #[allow(unused_macros)]
                macro_rules! this_or_default {
                    ($this:expr) => ($this);
                    () => (Default::default());
                }

                $Name {
                    $($required,)*
                    $($optional: this_or_default!($($default)?),)*
                }
            }

            $(
                #[allow(dead_code)]
                pub fn $optional(&mut self, $optional: $opt_ty) -> &mut Self {
                    self.$optional = $optional;
                    self
                }
            )*
        }

        impl $crate::twitter::Request for $Name {
            type Data = $Data;

            const METHOD: hyper::Method = hyper::Method::$method;
            const FORM: bool = $form;
            const URI: &'static str = $uri;
        }

        api_requests! { $($rest)* }
    };
    () => ();
}

pub mod account;
pub mod lists;
pub mod oauth;
pub mod statuses;

use std::error;
use std::fmt::{self, Display, Formatter};
use std::marker::PhantomData;
use std::ops::Deref;
use std::pin::Pin;

use failure::Fail;
use futures::compat::{Compat01As03, Future01CompatExt, Stream01CompatExt};
use futures::task::Context;
use futures::{try_ready, Future, FutureExt, Poll, TryStreamExt};
use futures_util::try_stream::TryConcat;
use hyper::client::connect::Connect;
use hyper::client::{Client, ResponseFuture as HyperResponseFuture};
use hyper::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use hyper::{Body, Method, StatusCode, Uri};
use oauth1::OAuth1Authorize;
use serde::{de, Deserialize};

use super::Credentials;

pub struct Response<T> {
    pub response: T,
    pub rate_limit: Option<RateLimit>,
}

pub struct ResponseFuture<T> {
    inner: ResponseFutureInner,
    marker: PhantomData<fn() -> T>,
}

enum ResponseFutureInner {
    Resp(HyperResponseFuture),
    Body {
        status: StatusCode,
        rate_limit: Option<RateLimit>,
        body: TryConcat<Compat01As03<Body>>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct RateLimit {
    pub limit: u64,
    pub remaining: u64,
    pub reset: u64,
}

#[derive(Debug, Fail)]
pub enum Error {
    #[fail(display = "Failed to deserialize the response body.")]
    Deserializing(#[cause] json::Error),
    #[fail(display = "HTTP error")]
    Hyper(#[cause] hyper::Error),
    #[fail(display = "Twitter returned error(s)")]
    Twitter(#[cause] TwitterErrors),
    #[fail(display = "Unexpected error occured.")]
    Unexpected,
}

#[derive(Debug)]
pub struct TwitterErrors {
    pub status: StatusCode,
    pub errors: Vec<ErrorCode>,
    pub rate_limit: Option<RateLimit>,
}

#[derive(Debug, Deserialize)]
pub struct ErrorCode {
    pub code: u32,
    pub message: String,
}

pub type Result<T> = std::result::Result<T, Error>;

pub trait Request: OAuth1Authorize {
    type Data: de::DeserializeOwned;

    const METHOD: Method;
    const FORM: bool;
    const URI: &'static str;

    fn send<'a, C>(
        &self,
        client_credentials: Credentials<&str>,
        token_credentials: Credentials<&'a str>,
        client: &Client<C>,
    ) -> ResponseFuture<Self::Data>
    where
        C: Connect + Sync + 'static,
        C::Transport: 'static,
        C::Future: 'static,
    {
        let oauth1::Request {
            authorization,
            data,
        } = if Self::FORM {
            OAuth1Authorize::authorize_form
        } else {
            OAuth1Authorize::authorize
        }(
            self,
            Self::METHOD.as_str(),
            Self::URI,
            client_credentials.key,
            client_credentials.secret,
            token_credentials.secret,
            oauth1::HmacSha1,
            &*oauth1::Options::new().token(token_credentials.key),
        );

        trace!("{} {}", Self::METHOD, Self::URI);
        trace!("data: {}", data);

        let mut req = hyper::Request::builder();
        req.method(Self::METHOD)
            .header(AUTHORIZATION, authorization);

        let req = if Self::FORM {
            req.uri(Uri::from_static(Self::URI))
                .header(
                    CONTENT_TYPE,
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                )
                .body(data.into_bytes().into())
                .unwrap()
        } else {
            req.uri(data).body(Default::default()).unwrap()
        };

        ResponseFuture {
            inner: ResponseFutureInner::Resp(client.request(req)),
            marker: PhantomData,
        }
    }
}

impl<T> Deref for Response<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.response
    }
}

impl<T: de::DeserializeOwned> Future for ResponseFuture<T> {
    type Output = Result<Response<T>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Result<Response<T>>> {
        trace_fn!(ResponseFuture::<T>::poll);

        if let ResponseFutureInner::Resp(ref mut res) = self.inner {
            let res = try_ready!(res.compat().poll_unpin(cx).map_err(Error::Hyper));
            trace!("response={:?}", res);

            self.inner = ResponseFutureInner::Body {
                status: res.status(),
                rate_limit: rate_limit(&res),
                body: res.into_body().compat().try_concat(),
            };
        }

        if let ResponseFutureInner::Body {
            status,
            rate_limit,
            ref mut body,
        } = self.inner
        {
            let json = try_ready!(body.poll_unpin(cx).map_err(Error::Hyper));

            trace!("done reading response body");

            Poll::Ready(make_response(status, rate_limit, &json, |json| {
                json::from_slice(json).map_err(Error::Deserializing)
            }))
        } else {
            unreachable!();
        }
    }
}

impl Display for TwitterErrors {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "status: {}", self.status)?;

        let mut errors = self.errors.iter();
        if let Some(e) = errors.next() {
            write!(f, "; errors: {}", e)?;
            for e in errors {
                write!(f, ", {}", e)?;
            }
        }

        Ok(())
    }
}

impl error::Error for TwitterErrors {}

impl Display for ErrorCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.code, self.message)
    }
}

fn make_response<T, F>(
    status: StatusCode,
    rate_limit: Option<RateLimit>,
    body: &[u8],
    parse: F,
) -> Result<Response<T>>
where
    F: FnOnce(&[u8]) -> Result<T>,
{
    if let StatusCode::OK = status {
        parse(body).map(|response| Response {
            response,
            rate_limit,
        })
    } else {
        #[derive(Default, Deserialize)]
        struct Errors {
            errors: Vec<ErrorCode>,
        }
        json::from_slice(body)
            .or_else(|_| Ok(Errors::default()))
            .and_then(|errors| {
                Err(Error::Twitter(TwitterErrors {
                    status,
                    errors: errors.errors,
                    rate_limit,
                }))
            })
    }
}

fn rate_limit<T>(res: &hyper::Response<T>) -> Option<RateLimit> {
    Some(RateLimit {
        limit: header(&res, "x-rate-limit-limit")?,
        remaining: header(&res, "x-rate-limit-remaining")?,
        reset: header(&res, "x-rate-limit-reset")?,
    })
}

fn header<T>(res: &hyper::Response<T>, name: &str) -> Option<u64> {
    res.headers()
        .get(name)
        .and_then(|value| atoi::atoi(value.as_bytes()))
}
