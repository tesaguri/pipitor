use std::collections::hash_set::HashSet;

use anyhow::Context;
use diesel::prelude::*;
use diesel::r2d2::ConnectionManager;
use diesel::SqliteConnection;
use futures::future::{self, FutureExt};
use futures::stream::{FuturesUnordered, StreamExt, TryStreamExt};
use oauth_credentials::{Credentials, Token};
use pipitor::models;
use pipitor::private::twitter;
use pipitor::schema::*;
use twitter_client::Request;

use crate::common::client;

#[derive(Default, structopt::StructOpt)]
pub struct Opt {}

pub async fn main(opt: &crate::Opt, _subopt: Opt) -> anyhow::Result<()> {
    let manifest = opt.open_manifest()?;

    let (config, list_id) = if let Some((config, Some(list))) = manifest
        .twitter
        .as_ref()
        .map(|config| (config, &config.list))
    {
        (config, list.id)
    } else {
        anyhow::bail!("missing `twitter.list` in the manifest");
    };

    let manager = ConnectionManager::<SqliteConnection>::new(manifest.database_url());
    let pool = pipitor::private::util::r2d2::new_pool(manager)
        .context("failed to initialize the connection pool")?;

    let mut client = client();

    let token: Credentials<_> = twitter_tokens::table
        .find(&config.user)
        .get_result::<models::TwitterToken>(&*pool.get()?)
        .optional()
        .context("failed to load tokens from the database")?
        .context("please run `pipitor twitter-login` first to login")?
        .into();
    let token = Token::from_ref(&config.client, &token);

    let res_fut = twitter::lists::Members::new(list_id)
        .count(Some(5000))
        .send(&token, &mut client);
    println!("Retrieving the list...");

    let users: HashSet<i64> = manifest.twitter_topics().collect();
    let res = res_fut
        .await
        .context("failed to retrieve the list from Twitter")?;
    let list: HashSet<i64> = (*res).users.iter().map(|u| u.id).collect();
    // `res` is actually a cursored response, but we have set `count` parameter to `5000`,
    // which is the maximum # of members of a list, so we needn't check cursor here.

    let mut updated = false;

    let destroy: FuturesUnordered<_> = list
        .difference(&users)
        .map(|&user| twitter::lists::members::Destroy::new(list_id, user).send(&token, &mut client))
        .collect();
    if !destroy.is_empty() {
        println!("Removing redundant user in the list...");
        updated = true;
    }

    destroy
        .try_for_each(|_| futures::future::ok(()))
        .await
        .context("failed to remove a user from the list")?;

    let create: FuturesUnordered<_> = users
        .difference(&list)
        .map(|&user| {
            twitter::lists::members::Create::new(list_id, user)
                .send(&token, &mut client)
                .map(move |r| (r, user))
        })
        .collect();
    if !create.is_empty() {
        println!("Adding users to the list...");
        updated = true;
    }

    create
        .then(|(result, user)| match result {
            Ok(_) => future::ok(()),
            Err(e) => {
                warn!("failed to add user {} to the list", user);
                if let twitter_client::Error::Twitter(ref e) = e {
                    for c in e.codes() {
                        match c {
                            // Skip this error as it occurs if (but not only if) the user is protected.
                            twitter_client::error::ErrorCode::YOU_ARENT_ALLOWED_TO_ADD_MEMBERS_TO_THIS_LIST => {
                                warn!("You aren't allowed to add the user to the list");
                                return future::ok(());
                            }
                            twitter_client::error::ErrorCode::CANNOT_FIND_SPECIFIED_USER => {
                                warn!("Cannot find the user");
                                return future::ok(());
                            }
                            _ => {}
                        }
                    }
                }
                future::err(e)
            }
        })
        .try_for_each(future::ok)
        .await
        .context("failed to add a user to the list")?;

    if updated {
        println!("Successfully updated the list");
    } else {
        println!("Nothing to update");
    }

    Ok(())
}
