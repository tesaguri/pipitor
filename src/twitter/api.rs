macro_rules! api_requests {
    (
        $method:ident $uri:expr => $Data:ty;
        $(#[$attr:meta])*
        $vis:vis struct $Name:ident $(<$($lt:lifetime),*>)? {
            $($(#[$req_attr:meta])* $required:ident: $req_ty:ty),*;
            $($(#[$opt_attr:meta])* $optional:ident: $opt_ty:ty $(= $default:expr)?),* $(,)?
        }
        $($rest:tt)*
    ) => {
        $(#[$attr])*
        #[derive(oauth1::Authorize)]
        $vis struct $Name $(<$($lt),*>)? {
            $($(#[$req_attr])* $required: $req_ty,)*
            $($(#[$opt_attr])* $optional: $opt_ty,)*
        }

        impl $(<$($lt),*>)? $Name $(<$($lt),*>)? {
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

        impl $(<$($lt),*>)? $crate::twitter::Request for $Name $(<$($lt),*>)? {
            type Data = $Data;

            const METHOD: http::Method = http::Method::$method;
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

use std::borrow::Borrow;
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
use pin_project::pin_project;
use serde::{de, Deserialize};
use tower_util::{Oneshot, ServiceExt};

use crate::util::http_service::{HttpService, IntoService};
use crate::util::ConcatBody;

#[derive(Debug)]
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

#[pin_project(project = ResponseFutureInnerProj)]
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
    const URI: &'static str;

    fn send<C, T, S, B>(
        &self,
        client: &Credentials<C>,
        token: &Credentials<T>,
        http: S,
    ) -> ResponseFuture<Self::Data, S, B>
    where
        C: Borrow<str>,
        T: Borrow<str>,
        S: HttpService<B>,
        B: From<Vec<u8>>,
    {
        let req = prepare_request(
            &Self::METHOD,
            Self::URI,
            self,
            client.as_ref(),
            token.as_ref(),
        );

        ResponseFuture::new(req.map(Into::into), http)
    }
}

impl<T> Deref for Response<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.data
    }
}

impl<T: de::DeserializeOwned, S, B> ResponseFuture<T, S, B>
where
    S: HttpService<B>,
{
    fn new(req: http::Request<B>, http: S) -> Self {
        ResponseFuture {
            inner: ResponseFutureInner::Resp(http.into_service().oneshot(req)),
            marker: PhantomData,
        }
    }
}

impl<T: de::DeserializeOwned, S, B> Future for ResponseFuture<T, S, B>
where
    S: HttpService<B>,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    type Output = Result<Response<T>, Error<S::Error, <S::ResponseBody as Body>::Error>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        trace_fn!(ResponseFuture::<T, S, B>::poll);

        if let ResponseFutureInnerProj::Resp(res) = self.as_mut().project().inner.project() {
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

        if let ResponseFutureInnerProj::Body {
            status,
            rate_limit,
            body,
            gzip,
        } = self.project().inner.project()
        {
            let body = ready!(body.poll(cx).map_err(Error::Body))?;

            trace!("done reading response body");

            Poll::Ready(make_response(*status, *rate_limit, &body, |body| {
                if *gzip {
                    json::from_reader(GzDecoder::new(body)).map_err(Error::Deserializing)
                } else {
                    json::from_slice(body).map_err(Error::Deserializing)
                }
            }))
        } else {
            unreachable!();
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

fn prepare_request<R>(
    method: &Method,
    uri: &'static str,
    req: &R,
    client: Credentials<&str>,
    token: Credentials<&str>,
) -> http::Request<Vec<u8>>
where
    R: oauth1::Authorize + ?Sized,
{
    let form = method == Method::POST;

    let mut builder = oauth1::Builder::new(client, oauth1::HmacSha1);
    builder.token(token);
    let oauth1::Request {
        authorization,
        data,
    } = if form {
        builder.build_form(method.as_str(), uri, req)
    } else {
        builder.build(method.as_str(), uri, req)
    };

    trace!("{} {}", method, uri);
    trace!("data: {}", data);

    let req = http::Request::builder()
        .method(method)
        .header(ACCEPT_ENCODING, HeaderValue::from_static("gzip"))
        .header(AUTHORIZATION, authorization);

    if form {
        req.uri(Uri::from_static(uri))
            .header(
                CONTENT_TYPE,
                HeaderValue::from_static("application/x-www-form-urlencoded"),
            )
            .body(data.into_bytes().into())
            .unwrap()
    } else {
        req.uri(data).body(Vec::default()).unwrap()
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

#[cfg(test)]
mod tests {
    use futures::TryFutureExt;
    use std::convert::Infallible;
    use tower_service::Service;

    use super::*;

    struct MapErr<S, F> {
        service: S,
        f: F,
    }

    struct MockBody<'a>(Option<&'a [u8]>);

    impl<S, F> MapErr<S, F> {
        fn new<T, E>(service: S, f: F) -> Self
        where
            S: Service<T>,
            F: FnOnce(S::Error) -> E + Clone,
        {
            MapErr { service, f }
        }
    }

    impl<S, F, T, E> Service<T> for MapErr<S, F>
    where
        S: Service<T>,
        F: FnOnce(S::Error) -> E + Clone,
    {
        type Response = S::Response;
        type Error = E;
        type Future = futures::future::MapErr<S::Future, F>;

        fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            self.service.poll_ready(cx).map_err(self.f.clone())
        }

        fn call(&mut self, req: T) -> Self::Future {
            self.service.call(req).map_err(self.f.clone())
        }
    }

    impl<'a> Body for MockBody<'a> {
        type Data = &'a [u8];
        type Error = Infallible;

        fn poll_data(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Option<Result<Self::Data, Self::Error>>> {
            Poll::Ready(self.0.take().map(Ok))
        }

        fn poll_trailers(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
        ) -> Poll<Result<Option<http::HeaderMap>, Self::Error>> {
            Poll::Ready(Ok(None))
        }
    }

    #[tokio::test]
    async fn parse_errors() {
        api_requests! {
            GET "https://api.twitter.com/1.1/test/foo.json" => de::IgnoredAny;
            struct Foo {
                param: u32;
            }
        }

        let client = &Credentials::new("", "");
        let token = &Credentials::new("", "");

        let (http, mut handle) = tower_test::mock::pair::<http::Request<Vec<u8>>, _>();
        let http = MapErr::new(http, |e| -> Infallible { panic!("{:?}", e) });

        let res_fut = tokio::spawn(Foo::new(42).send(client, token, http));

        let (_req, tx) = handle.next_request().await.unwrap();
        let payload = br#"{"errors":[{"code":104,"message":"You aren't allowed to add members to this list."}]}"#;
        let res = http::Response::builder()
            .status(http::StatusCode::FORBIDDEN)
            .body(MockBody(Some(payload)))
            .unwrap();
        tx.send_response(res);

        match res_fut.await.unwrap().unwrap_err() {
            Error::Twitter(TwitterErrors {
                status: http::StatusCode::FORBIDDEN,
                errors,
                rate_limit: None,
            }) => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].code, 104);
                assert_eq!(
                    errors[0].message,
                    "You aren't allowed to add members to this list.",
                );
            }
            e => panic!("{:?}", e),
        };
    }
}
