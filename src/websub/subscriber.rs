mod scheduler;
mod service;

use std::convert::{TryFrom, TryInto};
use std::marker::{PhantomData, Unpin};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use diesel::dsl::*;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use futures::channel::mpsc;
use futures::{Stream, StreamExt, TryStream};
use http::uri::{PathAndQuery, Uri};
use hyper::server::conn::Http;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::feed::{self, Feed};
use crate::manifest;
use crate::query;
use crate::schema::*;
use crate::util::{ArcService, HttpService};

use self::scheduler::Scheduler;
use self::service::Service;

/// A WebSub subscriber server.
#[pin_project]
pub struct Subscriber<S, B, I> {
    #[pin]
    incoming: I,
    server: Http,
    rx: mpsc::Receiver<(String, Content)>,
    service: Arc<Service<S, B>>,
}

pub struct Content {
    kind: feed::MediaType,
    content: Vec<u8>,
}

impl<S, B, I> Subscriber<S, B, I>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    B: Default + From<Vec<u8>> + Send + 'static,
    I: TryStream,
    I::Ok: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    pub fn new(
        manifest: &manifest::WebSub,
        incoming: I,
        client: S,
        pool: Pool<ConnectionManager<SqliteConnection>>,
    ) -> Self {
        let renewal_margin = manifest.renewal_margin.as_secs();

        let first_tick = query::expires_at()
            .first::<i64>(&pool.get().unwrap())
            .optional()
            .unwrap()
            .map(|expires_at| {
                u64::try_from(expires_at).map_or(0, |expires_at| expires_at - renewal_margin)
            });

        let callback = prepare_callback_prefix(manifest.callback.clone());

        let (tx, rx) = mpsc::channel(0);

        let service = Arc::new(service::Service {
            callback,
            renewal_margin,
            client,
            pool,
            tx,
            handle: scheduler::Handle::new(first_tick),
            _marker: PhantomData,
        });

        tokio::spawn(Scheduler::new(&service, |service| {
            let conn = &*service.pool.get().unwrap();
            service.renew_subscriptions(conn);
            query::expires_at()
                .filter(not(
                    websub_active_subscriptions::id.eq_any(query::renewing_subs())
                ))
                .first::<i64>(conn)
                .optional()
                .unwrap()
                .map(|expires_at| {
                    expires_at
                        .try_into()
                        .map_or(0, |expires_at| service.refresh_time(expires_at))
                })
        }));

        Subscriber {
            incoming,
            server: Http::new(),
            rx,
            service,
        }
    }

    fn accept_all(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), I::Error>> {
        let mut this = self.project();
        while let Poll::Ready(option) = this.incoming.as_mut().try_poll_next(cx)? {
            match option {
                None => return Poll::Ready(Ok(())),
                Some(sock) => {
                    let service = ArcService(this.service.clone());
                    tokio::spawn(this.server.serve_connection(sock, service));
                }
            }
        }

        Poll::Pending
    }
}

impl<S, B, I> Subscriber<S, B, I> {
    // XXX: We expose the `Service` type rather than exposing its methods through `Subscriber`
    // to prevent the return types of the methods from being bound by `I`
    // (https://github.com/rust-lang/rust/issues/42940).
    pub fn service(&self) -> &Service<S, B> {
        &self.service
    }
}

/// The `Stream` impl yields topic updates that the server has received.
impl<S, B, I> Stream for Subscriber<S, B, I>
where
    S: HttpService<B> + Clone + Send + Sync + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    B: Default + From<Vec<u8>> + Send + 'static,
    I: TryStream,
    I::Ok: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    type Item = Result<(String, Content), I::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        log::trace!("Subscriber::poll_next");

        let incoming_done = matches!(self.as_mut().accept_all(cx)?, Poll::Ready(()));

        match self.as_mut().project().rx.poll_next_unpin(cx) {
            Poll::Ready(option) => Poll::Ready(option.map(Ok)),
            Poll::Pending => {
                if incoming_done && Arc::strong_count(&self.service) == 1 {
                    Poll::Ready(None)
                } else {
                    Poll::Pending
                }
            }
        }
    }
}

impl Content {
    pub fn parse_feed(&self) -> Option<Feed> {
        Feed::parse(self.kind, &self.content)
    }
}

// Ensure that `prefix` ends with a slash.
fn prepare_callback_prefix(prefix: Uri) -> Uri {
    // Existence of `path_and_query` should have been verified upon deserialization.
    let path = prefix.path_and_query().unwrap().as_str();

    if path.ends_with('/') {
        prefix
    } else {
        let path = {
            let mut buf = String::with_capacity(path.len() + 1);
            buf.push_str(path);
            buf.push('/');
            PathAndQuery::try_from(buf).unwrap()
        };
        let mut parts = prefix.into_parts();
        parts.path_and_query = Some(path);
        Uri::from_parts(parts).unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::str;
    use std::time::Duration;

    use diesel::connection::SimpleConnection;
    use diesel::r2d2::ConnectionManager;
    use futures::channel::oneshot;
    use futures::future;
    use hmac::{Hmac, Mac, NewMac};
    use http::header::CONTENT_TYPE;
    use http::Uri;
    use http::{Method, Request, Response, StatusCode};
    use hyper::server::conn::Http;
    use hyper::{service, Body, Client};
    use sha1::Sha1;

    use crate::util::connection::{Connector, Listener};
    use crate::util::consts::{
        APPLICATION_ATOM_XML, APPLICATION_WWW_FORM_URLENCODED, HUB_SIGNATURE,
    };
    use crate::util::{self, EitherUnwrapExt, FutureTimeoutExt};

    use super::super::hub;
    use super::*;

    const TOPIC: &str = "http://example.com/feed.xml";
    const HUB: &str = "http://example.com/hub";
    const FEED: &str = include_str!("testcases/feed.xml");
    const MARGIN: Duration = Duration::from_secs(42);
    /// Network delay for communications between the subscriber and the hub.
    const DELAY: Duration = Duration::from_millis(1);

    #[test]
    fn callback_prefix() {
        // Should remain intact (root directory).
        let uri = prepare_callback_prefix("http://example.com/".try_into().unwrap());
        assert_eq!(uri, "http://example.com/");

        // Should remain intact (subdirectory).
        let uri = prepare_callback_prefix("http://example.com/websub/".try_into().unwrap());
        assert_eq!(uri, "http://example.com/websub/");

        // Should append a slash (root directory).
        let uri = prepare_callback_prefix("http://example.com".try_into().unwrap());
        assert_eq!(uri, "http://example.com/");

        // Should append a slash (subdirectory).
        let uri = prepare_callback_prefix("http://example.com/websub".try_into().unwrap());
        assert_eq!(uri, "http://example.com/websub/");
    }

    #[tokio::test]
    async fn renew_multi_subs() {
        tokio::time::pause();

        let begin = i64::try_from(util::now_unix().as_secs()).unwrap();

        let pool = util::r2d2::pool_with_builder(
            Pool::builder().max_size(1),
            ConnectionManager::new(":memory:"),
        )
        .unwrap();
        let conn = pool.get().unwrap();
        crate::migrations::run(&*conn).unwrap();

        conn.batch_execute(
            "INSERT INTO websub_subscriptions (hub, topic, secret)
            VALUES
                ('http://example.com/hub', 'http://example.com/topic/1', 'secret1'),
                ('http://example.com/hub', 'http://example.com/topic/2', 'secret2');",
        )
        .unwrap();

        let expiry1 = begin + MARGIN.as_secs() as i64 + 1;
        let expiry2 = expiry1 + MARGIN.as_secs() as i64;
        let values = [
            websub_active_subscriptions::expires_at.eq(expiry1),
            websub_active_subscriptions::expires_at.eq(expiry2),
        ];
        insert_into(websub_active_subscriptions::table)
            .values(&values[..])
            .execute(&*conn)
            .unwrap();

        drop(conn);

        let (mut subscriber, client, listener) = prepare_subscriber_with_pool(pool);
        let mut listener = tokio_test::task::spawn(listener);

        let hub = Http::new();

        listener.enter(|cx, listener| assert!(listener.poll_next(cx).is_pending()));

        // Renewal of first subscription.

        tokio::time::advance(Duration::from_secs(2)).await;

        // Subscriber should re-subscribe to the topic.
        let form = accept_request(&mut listener, &hub, "/hub").timeout().await;
        let callback1 = match form {
            hub::Form::Subscribe {
                callback, topic, ..
            } => {
                assert_eq!(topic, "http://example.com/topic/1");
                callback
            }
            _ => panic!(),
        };
        listener.enter(|cx, listener| assert!(listener.poll_next(cx).is_pending()));

        // Intent verification of the re-subscription.
        let task = verify_intent(
            &client,
            &callback1,
            &hub::Verify::Subscribe {
                topic: "http://example.com/topic/1",
                challenge: "subscription_challenge1",
                // The subscription should never be renewed again in this test.
                lease_seconds: 2 * MARGIN.as_secs() + 2,
            },
        );
        util::first(task.timeout(), subscriber.next())
            .await
            .unwrap_left();

        // Subscriber should unsubscribe from the old subscription.
        let id = super::super::encode_callback_id(&1_i64.to_le_bytes()).to_string();
        let old_callback1 = Uri::try_from(format!("http://example.com/{}", id)).unwrap();
        let form = accept_request(&mut listener, &hub, "/hub").timeout().await;
        match form {
            hub::Form::Unsubscribe {
                callback, topic, ..
            } => {
                assert_eq!(callback, old_callback1);
                assert_eq!(topic, "http://example.com/topic/1");
            }
            _ => panic!(),
        }
        listener.enter(|cx, listener| assert!(listener.poll_next(cx).is_pending()));

        // Intent verification of the unsubscription.
        let task = verify_intent(
            &client,
            &old_callback1,
            &hub::Verify::Unsubscribe {
                topic: "http://example.com/topic/1",
                challenge: "unsubscription_challenge1",
            },
        );
        util::first(task.timeout(), subscriber.next())
            .await
            .unwrap_left();

        // Renewal of second subscription.

        tokio::time::advance(MARGIN).await;

        let form = accept_request(&mut listener, &hub, "/hub").timeout().await;
        let callback2 = match form {
            hub::Form::Subscribe {
                callback, topic, ..
            } => {
                assert_eq!(topic, "http://example.com/topic/2");
                callback
            }
            _ => panic!(),
        };
        listener.enter(|cx, listener| assert!(listener.poll_next(cx).is_pending()));

        let task = verify_intent(
            &client,
            &callback2,
            &hub::Verify::Subscribe {
                topic: "http://example.com/topic/2",
                challenge: "subscription_challenge2",
                lease_seconds: MARGIN.as_secs() + 1,
            },
        );
        util::first(task.timeout(), subscriber.next())
            .await
            .unwrap_left();

        let id = super::super::encode_callback_id(&2_i64.to_le_bytes()).to_string();
        let old_callback2 = Uri::try_from(format!("http://example.com/{}", id)).unwrap();
        let form = accept_request(&mut listener, &hub, "/hub").timeout().await;
        match form {
            hub::Form::Unsubscribe {
                callback, topic, ..
            } => {
                assert_eq!(callback, old_callback2);
                assert_eq!(topic, "http://example.com/topic/2");
            }
            _ => panic!(),
        }
        listener.enter(|cx, listener| assert!(listener.poll_next(cx).is_pending()));

        let task = verify_intent(
            &client,
            &old_callback1,
            &hub::Verify::Unsubscribe {
                topic: "http://example.com/topic/2",
                challenge: "unsubscription_challenge2",
            },
        );
        util::first(task.timeout(), subscriber.next())
            .await
            .unwrap_left();
    }

    /// Go through the entire protocol flow at once.
    #[tokio::test]
    async fn entire_flow() {
        tokio::time::pause();

        let (subscriber, client, listener) = prepare_subscriber();
        let mut subscriber = tokio_test::task::spawn(subscriber);
        let mut listener = tokio_test::task::spawn(listener);

        let hub = Http::new();
        listener.enter(|cx, listener| assert!(listener.poll_next(cx).is_pending()));

        // Discover the hub from the feed.

        let task = tokio::spawn(subscriber.service().discover(TOPIC.to_owned()));

        tokio::time::advance(DELAY).await;
        let sock = listener.next().timeout().await.unwrap().unwrap();
        hub.serve_connection(
            sock,
            service::service_fn(|req| async move {
                assert_eq!(req.method(), Method::GET);
                assert_eq!(req.uri().path_and_query().unwrap(), "/feed.xml");
                let res = Response::builder()
                    .header(CONTENT_TYPE, APPLICATION_ATOM_XML)
                    .body(Body::from(FEED))
                    .unwrap();
                tokio::time::advance(DELAY).await;
                Ok::<_, Infallible>(res)
            }),
        )
        .timeout()
        .await
        .unwrap();
        listener.enter(|cx, listener| assert!(listener.poll_next(cx).is_pending()));

        let (topic, hubs) = task.timeout().await.unwrap().unwrap();
        assert_eq!(topic, TOPIC);
        let hubs = hubs.unwrap().collect::<Vec<_>>();
        assert_eq!(hubs, [HUB]);

        // Subscribe to the topic.

        let req_task = subscriber.service().subscribe(
            HUB.to_owned(),
            topic,
            &subscriber.service().pool.get().unwrap(),
        );
        tokio::time::advance(DELAY).await;
        let accept_task = accept_request(&mut listener, &hub, "/hub");
        let (result, form) = future::join(req_task, accept_task).timeout().await;
        result.unwrap();
        let (callback, topic, secret) = match form {
            hub::Form::Subscribe {
                callback,
                topic,
                secret,
            } => (callback, topic, secret),
            _ => panic!(),
        };
        assert_eq!(topic, TOPIC);
        listener.enter(|cx, listener| assert!(listener.poll_next(cx).is_pending()));

        // Hub verifies intent of the subscriber.

        let task = verify_intent(
            &client,
            &callback,
            &hub::Verify::Subscribe {
                topic: TOPIC,
                challenge: "subscription_challenge",
                lease_seconds: (2 * MARGIN).as_secs(),
            },
        );
        util::first(task.timeout(), subscriber.next())
            .await
            .unwrap_left();

        // Content distribution.

        let mut mac = Hmac::<Sha1>::new_varkey(secret.as_bytes()).unwrap();
        mac.update(FEED.as_bytes());
        let signature = mac.finalize().into_bytes();
        let req = Request::post(&callback)
            .header(CONTENT_TYPE, APPLICATION_ATOM_XML)
            .header(HUB_SIGNATURE, format!("sha1={}", hex::encode(&*signature)))
            .body(Body::from(FEED))
            .unwrap();
        let task = client.request(req);
        let (res, update) = future::join(task, subscriber.next()).timeout().await;

        let res = res.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let (topic, content) = update.unwrap().unwrap();
        assert_eq!(topic, TOPIC);
        assert_eq!(str::from_utf8(&content.content).unwrap(), FEED);

        // Subscription renewal.

        tokio::time::advance(MARGIN).await;

        tokio::time::advance(DELAY).await;
        let task = accept_request(&mut listener, &hub, "/hub").timeout();
        let form = util::first(task, subscriber.next()).await.unwrap_left();
        let (new_callback, topic) = match form {
            hub::Form::Subscribe {
                callback,
                topic,
                secret: _,
            } => (callback, topic),
            _ => panic!(),
        };
        assert_ne!(new_callback, callback);
        assert_eq!(topic, TOPIC);
        listener.enter(|cx, listener| assert!(listener.poll_next(cx).is_pending()));

        // Hub verifies intent of the subscriber.

        let task = verify_intent(
            &client,
            &new_callback,
            &hub::Verify::Subscribe {
                topic: TOPIC,
                challenge: "renewal_challenge",
                lease_seconds: (2 * MARGIN).as_secs(),
            },
        );
        util::first(task.timeout(), subscriber.next())
            .await
            .unwrap_left();

        // `subscriber` should unsubscribe from the old subscription.

        tokio::time::advance(DELAY).await;
        let task = accept_request(&mut listener, &hub, "/hub").timeout();
        let form = util::first(task, subscriber.next()).await.unwrap_left();
        let (unsubscribed, topic) = match form {
            hub::Form::Unsubscribe { callback, topic } => (callback, topic),
            _ => panic!(),
        };
        assert_eq!(unsubscribed, callback);
        assert_eq!(topic, TOPIC);
        // FIXME: The listener unexpectedly receives a subscription request.
        listener.enter(|cx, listener| assert!(listener.poll_next(cx).is_pending()));

        // Hub verifies intent of the subscriber.

        let task = verify_intent(
            &client,
            &callback,
            &hub::Verify::Unsubscribe {
                topic: TOPIC,
                challenge: "unsubscription_challenge",
            },
        );
        util::first(task.timeout(), subscriber.next())
            .await
            .unwrap_left();

        // Subscriber should have processed all requests.

        subscriber.enter(|cx, subscriber| assert!(subscriber.poll_next(cx).is_pending()));

        // Old subscription should be deleted from the database.

        let id = callback.path().rsplit('/').next().unwrap();
        let id = crate::websub::decode_callback_id(id).unwrap() as i64;
        let conn = subscriber.service().pool.get().unwrap();

        let row = websub_subscriptions::table.find(id);
        assert!(!select(exists(row)).get_result::<bool>(&conn).unwrap());
        let row = websub_active_subscriptions::table.find(id);
        assert!(!select(exists(row)).get_result::<bool>(&conn).unwrap());
        let row =
            websub_renewing_subscriptions::table.filter(websub_renewing_subscriptions::old.eq(id));
        assert!(!select(exists(row)).get_result::<bool>(&conn).unwrap());
    }

    fn prepare_subscriber() -> (
        Subscriber<Client<Connector>, Body, Listener>,
        Client<Connector>,
        Listener,
    ) {
        let pool = util::r2d2::pool_with_builder(
            Pool::builder().max_size(1),
            ConnectionManager::new(":memory:"),
        )
        .unwrap();
        crate::migrations::run(&*pool.get().unwrap()).unwrap();
        prepare_subscriber_with_pool(pool)
    }

    fn prepare_subscriber_with_pool(
        pool: Pool<ConnectionManager<SqliteConnection>>,
    ) -> (
        Subscriber<Client<Connector>, Body, Listener>,
        Client<Connector>,
        Listener,
    ) {
        let (hub_conn, sub_listener) = util::connection();
        let (sub_conn, hub_listener) = util::connection();
        let sub_client = Client::builder()
            // https://github.com/hyperium/hyper/issues/2312#issuecomment-722125137
            .pool_idle_timeout(Duration::from_secs(0))
            .pool_max_idle_per_host(0)
            .build::<_, Body>(sub_conn);
        let hub_client = Client::builder()
            .pool_idle_timeout(Duration::from_secs(0))
            .pool_max_idle_per_host(0)
            .build::<_, Body>(hub_conn);

        let manifest = manifest::WebSub {
            callback: Uri::from_static("http://example.com/"),
            bind: None,
            renewal_margin: MARGIN,
        };
        let subscriber = Subscriber::new(&manifest, sub_listener, sub_client, pool);

        (subscriber, hub_client, hub_listener)
    }

    async fn accept_request<'a>(
        listener: &'a mut Listener,
        http: &'a Http,
        path: &'static str,
    ) -> hub::Form {
        let (tx, rx) = oneshot::channel();
        let mut tx = Some(tx);

        let sock = listener.next().await;
        http.serve_connection(
            sock.unwrap().unwrap(),
            service::service_fn(move |req| {
                let tx = tx.take().unwrap();
                async move {
                    assert_eq!(req.uri().path_and_query().unwrap(), path);
                    assert_eq!(
                        req.headers().get(CONTENT_TYPE).unwrap(),
                        APPLICATION_WWW_FORM_URLENCODED
                    );
                    let body = hyper::body::to_bytes(req).await.unwrap();
                    let form: hub::Form = serde_urlencoded::from_bytes(&body).unwrap();
                    tx.send(form).unwrap();

                    let res = Response::builder()
                        .status(StatusCode::ACCEPTED)
                        .body(Body::empty())
                        .unwrap();
                    tokio::time::advance(DELAY).await;
                    Ok::<_, Infallible>(res)
                }
            }),
        )
        .await
        .unwrap();
        rx.await.unwrap()
    }

    fn verify_intent<'a>(
        client: &Client<Connector>,
        callback: &Uri,
        query: &hub::Verify<&'a str>,
    ) -> impl std::future::Future<Output = ()> + 'a {
        let challenge = match *query {
            hub::Verify::Subscribe { challenge, .. }
            | hub::Verify::Unsubscribe { challenge, .. } => challenge,
        };
        let query = serde_urlencoded::to_string(query).unwrap();
        let res = client.get(format!("{}?{}", callback, query).try_into().unwrap());
        async move {
            let res = res.await.unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let body = hyper::body::to_bytes(res).timeout().await.unwrap();
            assert_eq!(body, challenge.as_bytes());
        }
    }
}
