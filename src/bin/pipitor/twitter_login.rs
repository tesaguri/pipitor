use std::collections::HashSet;
use std::io::{self, Write};
use std::pin::Pin;

use anyhow::Context;
use diesel::prelude::*;
use diesel::r2d2::ConnectionManager;
use diesel::SqliteConnection;
use futures::stream::{FuturesUnordered, Stream, StreamExt, TryStreamExt};
use futures::{future, stream};
use http::StatusCode;
use oauth_credentials::{Credentials, Token};
use pipitor::models;
use pipitor::private::twitter;
use pipitor::schema::*;
use tokio::io::AsyncBufReadExt;
use twitter_client::Request;

use crate::common::client;

#[derive(Default, structopt::StructOpt)]
pub struct Opt {}

pub async fn main(opt: &crate::Opt, _subopt: Opt) -> anyhow::Result<()> {
    let manifest = opt.open_manifest()?;

    let config = if let Some(ref config) = manifest.twitter {
        config
    } else {
        anyhow::bail!("missing `twitter` in the manifest");
    };

    let manager = ConnectionManager::<SqliteConnection>::new(manifest.database_url());
    let pool = pipitor::private::util::r2d2::new_pool(manager)
        .context("failed to initialize the connection pool")?;
    let mut client = client();

    let unauthed_users: FuturesUnordered<_> = manifest
        .twitter_outboxes()
        .chain(Some(config.user))
        .collect::<HashSet<_>>()
        .iter()
        .map(|&user| {
            let token = twitter_tokens::table
                .find(&user)
                .get_result::<models::TwitterToken>(&*pool.get()?)
                .optional()
                .context("failed to load tokens from the database")?
                .map(Credentials::from);

            let mut client = client.clone();
            Ok(async move {
                if let Some(token) = token {
                    let token = Token::from_ref(&config.client, &token);
                    match twitter::account::VerifyCredentials::new()
                        .send(&token, &mut client)
                        .await
                    {
                        Ok(_) => return Ok(None),
                        Err(twitter_client::Error::Twitter(ref e))
                            if e.status == StatusCode::UNAUTHORIZED => {}
                        Err(e) => {
                            return Err(e).context("error while verifying Twitter credentials");
                        }
                    }
                }

                Ok(Some(user))
            })
        })
        .collect::<anyhow::Result<_>>()?;

    let mut unauthed_users: HashSet<_> = unauthed_users
        .try_filter_map(future::ok)
        .try_collect()
        .await?;

    if unauthed_users.is_empty() {
        println!("All users are already logged in.");
        return Ok(());
    }

    let stdin = tokio::io::stdin();
    let mut stdin = tokio::io::BufReader::new(stdin).lines();
    let mut stdin = stream::poll_fn(|cx| {
        Pin::new(&mut stdin)
            .poll_next_line(cx)
            .map(Result::transpose)
    });

    while !unauthed_users.is_empty() {
        let temporary =
            twitter_client::auth::request_token(&config.client, "oob", None, &mut client)
                .await
                .context("error while getting OAuth request token from Twitter")?;

        let verifier = input_verifier(&mut stdin, temporary.identifier(), &unauthed_users).await?;

        let token =
            twitter_client::auth::access_token(&config.client, &temporary, &verifier, &mut client)
                .await
                .context("error while getting OAuth access token from Twitter")?;

        if unauthed_users.remove(&token.user_id) {
            diesel::replace_into(twitter_tokens::table)
                .values(&models::NewTwitterTokens::from(&token))
                .execute(&*pool.get()?)?;
            println!("Successfully logged in as user_id={}", token.user_id);
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
