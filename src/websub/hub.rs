use std::convert::TryInto;
use std::str;

use bytes::Bytes;
use diesel::prelude::*;
use diesel::result::DatabaseErrorKind;
use diesel::SqliteConnection;
use futures::{Future, TryFutureExt};
use http::header::{HeaderValue, CONTENT_TYPE, LOCATION};
use http::uri::{Parts, PathAndQuery, Uri};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use tower_util::ServiceExt;

use crate::schema::*;
use crate::util::{consts::APPLICATION_WWW_FORM_URLENCODED, HttpService};

#[derive(Serialize, Deserialize)]
#[serde(tag = "hub.mode")]
#[serde(rename_all = "lowercase")]
pub enum Form<S = String> {
    Subscribe {
        #[serde(rename = "hub.callback")]
        #[serde(with = "http_serde::uri")]
        callback: Uri,
        #[serde(rename = "hub.topic")]
        topic: S,
        #[serde(rename = "hub.secret")]
        secret: S,
    },
    Unsubscribe {
        #[serde(rename = "hub.callback")]
        #[serde(with = "http_serde::uri")]
        callback: Uri,
        #[serde(rename = "hub.topic")]
        topic: S,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "hub.mode")]
#[serde(rename_all = "lowercase")]
pub enum Verify<S = String> {
    Subscribe {
        #[serde(rename = "hub.topic")]
        topic: S,
        #[serde(rename = "hub.challenge")]
        challenge: S,
        #[serde(rename = "hub.lease_seconds")]
        #[serde(deserialize_with = "crate::util::deserialize_from_str")]
        lease_seconds: u64,
    },
    Unsubscribe {
        #[serde(rename = "hub.topic")]
        topic: S,
        #[serde(rename = "hub.challenge")]
        challenge: S,
    },
}

const SECRET_LEN: usize = 32;
type Secret = string::String<[u8; SECRET_LEN]>;

pub fn subscribe<S, B>(
    callback: &Uri,
    hub: String,
    topic: String,
    client: S,
    conn: &SqliteConnection,
) -> impl Future<Output = Result<(), S::Error>>
where
    S: HttpService<B>,
    B: From<Vec<u8>>,
{
    let (id, secret) = create_subscription(&hub, &topic, conn);

    log::info!("Subscribing to topic {} at hub {} ({})", topic, hub, id);

    let body = serde_urlencoded::to_string(Form::Subscribe {
        callback: make_callback(callback.clone(), id),
        topic: &*topic,
        secret: &*secret,
    })
    .unwrap();

    send_request(hub, topic, body, client)
}

pub fn renew<S, B>(
    callback: &Uri,
    old: i64,
    hub: String,
    topic: String,
    client: S,
    conn: &SqliteConnection,
) -> impl Future<Output = Result<(), S::Error>>
where
    S: HttpService<B>,
    B: From<Vec<u8>>,
{
    let (new, secret) = conn
        .transaction(|| {
            let (new, secret) = create_subscription(&hub, &topic, conn);
            diesel::insert_into(websub_renewing_subscriptions::table)
                .values((
                    websub_renewing_subscriptions::old.eq(old),
                    websub_renewing_subscriptions::new.eq(new),
                ))
                .execute(conn)
                .map(|_| (new, secret))
        })
        .unwrap();

    log::info!(
        "Renewing a subscription of topic {} at hub {} ({} -> {})",
        topic,
        hub,
        old,
        new
    );

    let body = serde_urlencoded::to_string(Form::Subscribe {
        callback: make_callback(callback.clone(), new),
        topic: &*topic,
        secret: &*secret,
    })
    .unwrap();

    send_request(hub, topic, body, client)
}

pub fn unsubscribe<S, B>(
    callback: &Uri,
    id: i64,
    hub: String,
    topic: String,
    client: S,
    conn: &SqliteConnection,
) -> impl Future<Output = Result<(), S::Error>>
where
    S: HttpService<B>,
    B: From<Vec<u8>>,
{
    log::info!("Unsubscribing from topic {} at hub {} ({})", topic, hub, id);

    diesel::delete(websub_subscriptions::table.find(id))
        .execute(conn)
        .unwrap();

    let callback = make_callback(callback.clone(), id);
    let body = serde_urlencoded::to_string(Form::Unsubscribe {
        callback,
        topic: &topic,
    })
    .unwrap();
    send_request(hub, topic, body, client)
}

pub fn unsubscribe_all<S, B>(
    callback: &Uri,
    topic: String,
    client: S,
    conn: &SqliteConnection,
) -> impl Iterator<Item = impl Future<Output = Result<(), S::Error>>>
where
    S: HttpService<B> + Clone,
    B: From<Vec<u8>>,
{
    log::info!("Unsubscribing from topic {} at all hubs", topic);

    let rows = websub_subscriptions::table.filter(websub_subscriptions::topic.eq(&topic));
    let subscriptions = rows
        .select((websub_subscriptions::id, websub_subscriptions::hub))
        .load::<(i64, String)>(conn)
        .unwrap();

    diesel::delete(rows).execute(conn).unwrap();

    let callback = callback.clone();
    subscriptions.into_iter().map(move |(id, hub)| {
        let callback = make_callback(callback.clone(), id);
        let body = serde_urlencoded::to_string(Form::Unsubscribe {
            callback,
            topic: &topic,
        })
        .unwrap();
        send_request(hub, topic.clone(), body, client.clone())
    })
}

fn send_request<S, B>(
    hub: String,
    topic: String,
    body: String,
    client: S,
) -> impl Future<Output = Result<(), S::Error>>
where
    S: HttpService<B>,
    B: From<Vec<u8>>,
{
    let application_www_form_urlencoded = HeaderValue::from_static(APPLICATION_WWW_FORM_URLENCODED);
    let req = http::Request::post(&hub)
        .header(CONTENT_TYPE, application_www_form_urlencoded)
        .body(B::from(body.into_bytes()))
        .unwrap();

    client.into_service().oneshot(req).map_ok(move |res| {
        let status = res.status();

        if status.is_success() {
            return;
        }

        if status.is_redirection() {
            if let Some(to) = res.headers().get(LOCATION) {
                let to = String::from_utf8_lossy(to.as_bytes());
                log::warn!("Topic {} at hub {} redirects to {}", topic, hub, to);
            }
        }

        log::warn!(
            "Topic {} at hub {} returned HTTP status code {}",
            topic,
            hub,
            status
        );
    })
}

fn create_subscription(hub: &str, topic: &str, conn: &SqliteConnection) -> (i64, Secret) {
    let mut rng = rand::thread_rng();

    let secret = gen_secret(&mut rng);

    let id = conn
        .transaction(|| {
            let id = loop {
                let id = (rng.next_u64() >> 1) as i64;
                let result = diesel::insert_into(websub_subscriptions::table)
                    .values((
                        websub_subscriptions::id.eq(id),
                        websub_subscriptions::hub.eq(hub),
                        websub_subscriptions::topic.eq(topic),
                        websub_subscriptions::secret.eq(&*secret),
                    ))
                    .execute(conn);
                match result {
                    Ok(_) => break id,
                    Err(diesel::result::Error::DatabaseError(
                        DatabaseErrorKind::UniqueViolation,
                        _,
                    )) => {} // retry
                    Err(e) => return Err(e),
                }
            };

            diesel::insert_into(websub_pending_subscriptions::table)
                .values(websub_pending_subscriptions::id.eq(id))
                .execute(conn)?;

            Ok(id)
        })
        .unwrap();

    (id, secret)
}

fn make_callback(prefix: Uri, id: i64) -> Uri {
    let id = id.to_le_bytes();
    let id = super::encode_callback_id(&id);
    let mut parts = Parts::from(prefix);
    // `subscriber::prepare_callback_prefix` ensures that `path_and_query` is `Some`.
    let path = format!("{}{}", parts.path_and_query.unwrap(), id);
    parts.path_and_query = Some(PathAndQuery::from_maybe_shared(Bytes::from(path)).unwrap());
    parts.try_into().unwrap()
}

fn gen_secret<R: RngCore>(mut rng: R) -> Secret {
    let mut ret = [0_u8; SECRET_LEN];

    let mut rand = [0_u8; SECRET_LEN * 6 / 8];
    rng.fill_bytes(&mut rand);

    base64::encode_config_slice(&rand, base64::URL_SAFE_NO_PAD, &mut ret);

    unsafe { string::String::from_utf8_unchecked(ret) }
}
