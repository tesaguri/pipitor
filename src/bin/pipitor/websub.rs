use std::future;
use std::io::{self, Write};

use anyhow::Context;
use diesel::prelude::*;
use futures::stream::{FuturesUnordered, TryStreamExt};
use structopt::StructOpt;

use pipitor::manifest;
use pipitor::private::websub::hub;
use pipitor::schema::*;

use crate::common;

#[derive(StructOpt)]
pub struct Opt {
    #[structopt(subcommand)]
    cmd: Cmd,
}

#[derive(StructOpt)]
enum Cmd {
    #[structopt(name = "list", about = "Lists WebSub subscriptions")]
    List(List),
    #[structopt(name = "unsubscribe", about = "Unsubscribes from topics")]
    Unsubscribe(Unsubscribe),
}

#[derive(StructOpt)]
struct List {
    #[structopt(long = "show-secrets")]
    show_secrets: bool,
}

#[derive(StructOpt)]
struct Unsubscribe {
    #[structopt(long = "all", help = "Unsubscribes from all topics")]
    all: bool,
    #[structopt(long = "id", required_unless("all"))]
    id: Vec<i64>,
}

pub async fn main(opt: &crate::Opt, subopt: Opt) -> anyhow::Result<()> {
    let manifest = opt.open_manifest()?;

    let conn = SqliteConnection::establish(&manifest.database_url())
        .context("failed to connect to the database")?;
    pipitor::private::query::pragma_foreign_keys_on().execute(&conn)?;

    let config = if let Some(config) = manifest.websub {
        config
    } else {
        anyhow::bail!("missing `websub` in the manifest");
    };

    match subopt.cmd {
        Cmd::List(subsubopt) => list(subsubopt, conn),
        Cmd::Unsubscribe(subsubopt) => unsubscribe(subsubopt, config, conn).await,
    }
}

fn list(opt: List, conn: SqliteConnection) -> anyhow::Result<()> {
    let list = websub_subscriptions::table
        .left_outer_join(websub_active_subscriptions::table)
        .select((
            websub_subscriptions::id,
            websub_subscriptions::hub,
            websub_subscriptions::topic,
            websub_subscriptions::secret,
            websub_active_subscriptions::expires_at.nullable(),
        ))
        .load::<(i64, String, String, String, Option<i64>)>(&conn)?;
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    for (id, hub, topic, secret, expires_at) in list {
        write!(stdout, "{}", id)?;
        stdout.write_all(b"\t")?;
        write!(stdout, "{}", hub)?;
        stdout.write_all(b"\t")?;
        write!(stdout, "{}", topic)?;
        stdout.write_all(b"\t")?;
        if opt.show_secrets {
            write!(stdout, "{}", secret)?;
        }
        stdout.write_all(b"\t")?;
        if let Some(expires_at) = expires_at {
            write!(stdout, "{}", expires_at)?;
        }
    }
    Ok(())
}

async fn unsubscribe(
    opt: Unsubscribe,
    config: manifest::Websub,
    conn: SqliteConnection,
) -> anyhow::Result<()> {
    let client = common::client();
    let tasks = FuturesUnordered::new();

    let rows = websub_subscriptions::table
        .left_outer_join(websub_active_subscriptions::table)
        .select((
            websub_subscriptions::id,
            websub_subscriptions::hub,
            websub_subscriptions::topic,
        ));

    conn.transaction(|| -> QueryResult<()> {
        let rows: Vec<(i64, String, String)> = if opt.all {
            rows.load(&conn)?
        } else {
            rows.filter(websub_subscriptions::id.eq_any(opt.id))
                .load(&conn)?
        };
        for (id, hub, topic) in rows {
            let task = hub::unsubscribe(&config.host, id, hub, topic, client.clone(), &conn);
            tasks.push(task);
        }
        Ok(())
    })?;

    tasks.try_for_each(|()| future::ready(Ok(()))).await?;

    Ok(())
}
