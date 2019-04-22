use futures::compat::Stream01CompatExt;
use futures::TryStreamExt;
use hyper::client::connect::Connect;
use hyper::header::AUTHORIZATION;
use hyper::{Client, Uri};
use oauth1::OAuth1Authorize;

use super::{Credentials, Error, Result};

pub async fn request_token<'a, C>(
    client_credentials: Credentials<&'a str>,
    client: &'a Client<C>,
) -> Result<Credentials>
where
    C: Connect + Sync + 'static,
{
    const URI: &str = "https://api.twitter.com/oauth/request_token";

    let oauth1::Request { authorization, .. } = ().authorize_form(
        "POST",
        URI,
        client_credentials.key,
        client_credentials.secret,
        None,
        oauth1::HmacSha1,
        &*oauth1::Options::new().callback("oob"),
    );

    let mut req = hyper::Request::post(Uri::from_static(URI));
    req.header(AUTHORIZATION, authorization);

    let res =
        await!(client.request(req.body(Default::default()).unwrap())).map_err(Error::Hyper)?;
    super::check_status(&res)?;

    let body = await!(res.into_body().compat().try_concat()).map_err(Error::Hyper)?;

    #[derive(serde::Deserialize)]
    struct Token {
        oauth_token: String,
        oauth_token_secret: String,
    }

    let Token {
        oauth_token: key,
        oauth_token_secret: secret,
    } = serde_urlencoded::from_bytes::<Token>(&body).map_err(|_| Error::Unexpected)?;

    Ok(Credentials { key, secret })
}

pub async fn access_token<'a, C>(
    oauth_verifier: &'a str,
    client_credentials: Credentials<&'a str>,
    temporary_credentials: Credentials<&'a str>,
    client: &'a Client<C>,
) -> Result<(i64, Credentials)>
where
    C: Connect + Sync + 'static,
{
    const URI: &str = "https://api.twitter.com/oauth/access_token";

    let oauth1::Request { authorization, .. } = ().authorize_form(
        "POST",
        URI,
        client_credentials.key,
        client_credentials.secret,
        temporary_credentials.secret,
        oauth1::HmacSha1,
        &*oauth1::Options::new()
            .token(temporary_credentials.key)
            .verifier(oauth_verifier),
    );

    let mut req = hyper::Request::post(Uri::from_static(URI));
    req.header(AUTHORIZATION, authorization);

    let res =
        await!(client.request(req.body(Default::default()).unwrap())).map_err(Error::Hyper)?;
    super::check_status(&res)?;

    let body = await!(res.into_body().compat().try_concat()).map_err(Error::Hyper)?;

    #[derive(serde::Deserialize)]
    struct Token {
        oauth_token: String,
        oauth_token_secret: String,
        user_id: i64,
    }

    let Token {
        oauth_token: key,
        oauth_token_secret: secret,
        user_id,
    } = serde_urlencoded::from_bytes::<Token>(&body).map_err(|_| Error::Unexpected)?;

    Ok((user_id, Credentials { key, secret }))
}
