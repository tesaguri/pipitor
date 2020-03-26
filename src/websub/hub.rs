use std::convert::TryInto;
use std::str;

use diesel::prelude::*;
use diesel::result::DatabaseErrorKind;
use diesel::SqliteConnection;
use futures::{Future, TryFutureExt};
use http::header::{CONTENT_TYPE, LOCATION};
use http::uri::{Parts, Uri};
use rand::RngCore;
use tower_util::ServiceExt;

use crate::schema::*;
use crate::util::HttpService;

#[derive(serde::Serialize)]
#[serde(tag = "hub.mode")]
#[serde(rename_all = "lowercase")]
enum Form<'a> {
    Subscribe {
        #[serde(rename = "hub.callback")]
        #[serde(serialize_with = "serialize_uri")]
        callback: &'a Uri,
        #[serde(rename = "hub.topic")]
        topic: &'a str,
        #[serde(rename = "hub.secret")]
        secret: &'a str,
    },
    Unsubscribe {
        #[serde(rename = "hub.callback")]
        #[serde(serialize_with = "serialize_uri")]
        callback: &'a Uri,
        #[serde(rename = "hub.topic")]
        topic: &'a str,
    },
}

const SECRET_LEN: usize = 32;
type Secret = string::String<[u8; SECRET_LEN]>;

pub fn subscribe<S, B>(
    host: &Uri,
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
        callback: &callback(host.clone(), id),
        topic: &topic,
        secret: &secret,
    })
    .unwrap();

    send_request(hub, topic, body, client)
}

pub fn renew<S, B>(
    host: &Uri,
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
        callback: &callback(host.clone(), new),
        topic: &topic,
        secret: &secret,
    })
    .unwrap();

    send_request(hub, topic, body, client)
}

pub fn unsubscribe<S, B>(
    host: &Uri,
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

    let callback = &callback(host.clone(), id);
    let body = serde_urlencoded::to_string(Form::Unsubscribe {
        callback,
        topic: &topic,
    })
    .unwrap();
    send_request(hub, topic, body, client)
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
    let req = http::Request::post(&hub)
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
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
                    )) => {
                        // retry
                    }
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

fn callback(host: Uri, id: i64) -> Uri {
    let mut parts = Parts::from(host);
    parts.path_and_query = Some((*format!("/websub/callback/{}", id)).try_into().unwrap());
    parts.try_into().unwrap()
}

fn gen_secret<R: RngCore>(mut rng: R) -> Secret {
    let mut ret = [0u8; SECRET_LEN];

    let mut rand = [0u8; SECRET_LEN * 6 / 8];
    rng.fill_bytes(&mut rand);

    let config = base64::Config::new(base64::CharacterSet::UrlSafe, false);
    base64::encode_config_slice(&rand, config, &mut ret);

    unsafe { string::String::from_utf8_unchecked(ret) }
}

fn serialize_uri<S: serde::Serializer>(uri: &Uri, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(uri)
}
