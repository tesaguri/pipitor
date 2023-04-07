mod scheduler;
mod service;

use std::convert::TryInto;
use std::marker::{PhantomData, Unpin};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use diesel::dsl::*;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use futures::channel::mpsc;
use futures::{Stream, StreamExt, TryStream};
use http::Uri;
use hyper::server::conn::Http;
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::feed::{self, Feed};
use crate::manifest::{self, Manifest};
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

// XXX: mediocre naming
const RENEW: Duration = Duration::from_secs(10);

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
        manifest: &Manifest,
        incoming: I,
        host: Uri,
        client: S,
        pool: Pool<ConnectionManager<SqliteConnection>>,
    ) -> Self {
        Self::new_(&manifest.websub, incoming, host, client, pool)
    }

    fn new_(
        manifest: &manifest::Websub,
        incoming: I,
        host: Uri,
        client: S,
        pool: Pool<ConnectionManager<SqliteConnection>>,
    ) -> Self {
        let renewal_margin = manifest.renewal_margin.as_secs();

        let first_tick = query::expires_at()
            .first::<i64>(&pool.get().unwrap())
            .optional()
            .unwrap()
            .map(|expires_at| {
                expires_at
                    .try_into()
                    .map_or(0, |expires_at: u64| expires_at - renewal_margin)
            });

        let (tx, rx) = mpsc::channel(0);

        let service = Arc::new(service::Service {
            host,
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

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::str;

    use diesel::r2d2::ConnectionManager;
    use futures::channel::oneshot;
    use hmac::{Hmac, Mac, NewMac};
    use http::header::CONTENT_TYPE;
    use http::{Method, Request, Response, StatusCode};
    use hyper::server::conn::Http;
    use hyper::{service, Body, Client};
    use sha1::Sha1;

    use crate::util::connection::{Connector, Listener};
    use crate::util::consts::{
        APPLICATION_ATOM_XML, APPLICATION_WWW_FORM_URLENCODED, HUB_SIGNATURE,
    };
    use crate::util::{self, ConcatBody, FutureTimeoutExt};

    use super::super::hub;
    use super::*;

    const TOPIC: &str = "http://example.com/feed.xml";
    const HUB: &str = "http://example.com/hub";
    const FEED: &str = include_str!("testcases/feed.xml");
    const MARGIN: Duration = Duration::from_secs(42);

    #[tokio::test]
    #[ignore]
    async fn test() {
        tokio::time::pause();

        let (subscriber, client, mut listener) = prepare_subscriber();
        let mut subscriber = tokio_test::task::spawn(subscriber);

        let hub = Http::new();

        // Discover the hub from the feed.

        let task = tokio::spawn(subscriber.service().discover(TOPIC.to_owned()));

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
                Ok::<_, Infallible>(res)
            }),
        )
        .timeout()
        .await
        .unwrap();

        let (topic, hubs) = task.timeout().await.unwrap().unwrap();
        assert_eq!(topic, TOPIC);
        let hubs = hubs.unwrap().collect::<Vec<_>>();
        assert_eq!(hubs, [HUB]);

        // Subscribe to the topic.

        let task = subscriber.service().subscribe(
            HUB.to_owned(),
            topic,
            &subscriber.service().pool.get().unwrap(),
        );
        let task = tokio::spawn(task);

        let (tx, rx) = oneshot::channel();
        let mut tx = Some(tx);

        let sock = listener.next().timeout().await.unwrap().unwrap();
        hub.serve_connection(
            sock,
            service::service_fn(move |req| {
                let tx = tx.take().unwrap();
                async move {
                    assert_eq!(req.uri().path_and_query().unwrap(), "/hub");
                    assert_eq!(
                        req.headers().get(CONTENT_TYPE).unwrap(),
                        APPLICATION_WWW_FORM_URLENCODED
                    );
                    let body = ConcatBody::new(req.into_body()).await.unwrap();
                    let form: hub::Form = serde_urlencoded::from_bytes(&body).unwrap();
                    let (callback, topic, secret) = match form {
                        hub::Form::Subscribe {
                            callback,
                            topic,
                            secret,
                        } => (callback, topic, secret),
                        _ => panic!(),
                    };
                    assert_eq!(topic, TOPIC);

                    tx.send((callback, secret)).unwrap();

                    let res = Response::builder()
                        .status(StatusCode::ACCEPTED)
                        .body(Body::empty())
                        .unwrap();
                    Ok::<_, Infallible>(res)
                }
            }),
        )
        .timeout()
        .await
        .unwrap();

        task.timeout().await.unwrap().unwrap();

        let (callback, secret) = rx.await.unwrap();

        // Run `subscriber` in the background.
        let pool = subscriber.service().pool.clone();
        tokio::spawn(async move {
            let (topic, content) = subscriber.next().await.unwrap().unwrap();
            assert_eq!(topic, TOPIC);
            assert_eq!(str::from_utf8(&content.content).unwrap(), FEED);

            assert!(subscriber.next().await.is_none());
        });

        // Hub verifies intent of the subscriber.

        let query = serde_urlencoded::to_string(&hub::Verify::Subscribe {
            topic: TOPIC,
            challenge: "subscription_challenge",
            lease_seconds: (2 * MARGIN).as_secs(),
        })
        .unwrap();
        let res = client
            .get(format!("{}?{}", callback, query).try_into().unwrap())
            .timeout()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = ConcatBody::new(res.into_body()).timeout().await.unwrap();
        assert_eq!(body, b"subscription_challenge");

        // Content distribution.

        let mut mac = Hmac::<Sha1>::new_varkey(secret.as_bytes()).unwrap();
        mac.update(FEED.as_bytes());
        let signature = mac.finalize().into_bytes();
        let req = Request::post(&callback)
            .header(CONTENT_TYPE, APPLICATION_ATOM_XML)
            .header(HUB_SIGNATURE, format!("sha1={}", hex::encode(&*signature)))
            .body(Body::from(FEED))
            .unwrap();
        let res = client.request(req).timeout().await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Subscription renewal.

        // tokio::task::yield_now().await;
        tokio::time::advance(MARGIN).await;

        let (tx, rx) = oneshot::channel();
        let mut tx = Some(tx);

        // FIXME: This line occasionally hangs. If you uncomment `yield_now` above,
        // the hang occurs deterministically.
        let sock = listener.next().timeout().await.unwrap().unwrap();
        let old_callback = callback.clone();
        hub.serve_connection(
            sock,
            service::service_fn(|req| {
                let tx = tx.take().unwrap();
                let old_callback = old_callback.clone();
                async move {
                    assert_eq!(
                        req.headers().get(CONTENT_TYPE).unwrap(),
                        APPLICATION_WWW_FORM_URLENCODED
                    );
                    let body = ConcatBody::new(req.into_body()).await.unwrap();
                    let form: hub::Form = serde_urlencoded::from_bytes(&body).unwrap();
                    let (callback, topic) = match form {
                        hub::Form::Subscribe {
                            callback,
                            topic,
                            secret: _,
                        } => (callback, topic),
                        _ => panic!(),
                    };
                    assert_ne!(callback, old_callback);
                    assert_eq!(topic, TOPIC);

                    tx.send(callback).unwrap();

                    let res = Response::builder()
                        .status(StatusCode::ACCEPTED)
                        .body(Body::empty())
                        .unwrap();
                    Ok::<_, Infallible>(res)
                }
            }),
        )
        .timeout()
        .await
        .unwrap();

        let new_callback = rx.await.unwrap();

        // Hub verifies intent of the subscriber.

        let query = serde_urlencoded::to_string(&hub::Verify::Subscribe {
            topic: TOPIC,
            challenge: "renewal_challenge",
            lease_seconds: (2 * MARGIN).as_secs(),
        })
        .unwrap();
        let res = client
            .get(format!("{}?{}", new_callback, query).try_into().unwrap())
            .timeout()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = ConcatBody::new(res.into_body()).timeout().await.unwrap();
        assert_eq!(body, b"renewal_challenge");

        // `subscriber` should unsubscribe from the old subscription.

        let sock = listener.next().timeout().await.unwrap().unwrap();
        let old_callback = callback.clone();
        hub.serve_connection(
            sock,
            service::service_fn(|req| {
                let old_callback = old_callback.clone();
                async move {
                    assert_eq!(
                        req.headers().get(CONTENT_TYPE).unwrap(),
                        APPLICATION_WWW_FORM_URLENCODED
                    );
                    let body = ConcatBody::new(req.into_body()).await.unwrap();
                    let form: hub::Form = serde_urlencoded::from_bytes(&body).unwrap();
                    let (callback, topic) = match form {
                        hub::Form::Unsubscribe { callback, topic } => (callback, topic),
                        _ => panic!(),
                    };
                    assert_eq!(callback, old_callback);
                    assert_eq!(topic, TOPIC);

                    let res = Response::builder()
                        .status(StatusCode::ACCEPTED)
                        .body(Body::empty())
                        .unwrap();
                    Ok::<_, Infallible>(res)
                }
            }),
        )
        .timeout()
        .await
        .unwrap();

        // Hub verifies intent of the subscriber.

        let query = serde_urlencoded::to_string(&hub::Verify::Unsubscribe {
            topic: TOPIC,
            challenge: "unsubscription_challenge",
        })
        .unwrap();
        let res = client
            .get(format!("{}?{}", callback, query).try_into().unwrap())
            .timeout()
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = ConcatBody::new(res.into_body()).timeout().await.unwrap();
        assert_eq!(body, b"unsubscription_challenge");

        // Old subscription should be deleted from the database.

        let id: i64 = callback.path().rsplit('/').next().unwrap().parse().unwrap();
        let conn = pool.get().unwrap();

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

        let manifest = manifest::Websub {
            renewal_margin: MARGIN,
        };
        let pool = util::r2d2::new_pool(ConnectionManager::new(":memory:")).unwrap();
        crate::migrations::run(&*pool.get().unwrap()).unwrap();
        let subscriber = Subscriber::new_(
            &manifest,
            sub_listener,
            Uri::from_static("http://example.com/"),
            sub_client,
            pool,
        );

        (subscriber, hub_client, hub_listener)
    }
}
