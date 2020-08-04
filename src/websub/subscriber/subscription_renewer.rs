use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};

use diesel::dsl::*;
use diesel::prelude::*;
use futures::{ready, FutureExt};

use crate::query;
use crate::schema::*;
use crate::util::{instant_from_epoch, HttpService};

use super::{refresh_time, Service};

pub struct Renewer<S, B> {
    service: Weak<Service<S, B>>,
    timer: Option<tokio::time::Delay>,
}

impl<S, B> Renewer<S, B> {
    pub fn new(service: &Arc<Service<S, B>>) -> Self {
        Renewer {
            service: Arc::downgrade(service),
            timer: service
                .decode_expires_at()
                .map(|expires_at| tokio::time::delay_until(refresh_time(expires_at).into())),
        }
    }
}

impl<S, B> Future for Renewer<S, B>
where
    S: HttpService<B> + Clone + Send + 'static,
    S::Future: Send,
    S::ResponseBody: Send,
    B: From<Vec<u8>> + Send + 'static,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        trace_fn!(Renewer::<S, B>::poll);

        let service = if let Some(service) = self.service.upgrade() {
            service
        } else {
            return Poll::Ready(());
        };

        service.renewer_task.register(cx.waker());

        let timer = if let Some(ref mut timer) = self.timer {
            if let Some(expires_at) = service.decode_expires_at() {
                let refresh_time = refresh_time(expires_at);
                if refresh_time < timer.deadline().into_std() {
                    timer.reset(refresh_time.into());
                }
            }
            timer
        } else {
            if let Some(expires_at) = service.decode_expires_at() {
                self.timer = Some(tokio::time::delay_until(refresh_time(expires_at).into()));
                self.timer.as_mut().unwrap()
            } else {
                return Poll::Pending;
            }
        };

        ready!(timer.poll_unpin(cx));

        let conn = &*service.pool.get().unwrap();
        service.renew_subscriptions(conn);

        if let Some(expires_at) = query::expires_at()
            .filter(not(
                websub_active_subscriptions::id.eq_any(query::renewing_subs())
            ))
            .first::<i64>(conn)
            .optional()
            .unwrap()
        {
            service.expires_at.store(expires_at, Ordering::SeqCst);
            let refresh_time = refresh_time(instant_from_epoch(expires_at));
            timer.reset(refresh_time.into());
        } else {
            service.expires_at.store(i64::MAX, Ordering::SeqCst);
            self.timer = None;
        }

        self.poll(cx)
    }
}
