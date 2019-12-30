use hyper::client::connect::Connect;
use hyper::header::AUTHORIZATION;
use hyper::{Client, Uri};
use oauth1::Credentials;

use crate::util::ConcatBody;

use super::{Error, Response, Result};

pub async fn request_token<'a, C>(
    client_credentials: Credentials<&'a str>,
    client: &'a Client<C>,
) -> Result<Response<Credentials>>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    const URI: &str = "https://api.twitter.com/oauth/request_token";

    let oauth1::Request { authorization, .. } =
        oauth1::Builder::new(client_credentials, oauth1::HmacSha1)
            .callback("oob")
            .post_form(URI, ());

    let req = hyper::Request::post(Uri::from_static(URI))
        .header(AUTHORIZATION, authorization)
        .body(Default::default())
        .unwrap();

    let res = client.request(req).await.map_err(Error::Hyper)?;

    let status = res.status();
    let rate_limit = super::rate_limit(&res);
    let body = ConcatBody::new(res.into_body())
        .await
        .map_err(Error::Hyper)?;

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

pub async fn access_token<'a, C>(
    oauth_verifier: &'a str,
    client_credentials: Credentials<&'a str>,
    temporary_credentials: Credentials<&'a str>,
    client: &'a Client<C>,
) -> Result<Response<(i64, Credentials)>>
where
    C: Connect + Clone + Send + Sync + 'static,
{
    const URI: &str = "https://api.twitter.com/oauth/access_token";

    let oauth1::Request { authorization, .. } =
        oauth1::Builder::new(client_credentials, oauth1::HmacSha1)
            .token(temporary_credentials)
            .verifier(oauth_verifier)
            .post_form(URI, ());

    let req = hyper::Request::post(Uri::from_static(URI))
        .header(AUTHORIZATION, authorization)
        .body(Default::default())
        .unwrap();

    let res = client.request(req).await.map_err(Error::Hyper)?;

    let status = res.status();
    let rate_limit = super::rate_limit(&res);
    let body = ConcatBody::new(res.into_body())
        .await
        .map_err(Error::Hyper)?;

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
