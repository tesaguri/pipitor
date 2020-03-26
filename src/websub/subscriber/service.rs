use std::convert::{TryFrom, TryInto};
use std::marker::PhantomData;
use std::task::{Context, Poll};

use diesel::dsl::*;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use futures::channel::mpsc;
use futures::{future, Future, FutureExt, TryFutureExt, TryStreamExt};
use hmac::digest::generic_array::typenum::Unsigned;
use hmac::digest::FixedOutput;
use hmac::{Hmac, Mac};
use http::header::CONTENT_TYPE;
use http::{Request, Response, StatusCode, Uri};
use hyper::Body;
use sha1::Sha1;

use crate::query;
use crate::schema::*;
use crate::util::{now_epoch, HttpService, Never};
use crate::websub::{hub, X_HUB_SIGNATURE};

use super::Content;

pub struct Service<S, B> {
    pub host: Uri,
    pub client: S,
    pub pool: Pool<ConnectionManager<SqliteConnection>>,
    pub(super) tx: mpsc::UnboundedSender<(String, Content)>,
    pub _marker: PhantomData<fn() -> B>,
}

#[derive(serde::Deserialize, Debug)]
#[serde(tag = "hub.mode")]
enum Verify {
    #[serde(rename = "subscribe")]
    Subscribe {
        #[serde(rename = "hub.topic")]
        topic: String,
        #[serde(rename = "hub.challenge")]
        challenge: String,
        #[serde(rename = "hub.lease_seconds")]
        #[serde(deserialize_with = "crate::util::deserialize_from_str")]
        lease_seconds: u64,
    },
    #[serde(rename = "unsubscribe")]
    Unsubscribe {
        #[serde(rename = "hub.topic")]
        topic: String,
        #[serde(rename = "hub.challenge")]
        challenge: String,
    },
}

// XXX: mediocre naming
const RENEW: u64 = 10;

impl<S, B> Service<S, B>
where
    S: HttpService<B> + Clone + Send + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    B: From<Vec<u8>> + Send + 'static,
{
    pub fn subscribe(
        &self,
        hub: String,
        topic: String,
        conn: &SqliteConnection,
    ) -> impl Future<Output = Result<(), S::Error>> {
        hub::subscribe(&self.host, hub, topic, self.client.clone(), conn)
    }

    pub fn renew_subscriptions(&self, conn: &SqliteConnection) {
        let now_epoch = now_epoch();
        let threshold: i64 = (now_epoch + RENEW).try_into().unwrap();

        let expiring = websub_subscriptions::table
            .inner_join(websub_active_subscriptions::table)
            .select((
                websub_subscriptions::id,
                websub_subscriptions::hub,
                websub_subscriptions::topic,
            ))
            .filter(websub_active_subscriptions::expires_at.le(threshold))
            .filter(not(websub_subscriptions::id.eq_any(query::renewing_subs())))
            .load::<(i64, String, String)>(conn)
            .unwrap();

        if expiring.is_empty() {
            return;
        }

        log::info!("Renewing {} expiring subscription(s)", expiring.len());

        for (id, hub, topic) in expiring {
            tokio::spawn(self.renew(id, hub, topic, &conn).map(log_and_discard_error));
        }
    }

    fn renew(
        &self,
        id: i64,
        hub: String,
        topic: String,
        conn: &SqliteConnection,
    ) -> impl Future<Output = Result<(), S::Error>> {
        hub::renew(&self.host, id, hub, topic, self.client.clone(), conn)
    }

    fn unsubscribe(
        &self,
        id: i64,
        hub: String,
        topic: String,
        conn: &SqliteConnection,
    ) -> impl Future<Output = Result<(), S::Error>> {
        hub::unsubscribe(&self.host, id, hub, topic, self.client.clone(), conn)
    }

    fn call(&self, req: Request<Body>) -> Response<Body> {
        macro_rules! validate {
            ($input:expr) => {
                match $input {
                    Ok(x) => x,
                    Err(_) => {
                        return Response::builder()
                            .status(StatusCode::BAD_REQUEST)
                            .body(Default::default())
                            .unwrap();
                    }
                }
            };
        }

        let path = req.uri().path();
        let id = if path.starts_with(crate::websub::CALLBACK_PREFIX) {
            let id: u64 = validate!(path[crate::websub::CALLBACK_PREFIX.len()..].parse());
            validate!(i64::try_from(id))
        } else {
            return Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Default::default())
                .unwrap();
        };

        let conn = self.pool.get().unwrap();

        if let Some(q) = req.uri().query() {
            return self.verify_intent(id, q, &conn);
        }

        let kind: super::MediaType = if let Some(m) = req
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse().ok())
        {
            m
        } else {
            return Response::builder()
                .status(StatusCode::UNSUPPORTED_MEDIA_TYPE)
                .body(Default::default())
                .unwrap();
        };

        let signature_header = if let Some(v) = req.headers().get(X_HUB_SIGNATURE) {
            v.as_bytes()
        } else {
            log::debug!("Callback {}: missing signature", id);
            return Response::new(Default::default());
        };

        let pos = signature_header.iter().position(|&b| b == b'=');
        let (method, signature_hex) = if let Some(i) = pos {
            let (method, hex) = signature_header.split_at(i);
            (method, &hex[1..])
        } else {
            log::debug!("Callback {}: malformed signature", id);
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(Default::default())
                .unwrap();
        };

        let signature = match method {
            b"sha1" => {
                const LEN: usize = <<Sha1 as FixedOutput>::OutputSize as Unsigned>::USIZE;
                let mut buf = [0u8; LEN];
                validate!(hex::decode_to_slice(signature_hex, &mut buf));
                buf
            }
            _ => {
                let method = String::from_utf8_lossy(method);
                log::debug!("Callback {}: unknown digest algorithm: {}", id, method);
                return Response::builder()
                    .status(StatusCode::NOT_ACCEPTABLE)
                    .body(Default::default())
                    .unwrap();
            }
        };

        let (topic, mac) = {
            let cols = websub_subscriptions::table
                .select((websub_subscriptions::topic, websub_subscriptions::secret))
                .find(id)
                .get_result::<(String, String)>(&conn)
                .optional()
                .unwrap();
            let (topic, secret) = if let Some(cols) = cols {
                cols
            } else {
                return Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Default::default())
                    .unwrap();
            };
            let mac = Hmac::<Sha1>::new_varkey(secret.as_bytes()).unwrap();
            (topic, mac)
        };

        let tx = self.tx.clone();
        let verify_signature = req
            .into_body()
            .try_fold((Vec::new(), mac), move |(mut vec, mut mac), chunk| {
                vec.extend(&*chunk);
                mac.input(&chunk);
                future::ok((vec, mac))
            })
            .map_ok(move |(content, mac)| {
                let code = mac.result().code();
                if *code == signature {
                    tx.unbounded_send((topic, Content { kind, content }))
                        .unwrap();
                } else {
                    log::debug!("Callback {}: signature mismatch", id);
                }
            })
            .map_err(move |e| log::debug!("Callback {}: failed to load request body: {:?}", id, e))
            .map(|_| ());
        tokio::spawn(verify_signature);

        Response::new(Default::default())
    }

    fn verify_intent(&self, id: i64, query: &str, conn: &SqliteConnection) -> Response<Body> {
        let row = |topic| {
            websub_subscriptions::table
                .filter(websub_subscriptions::id.eq(id))
                .filter(websub_subscriptions::topic.eq(topic))
        };
        let sub_is_active = websub_subscriptions::id
            .eq_any(websub_active_subscriptions::table.select(websub_active_subscriptions::id));

        match serde_urlencoded::from_str::<Verify>(query) {
            Ok(Verify::Subscribe {
                topic,
                challenge,
                lease_seconds,
            }) if select(exists(row(&topic).filter(not(sub_is_active))))
                .get_result(conn)
                .unwrap() =>
            {
                log::info!("Verifying subscription {}", id);

                let now_epoch = now_epoch();
                let expires_at = now_epoch
                    .saturating_add(lease_seconds)
                    .try_into()
                    .unwrap_or(i64::max_value());

                // Remove the old subscription if the subscription was created by a renewal.
                let old_id = websub_renewing_subscriptions::table
                    .filter(websub_renewing_subscriptions::new.eq(id))
                    .select(websub_renewing_subscriptions::old)
                    .get_result::<i64>(conn)
                    .optional()
                    .unwrap();
                if let Some(old_id) = old_id {
                    let hub = websub_subscriptions::table
                        .select(websub_subscriptions::hub)
                        .find(id)
                        .get_result::<String>(conn)
                        .unwrap();
                    log::info!("Removing the old subscription");
                    tokio::spawn(
                        self.unsubscribe(old_id, hub, topic, conn)
                            .map(log_and_discard_error),
                    );
                }

                conn.transaction(|| {
                    delete(websub_pending_subscriptions::table.find(id)).execute(conn)?;
                    insert_into(websub_active_subscriptions::table)
                        .values((
                            websub_active_subscriptions::id.eq(id),
                            websub_active_subscriptions::expires_at.eq(expires_at),
                        ))
                        .execute(conn)
                })
                .unwrap();

                Response::new(challenge.into())
            }
            Ok(Verify::Unsubscribe { topic, challenge })
                if select(not(exists(row(&topic)))).get_result(conn).unwrap() =>
            {
                log::info!("Vefirying unsubscription of {}", id);
                Response::new(challenge.into())
            }
            _ => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Default::default())
                .unwrap(),
        }
    }
}

impl<S, B> tower_service::Service<Request<Body>> for &Service<S, B>
where
    S: HttpService<B> + Clone + Send + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    B: From<Vec<u8>> + Send + 'static,
{
    type Response = Response<Body>;
    type Error = Never;
    type Future = future::Ready<Result<Self::Response, Self::Error>>;

    fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: Request<Body>) -> Self::Future {
        log::trace!("Service::call; req.uri()={:?}", req.uri());
        future::ok((*self).call(req))
    }
}

fn log_and_discard_error<T, E>(result: Result<T, E>)
where
    E: std::fmt::Debug,
{
    if let Err(e) = result {
        log::error!("An HTTP request failed: {:?}", e);
    }
}
