use http::header::AUTHORIZATION;
use http::Uri;
use http_body::Body;
use oauth_credentials::Credentials;
use tower_util::ServiceExt;

use crate::util::HttpService;

use super::super::models::AccessToken;
use super::{Error, Response};

pub async fn request_token<'a, S, B>(
    client_credentials: Credentials<&'a str>,
    client: S,
) -> Result<Response<Credentials<Box<str>>>, Error<S::Error, <S::ResponseBody as Body>::Error>>
where
    S: HttpService<B>,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    const URI: &str = "https://api.twitter.com/oauth/request_token";

    let authorization = oauth1::Builder::<_, _>::new(client_credentials, oauth1::HmacSha1)
        .callback("oob")
        .post(URI, &());

    let req = http::Request::post(Uri::from_static(URI))
        .header(AUTHORIZATION, authorization)
        .body(Default::default())
        .unwrap();

    let res = client
        .into_service()
        .oneshot(req)
        .await
        .map_err(Error::Service)?;

    let status = res.status();
    let rate_limit = super::rate_limit(&res);
    let body = hyper::body::to_bytes(res).await.map_err(Error::Body)?;

    super::make_response(status, rate_limit, &body, |body| {
        serde_urlencoded::from_bytes(body).map_err(|_| Error::Unexpected)
    })
}

pub async fn access_token<'a, S, B>(
    oauth_verifier: &'a str,
    client_credentials: Credentials<&'a str>,
    temporary_credentials: Credentials<&'a str>,
    client: S,
) -> Result<Response<AccessToken>, Error<S::Error, <S::ResponseBody as Body>::Error>>
where
    S: HttpService<B>,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    const URI: &str = "https://api.twitter.com/oauth/access_token";

    let authorization = oauth1::Builder::new(client_credentials, oauth1::HmacSha1)
        .token(temporary_credentials)
        .verifier(oauth_verifier)
        .post(URI, &());

    let req = http::Request::post(Uri::from_static(URI))
        .header(AUTHORIZATION, authorization)
        .body(Default::default())
        .unwrap();

    let res = client
        .into_service()
        .oneshot(req)
        .await
        .map_err(Error::Service)?;

    let status = res.status();
    let rate_limit = super::rate_limit(&res);
    let body = hyper::body::to_bytes(res).await.map_err(Error::Body)?;

    super::make_response(status, rate_limit, &body, |body| {
        serde_urlencoded::from_bytes(body).map_err(|_| Error::Unexpected)
    })
}
