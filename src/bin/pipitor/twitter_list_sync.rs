use std::collections::hash_set::HashSet;

use diesel::prelude::*;
use diesel::SqliteConnection;
use failure::{Fallible, ResultExt};
use futures::stream::{FuturesUnordered, TryStreamExt};
use hyper::client::Client;
use hyper_tls::HttpsConnector;
use pipitor::models;
use pipitor::twitter::{self, Request as _};
use r2d2::Pool;
use r2d2_diesel::ConnectionManager;

use crate::common::open_manifest;

#[derive(Default, structopt::StructOpt)]
pub struct Opt {}

pub async fn main(opt: &crate::Opt, _subopt: Opt) -> Fallible<()> {
    use pipitor::schema::twitter_tokens::dsl::*;

    let manifest = open_manifest(opt)?;
    let list_id = if let Some(list) = manifest.twitter.list {
        list
    } else {
        println!("`twitter.list` is not set in the manifest");
        return Ok(());
    };

    let manager = ConnectionManager::<SqliteConnection>::new(manifest.database_url());
    let pool = Pool::new(manager)?;

    let conn = HttpsConnector::new(4).context("failed to initialize TLS client")?;
    let client = Client::builder().build(conn);

    let token: twitter::Credentials = twitter_tokens
        .find(&manifest.twitter.user)
        .get_result::<models::TwitterToken>(&*pool.get()?)
        .optional()
        .context("failed to load tokens from the database")?
        .ok_or_else(|| failure::err_msg("please run `pipitor twitter-login` first to login"))?
        .into();

    let res_fut = twitter::lists::Members::new(list_id)
        .count(Some(5000))
        .send(manifest.twitter.client.as_ref(), token.as_ref(), &client);
    println!("Retrieving the list...");

    let users: HashSet<i64> = manifest.rule.twitter_topics().collect();
    let res = res_fut
        .await
        .context("failed to retrieve the list from Twitter")?;
    let list: HashSet<i64> = (*res).users.iter().map(|u| u.id).collect();
    // `res` is actually a cursored response, but we have set `count` parameter to `5000`,
    // which is the maximum # of members of a list, so we needn't check cursor here.

    let mut updated = false;

    let destroy: FuturesUnordered<_> = list
        .difference(&users)
        .map(|&user| {
            twitter::lists::members::Destroy::new(list_id, user).send(
                manifest.twitter.client.as_ref(),
                token.as_ref(),
                &client,
            )
        })
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
            twitter::lists::members::Create::new(list_id, user).send(
                manifest.twitter.client.as_ref(),
                token.as_ref(),
                &client,
            )
        })
        .collect();
    if !create.is_empty() {
        println!("Adding users to the list...");
        updated = true;
    }

    create
        .try_for_each(|_| futures::future::ok(()))
        .await
        .context("failed to add a user to the list")?;

    if updated {
        println!("Successfully updated the list");
    } else {
        println!("Nothing to update");
    }

    Ok(())
}
