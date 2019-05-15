use std::collections::HashSet;
use std::io::{self, Write};

use diesel::prelude::*;
use diesel::SqliteConnection;
use failure::{Fallible, ResultExt};
use futures::compat::Stream01CompatExt;
use futures::future;
use futures::stream::{FuturesUnordered, Stream, StreamExt, TryStreamExt};
use hyper::client::Client;
use hyper::StatusCode;
use hyper_tls::HttpsConnector;
use itertools::Itertools;
use pipitor::models;
use pipitor::twitter::{self, Request as _};
use r2d2::Pool;
use r2d2_diesel::ConnectionManager;

use crate::common::open_manifest;

#[derive(Default, structopt::StructOpt)]
pub struct Opt {}

pub async fn main(opt: &crate::Opt, _subopt: Opt) -> Fallible<()> {
    let manifest = open_manifest(opt)?;

    use pipitor::schema::twitter_tokens::dsl::*;

    let manager = ConnectionManager::<SqliteConnection>::new(manifest.database_url());
    let pool = Pool::new(manager).context("failed to initialize the connection pool")?;
    let conn = HttpsConnector::new(4).context("failed to initialize TLS client")?;
    let client = Client::builder().build(conn);

    let unauthed_users: FuturesUnordered<_> = manifest
        .rule
        .twitter_outboxes()
        .chain(Some(manifest.twitter.user))
        .unique()
        .map(|user| {
            let token = twitter_tokens
                .find(&user)
                .get_result::<models::TwitterToken>(&*pool.get()?)
                .optional()
                .context("failed to load tokens from the database")?;

            // Make borrowck happy
            let (manifest, client) = (&manifest, &client);
            Ok(async move {
                if let Some(token) = token {
                    match twitter::account::VerifyCredentials::new()
                        .send(manifest.twitter.client.as_ref(), (&token).into(), client)
                        .await
                    {
                        Ok(_) => return Ok(None),
                        Err(twitter::Error::Twitter(ref e))
                            if e.status == StatusCode::UNAUTHORIZED => {}
                        Err(e) => {
                            return Err(e)
                                .context("error while verifying Twitter credentials")
                                .map_err(Into::into)
                                as Fallible<_>;
                        }
                    }
                }

                Ok(Some(user))
            })
        })
        .collect::<Fallible<_>>()?;

    let mut unauthed_users: HashSet<_> = unauthed_users
        .try_filter_map(future::ok)
        .try_collect()
        .await?;

    if unauthed_users.is_empty() {
        println!("All users are already logged in.");
        return Ok(());
    }

    let stdin = tokio_file_unix::File::new_nb(tokio_file_unix::raw_stdin()?)?
        .into_io(&Default::default())?;
    let mut stdin = tokio::io::lines(io::BufReader::new(stdin)).compat();

    while !unauthed_users.is_empty() {
        let temporary = twitter::oauth::request_token(manifest.twitter.client.as_ref(), &client)
            .await
            .context("error while getting OAuth request token from Twitter")?;

        let verifier = input_verifier(&mut stdin, &temporary.key, &unauthed_users).await?;

        let (user, token) = twitter::oauth::access_token(
            &verifier,
            manifest.twitter.client.as_ref(),
            temporary.as_ref(),
            &client,
        )
        .await
        .context("error while getting OAuth access token from Twitter")?
        .response;

        if unauthed_users.remove(&user) {
            diesel::replace_into(twitter_tokens)
                .values(models::NewTwitterTokens {
                    id: user,
                    access_token: &token.key,
                    access_token_secret: &token.secret,
                })
                .execute(&*pool.get()?)?;
            println!("Successfully logged in as user_id={}", user);
        } else {
            println!("Invalid user, try again");
            // TODO: invalidate token
        }
    }

    Ok(())
}

async fn input_verifier<'a, S, I, E>(
    stdin: &'a mut S,
    token: &'a str,
    users: I,
) -> Result<String, E>
where
    S: Stream<Item = Result<String, E>> + Unpin,
    I: IntoIterator<Item = &'a i64> + Copy,
{
    loop {
        {
            let stdout = io::stdout();
            let mut stdout = stdout.lock();
            write!(
                stdout,
                "\n\
                 Open the following URL in a Web browser:\n\
                 https://api.twitter.com/oauth/authorize?force_login=true&oauth_token={}\n\
                 Log into an account with one of the following `user_id`s:\n",
                token,
            )
            .unwrap();
            for user in users {
                writeln!(stdout, "{}", user).unwrap();
            }
            stdout
                .write_all(b"And enter the PIN code given by Twitter:\n>")
                .unwrap();
            stdout.flush().unwrap();
        }

        if let Some(input) = stdin.next().await {
            return input;
        }
    }
}
