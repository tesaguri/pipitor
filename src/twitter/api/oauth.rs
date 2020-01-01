use http::header::AUTHORIZATION;
use http::Uri;
use http_body::Body;
use oauth1::Credentials;

use crate::util::{ConcatBody, HttpResponseFuture, HttpService};

use super::{Error, Response};

pub async fn request_token<'a, S>(
    client_credentials: Credentials<&'a str>,
    mut client: S,
) -> Result<Response<Credentials>, Error<S::Error, <S::ResponseBody as Body>::Error>>
where
    S: HttpService<hyper::Body>,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    const URI: &str = "https://api.twitter.com/oauth/request_token";

    let oauth1::Request { authorization, .. } =
        oauth1::Builder::new(client_credentials, oauth1::HmacSha1)
            .callback("oob")
            .post_form(URI, ());

    let req = http::Request::post(Uri::from_static(URI))
        .header(AUTHORIZATION, authorization)
        .body(Default::default())
        .unwrap();

    let res = client
        .call(req)
        .into_future()
        .await
        .map_err(Error::Service)?;

    let status = res.status();
    let rate_limit = super::rate_limit(&res);
    let body = ConcatBody::new(res.into_body())
        .await
        .map_err(Error::Body)?;

    #[derive(serde::Deserialize)]
    struct Token {
        oauth_token: String,
        oauth_token_secret: String,
    }

    super::make_response(status, rate_limit, &body, |body| {
        let Token {
            oauth_token: identifier,
            oauth_token_secret: secret,
        } = serde_urlencoded::from_bytes::<Token>(body).map_err(|_| Error::Unexpected)?;

        Ok(Credentials { identifier, secret })
    })
}

pub async fn access_token<'a, S>(
    oauth_verifier: &'a str,
    client_credentials: Credentials<&'a str>,
    temporary_credentials: Credentials<&'a str>,
    mut client: S,
) -> Result<Response<(i64, Credentials)>, Error<S::Error, <S::ResponseBody as Body>::Error>>
where
    S: HttpService<hyper::Body>,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
{
    const URI: &str = "https://api.twitter.com/oauth/access_token";

    let oauth1::Request { authorization, .. } =
        oauth1::Builder::new(client_credentials, oauth1::HmacSha1)
            .token(temporary_credentials)
            .verifier(oauth_verifier)
            .post_form(URI, ());

    let req = http::Request::post(Uri::from_static(URI))
        .header(AUTHORIZATION, authorization)
        .body(Default::default())
        .unwrap();

    let res = client
        .call(req)
        .into_future()
        .await
        .map_err(Error::Service)?;

    let status = res.status();
    let rate_limit = super::rate_limit(&res);
    let body = ConcatBody::new(res.into_body())
        .await
        .map_err(Error::Body)?;

    #[derive(serde::Deserialize)]
    struct Token {
        oauth_token: String,
        oauth_token_secret: String,
        user_id: i64,
    }

    super::make_response(status, rate_limit, &body, |body| {
        let Token {
            oauth_token: identifier,
            oauth_token_secret: secret,
            user_id,
        } = serde_urlencoded::from_bytes::<Token>(body).map_err(|_| Error::Unexpected)?;

        Ok((user_id, Credentials { identifier, secret }))
    })
}
