use std::error::Error;
use std::marker::PhantomData;

use futures::{stream, TryStream};
use http::Uri;
use http_body::Body;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::util::{HttpService, Never};
use crate::{websub, Manifest};

use super::{App, Core, Sender};

#[derive(Default)]
pub struct Builder<S, W> {
    client: S,
    websub: W,
}

impl Builder<(), ()> {
    pub fn new() -> Self {
        Default::default()
    }
}

impl<S, W> Builder<S, W> {
    pub fn http_client<S2>(self, client: S2) -> Builder<(S2,), W> {
        Builder {
            client: (client,),
            websub: self.websub,
        }
    }

    pub fn websub<I>(self, incoming: I, host: Uri) -> Builder<S, (I, Uri)> {
        Builder {
            client: self.client,
            websub: (incoming, host),
        }
    }
}

#[cfg(feature = "native-tls")]
impl Builder<(), ()> {
    pub async fn build(
        self,
        manifest: Manifest,
    ) -> anyhow::Result<
        App<
            stream::Pending<Result<Never, Never>>,
            hyper::Client<hyper_tls::HttpsConnector<hyper::client::HttpConnector>>,
            hyper::Body,
        >,
    > {
        let conn = hyper_tls::HttpsConnector::new();
        let client = hyper::Client::builder().build(conn);
        self.http_client(client)
            .build::<hyper::Body>(manifest)
            .await
    }
}

impl<S> Builder<(S,), ()> {
    pub async fn build<B>(
        self,
        manifest: Manifest,
    ) -> anyhow::Result<App<stream::Pending<Result<Never, Never>>, S, B>>
    where
        S: HttpService<B> + Clone + Send + Sync + 'static,
        S::Future: Send,
        S::ResponseBody: Send,
        <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
        B: Default + From<Vec<u8>> + Send + 'static,
    {
        let (client,) = self.client;
        let core = Core::new(manifest, client.clone())?;
        let twitter = core.init_twitter().await?;
        let twitter_list = core.init_twitter_list()?;

        Ok(App {
            core,
            twitter_list,
            twitter,
            twitter_done: false,
            websub: None,
            sender: Sender::new(),
            body_marker: PhantomData,
        })
    }
}

impl<S, I> Builder<(S,), (I, Uri)>
where
    I: TryStream,
    I::Ok: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    I::Error: Error + Send + Sync + 'static,
{
    pub async fn build<B>(self, manifest: Manifest) -> anyhow::Result<App<I, S, B>>
    where
        S: HttpService<B> + Clone + Send + Sync + 'static,
        S::Future: Send,
        S::ResponseBody: Send,
        <S::ResponseBody as Body>::Error: std::error::Error + Send + Sync + 'static,
        B: Default + From<Vec<u8>> + Send + Sync + 'static,
    {
        let app = Builder {
            client: self.client,
            websub: (),
        }
        .build(manifest)
        .await?;

        let (incoming, host) = self.websub;
        let websub = websub::Subscriber::new(
            incoming,
            host,
            app.core.http_client().clone(),
            app.core.database_pool().clone(),
        );

        // XXX: The type-changing FRU syntax (Rust RFC 2528) would be useful here.
        Ok(App {
            core: app.core,
            twitter_list: app.twitter_list,
            twitter: app.twitter,
            twitter_done: app.twitter_done,
            websub: Some(websub),
            sender: app.sender,
            body_marker: app.body_marker,
        })
    }
}
