use std::marker::PhantomData;
use std::sync::Arc;

use diesel::dsl::*;
use diesel::prelude::*;
use futures::FutureExt;
use http_body::Body;
use pin_project::pin_project;

use crate::feed::{Entry, Feed};
use crate::manifest::Outbox;
use crate::schema::*;
use crate::util::HttpService;
use crate::{models, twitter};

use super::{Core, TwitterRequestExt as _};

/// An object used to send entries to their corresponding outboxes.
#[pin_project]
pub struct Sender<S, B>
where
    S: HttpService<B>,
{
    marker: PhantomData<fn() -> (S, B)>,
}

impl<S, B> Sender<S, B>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>> + Send + 'static,
{
    pub fn new() -> Self {
        Default::default()
    }

    pub fn send_feed(&self, topic: &str, feed: Feed, core: &Core<S>) -> anyhow::Result<()> {
        trace_fn!(Sender::<S, B>::send_feed, topic, feed.id);

        for e in feed.entries {
            self.send_entry(topic, e, core)?;
        }

        Ok(())
    }

    pub fn send_entry(&self, topic: &str, entry: Entry, core: &Core<S>) -> anyhow::Result<()> {
        trace_fn!(Sender::<S, B>::send_entry, entry);

        let link = if let Some(ref link) = entry.link {
            link
        } else {
            debug!("Received an entry with no link: {:?}", entry);
            return Ok(());
        };

        let conn = core.conn()?;

        let row = entries::table
            .find((topic, &entry.id))
            .or_filter(entries::link.eq(link));
        let already_processed = select(exists(row)).get_result::<bool>(&*conn)?;
        if already_processed {
            trace!("The entry has already been processed");
            return Ok(());
        }

        let text = if let Some(mut text) = entry.title.clone() {
            const TITLE_LEN: usize = twitter::text::MAX_WEIGHTED_TWEET_LENGTH
                - 1
                - twitter::text::TRANSFORMED_URL_LENGTH;
            twitter::text::sanitize(&mut text, TITLE_LEN);
            text.push('\n');
            text.push_str(link);
            text
        } else {
            link.to_owned()
        };

        let shared = Arc::new((Box::<str>::from(topic), entry));
        let entry = &shared.1;
        let mut conn = Some(conn);
        for outbox in core.router().route_entry(topic, entry) {
            debug!("sending an entry to outbox {:?}: {:?}", outbox, entry);

            match *outbox {
                Outbox::Twitter(user) => {
                    let shared = shared.clone();
                    let conn = conn.take().map(Ok).unwrap_or_else(|| core.conn())?;
                    let fut = twitter::statuses::Update::new(&text)
                        .send(core, user)
                        .map(move |result| -> anyhow::Result<()> {
                            result?;
                            diesel::replace_into(entries::table)
                                .values(&models::NewEntry::new(&shared.0, &shared.1).unwrap())
                                .execute(&*conn)?;
                            Ok(())
                        })
                        .map(|result| {
                            // TODO: better error handling
                            if let Err(e) = result {
                                error!("{:?}", e);
                            }
                        });
                    tokio::spawn(core.shutdown_handle().wrap_future(fut));
                }
            }
        }

        Ok(())
    }
}

impl<S, B> Default for Sender<S, B>
where
    S: HttpService<B> + Clone,
    <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
    B: Default + From<Vec<u8>>,
{
    fn default() -> Self {
        Sender {
            marker: PhantomData,
        }
    }
}
