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
        #[derive(oauth1::Authorize)]
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

            const METHOD: http::Method = http::Method::$method;
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
use std::task::{Context, Poll};

use flate2::bufread::GzDecoder;
use futures::{ready, Future};
use http::header::{HeaderValue, ACCEPT_ENCODING, AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE};
use http::{Method, StatusCode, Uri};
use http_body::Body;
use oauth1::Credentials;
use pin_project::{pin_project, project};
use serde::{de, Deserialize};
use tower_util::{Oneshot, ServiceExt};

use crate::util::http_service::{HttpService, IntoService};
use crate::util::ConcatBody;

pub struct Response<T> {
    pub data: T,
    pub rate_limit: Option<RateLimit>,
}

#[pin_project]
pub struct ResponseFuture<T, S: HttpService<B>, B> {
    #[pin]
    inner: ResponseFutureInner<S, B>,
    marker: PhantomData<fn() -> T>,
}

#[pin_project]
enum ResponseFutureInner<S: HttpService<B>, B> {
    Resp(#[pin] Oneshot<IntoService<S>, http::Request<B>>),
    Body {
        status: StatusCode,
        rate_limit: Option<RateLimit>,
        #[pin]
        body: ConcatBody<S::ResponseBody>,
        gzip: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct RateLimit {
    pub limit: u64,
    pub remaining: u64,
    pub reset: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum Error<SE, BE>
where
    SE: std::error::Error + 'static,
    BE: std::error::Error + 'static,
{
    #[error("Failed to deserialize the response body.")]
    Deserializing(#[source] json::Error),
    #[error("HTTP error")]
    Service(#[source] SE),
    #[error("Error while reading the response body")]
    Body(#[source] BE),
    #[error("Twitter returned error(s)")]
    Twitter(#[source] TwitterErrors),
    #[error("Unexpected error occured.")]
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

pub trait Request: oauth1::Authorize {
    type Data: de::DeserializeOwned;

    const METHOD: Method;
    const FORM: bool;
    const URI: &'static str;

    fn send<'a, S, B>(
        &self,
        client_credentials: Credentials<&str>,
        token_credentials: Credentials<&'a str>,
        client: S,
    ) -> ResponseFuture<Self::Data, S, B>
    where
        S: HttpService<B>,
        B: Default + From<Vec<u8>>,
    {
        let mut builder = oauth1::Builder::new(client_credentials, oauth1::HmacSha1);
        builder.token(token_credentials);
        let oauth1::Request {
            authorization,
            data,
        } = if Self::FORM {
            builder.build_form(Self::METHOD.as_str(), Self::URI, self)
        } else {
            builder.build(Self::METHOD.as_str(), Self::URI, self)
        };

        trace!("{} {}", Self::METHOD, Self::URI);
        trace!("data: {}", data);

        let req = http::Request::builder()
            .method(Self::METHOD)
            .header(ACCEPT_ENCODING, HeaderValue::from_static("gzip"))
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
            inner: ResponseFutureInner::Resp(client.into_service().oneshot(req)),
            marker: PhantomData,
        }
    }
}

impl<T> Deref for Response<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.data
    }
}

impl<T: de::DeserializeOwned, S, B> Future for ResponseFuture<T, S, B>
where
    S: HttpService<B>,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    type Output = Result<Response<T>, Error<S::Error, <S::ResponseBody as Body>::Error>>;

    #[project]
    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<Response<T>, Error<S::Error, <S::ResponseBody as Body>::Error>>> {
        trace_fn!(ResponseFuture::<T, S, B>::poll);

        #[project]
        match self.as_mut().project().inner.project() {
            ResponseFutureInner::Resp(res) => {
                let res = ready!(res.poll(cx).map_err(Error::Service))?;

                let gzip = res
                    .headers()
                    .get_all(CONTENT_ENCODING)
                    .iter()
                    .any(|e| e == "gzip");

                self.as_mut()
                    .project()
                    .inner
                    .set(ResponseFutureInner::Body {
                        status: res.status(),
                        rate_limit: rate_limit(&res),
                        body: ConcatBody::new(res.into_body()),
                        gzip,
                    });
            }
            _ => {}
        }

        #[project]
        match self.project().inner.project() {
            ResponseFutureInner::Body {
                status,
                rate_limit,
                body,
                gzip,
            } => {
                let body = ready!(body.poll(cx).map_err(Error::Body))?;

                trace!("done reading response body");

                Poll::Ready(make_response(*status, *rate_limit, &body, |body| {
                    if *gzip {
                        json::from_reader(GzDecoder::new(body)).map_err(Error::Deserializing)
                    } else {
                        json::from_slice(body).map_err(Error::Deserializing)
                    }
                }))
            }
            _ => unreachable!(),
        }
    }
}

impl TwitterErrors {
    pub fn codes<'a>(&'a self) -> impl Iterator<Item = u32> + 'a {
        self.errors.iter().map(|e| e.code)
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

impl ErrorCode {
    pub const YOU_ARENT_ALLOWED_TO_ADD_MEMBERS_TO_THIS_LIST: u32 = 104;
    pub const CANNOT_FIND_SPECIFIED_USER: u32 = 108;
    pub const NO_STATUS_FOUND_WITH_THAT_ID: u32 = 144;
    pub const YOU_HAVE_ALREADY_RETWEETED_THIS_TWEET: u32 = 327;
}

impl Display for ErrorCode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.code, self.message)
    }
}

fn make_response<T, F, SE, BE>(
    status: StatusCode,
    rate_limit: Option<RateLimit>,
    body: &[u8],
    parse: F,
) -> Result<Response<T>, Error<SE, BE>>
where
    F: FnOnce(&[u8]) -> Result<T, Error<SE, BE>>,
    SE: std::error::Error + Send + Sync,
    BE: std::error::Error + Send + Sync,
{
    if let StatusCode::OK = status {
        parse(body).map(|data| Response { data, rate_limit })
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

fn rate_limit<T>(res: &http::Response<T>) -> Option<RateLimit> {
    Some(RateLimit {
        limit: header(res, "x-rate-limit-limit")?,
        remaining: header(res, "x-rate-limit-remaining")?,
        reset: header(res, "x-rate-limit-reset")?,
    })
}

fn header<T>(res: &http::Response<T>, name: &str) -> Option<u64> {
    res.headers()
        .get(name)
        .and_then(|value| atoi::atoi(value.as_bytes()))
}
