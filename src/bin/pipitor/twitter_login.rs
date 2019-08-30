use std::collections::HashSet;
use std::io::{self, Write};

use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::SqliteConnection;
use failure::{Fallible, ResultExt};
use futures::compat::Stream01CompatExt;
use futures::future;
use futures::stream::{FuturesUnordered, Stream, StreamExt, TryStreamExt};
use hyper::client::Client;
use hyper::StatusCode;
use pipitor::models;
use pipitor::private::twitter::{self, Request as _};

use crate::common::{https_connector, open_credentials, open_manifest};

#[derive(Default, structopt::StructOpt)]
pub struct Opt {}

pub async fn main(opt: &crate::Opt, _subopt: Opt) -> Fallible<()> {
    use pipitor::schema::twitter_tokens::dsl::*;

    let manifest = open_manifest(opt)?;
    let credentials = open_credentials(opt, &manifest)?;
    let manager = ConnectionManager::<SqliteConnection>::new(manifest.database_url());
    let pool = Pool::new(manager).context("failed to initialize the connection pool")?;
    let client = Client::builder().build(https_connector()?);

    let unauthed_users: FuturesUnordered<_> = manifest
        .rule
        .twitter_outboxes()
        .chain(Some(manifest.twitter.user))
        .collect::<HashSet<_>>()
        .iter()
        .map(|&user| {
            let token = twitter_tokens
                .find(&user)
                .get_result::<models::TwitterToken>(&*pool.get()?)
                .optional()
                .context("failed to load tokens from the database")?;

            // Make borrowck happy
            let (credentials, client) = (&credentials, &client);
            Ok(async move {
                if let Some(token) = token {
                    match twitter::account::VerifyCredentials::new()
                        .send(credentials.twitter.client.as_ref(), (&token).into(), client)
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

    let stdin = stdin::nb()?;
    let mut stdin = tokio::io::lines(io::BufReader::new(stdin)).compat();

    while !unauthed_users.is_empty() {
        let temporary = twitter::oauth::request_token(credentials.twitter.client.as_ref(), &client)
            .await
            .context("error while getting OAuth request token from Twitter")?;

        let verifier = input_verifier(&mut stdin, temporary.identifier(), &unauthed_users).await?;

        let (user, token) = twitter::oauth::access_token(
            &verifier,
            credentials.twitter.client.as_ref(),
            temporary.as_ref(),
            &client,
        )
        .await
        .context("error while getting OAuth access token from Twitter")?
        .data;

        if unauthed_users.remove(&user) {
            diesel::replace_into(twitter_tokens)
                .values(models::NewTwitterTokens {
                    id: user,
                    access_token: token.identifier(),
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

#[cfg(unix)]
mod stdin {
    use std::io;

    use tokio::io::AsyncRead as AsyncRead01;
    use tokio_file_unix::{raw_stdin, File};

    pub fn nb() -> io::Result<impl AsyncRead01> {
        File::new_nb(raw_stdin()?)?.into_io(&Default::default())
    }
}

#[cfg(windows)]
mod stdin {
    use std::io;

    use tokio::io::AsyncRead as AsyncRead01;

    pub fn nb() -> io::Result<impl AsyncRead01> {
        Ok(tokio_stdin_stdout::stdin(0))
    }
}
