use std::convert::{TryFrom, TryInto};
use std::error::Error;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicI64, Ordering};
use std::task::{Context, Poll};
use std::time::Instant;

use bytes::Buf;
use diesel::dsl::*;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use futures::channel::mpsc;
use futures::task::AtomicWaker;
use futures::{future, pin_mut, stream, Future, FutureExt, TryFutureExt, TryStreamExt};
use hmac::digest::generic_array::typenum::Unsigned;
use hmac::digest::FixedOutput;
use hmac::{Hmac, Mac};
use http::header::CONTENT_TYPE;
use http::{Request, Response, StatusCode, Uri};
use http_body::Body as _;
use hyper::Body;
use sha1::Sha1;
use tower_util::ServiceExt;

use crate::feed::{self, RawFeed};
use crate::query;
use crate::schema::*;
use crate::util::{instant_from_epoch, now_epoch, HttpService, Never};
use crate::websub::{hub, X_HUB_SIGNATURE};

use super::Content;

pub struct Service<S, B> {
    pub(super) host: Uri,
    pub(super) client: S,
    pub(super) pool: Pool<ConnectionManager<SqliteConnection>>,
    pub(super) tx: mpsc::UnboundedSender<(String, Content)>,
    pub(super) renewer_task: AtomicWaker,
    pub(super) expires_at: AtomicI64,
    pub(super) _marker: PhantomData<fn() -> B>,
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

impl<S, B> Service<S, B>
where
    S: HttpService<B> + Clone + Send + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    B: From<Vec<u8>> + Send + 'static,
{
    pub fn discover_and_subscribe(&self, topic: String) -> impl Future<Output = anyhow::Result<()>>
    where
        <S::ResponseBody as http_body::Body>::Error: Error + Send + Sync + 'static,
        B: Default,
    {
        log::info!("Attempting to discover WebSub hubs for topic {}", topic);

        let host = self.host.clone();
        let client = self.client.clone();
        let pool = self.pool.clone();

        let req = http::Request::get(&*topic)
            .body(Default::default())
            .unwrap();
        client
            .clone()
            .into_service()
            .oneshot(req)
            .map_err(Into::into)
            .and_then(|res| async move {
                // TODO: Web Linking discovery

                let kind = if let Some(v) = res.headers().get(CONTENT_TYPE) {
                    if let Some(m) = v.to_str().ok().and_then(|s| s.parse().ok()) {
                        m
                    } else {
                        log::warn!("Topic {}: unsupported media type `{:?}`", topic, v);
                        return Ok(());
                    }
                } else {
                    feed::MediaType::Xml
                };

                let body = res.into_body();
                pin_mut!(body);
                let body = stream::poll_fn(move |cx| body.as_mut().poll_data(cx))
                    .try_fold(Vec::new(), |mut vec, chunk| {
                        vec.extend(chunk.bytes());
                        future::ok(vec)
                    })
                    .await?;

                match RawFeed::parse(kind, &body) {
                    Some(RawFeed::Atom(feed)) => {
                        let hubs = feed
                            .links
                            .into_iter()
                            .filter(|link| link.rel == "hub")
                            .map(|link| link.href);
                        for hub in hubs {
                            tokio::spawn(hub::subscribe(
                                &host,
                                hub,
                                topic.clone(),
                                client.clone(),
                                &*pool.get()?,
                            ));
                        }
                    }
                    Some(RawFeed::Rss(_)) => {
                        log::warn!("Discovery of WebSub hubs for RSS feeds is not supported yet")
                    }
                    None => log::warn!("Topic {}: failed to parse the content", topic),
                }

                Ok(())
            })
    }

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
        let threshold: i64 = (now_epoch + super::RENEW).try_into().unwrap();

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

        let kind: feed::MediaType = if let Some(m) = req
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

                self.reset_renewer(expires_at);

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

    fn reset_renewer(&self, expires_at: i64) {
        let prev = fetch_min(&self.expires_at, expires_at);
        if expires_at < prev {
            self.renewer_task.wake();
        }
    }
}

impl<S, B> Service<S, B> {
    pub fn decode_expires_at(&self) -> Option<Instant> {
        let val = self.expires_at.load(Ordering::SeqCst);
        if val == i64::MAX {
            None
        } else {
            Some(instant_from_epoch(val))
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

// TODO: Use `AtomicI64::fetch_min` once it hits stable.
// https://github.com/rust-lang/rust/issues/48655
fn fetch_min(atomic: &AtomicI64, val: i64) -> i64 {
    let mut prev = atomic.load(Ordering::SeqCst);
    while prev > val {
        match atomic.compare_exchange_weak(prev, val, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return prev,
            Err(p) => prev = p,
        }
    }
    prev
}

fn log_and_discard_error<T, E>(result: Result<T, E>)
where
    E: std::fmt::Debug,
{
    if let Err(e) = result {
        log::error!("An HTTP request failed: {:?}", e);
    }
}