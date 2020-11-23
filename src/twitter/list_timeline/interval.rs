use std::cmp;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::task::AtomicWaker;
use futures::{ready, FutureExt, Stream};
use tokio::time::Delay;

use crate::util;

/// Like `tokio::time::Interval`, but can be controlled by a `Handle`.
pub struct Interval<T> {
    handle: Weak<T>,
    delay: Delay,
}

pub struct Handle {
    period: Duration,
    /// The time until which the `Interval` waits.
    next_tick: AtomicU64,
    waker: AtomicWaker,
}

impl<T> Interval<T>
where
    T: AsRef<Handle>,
{
    pub fn new(handle: Weak<T>) -> Self {
        Self::at(Instant::now(), handle)
    }

    pub fn at(at: Instant, handle: Weak<T>) -> Self {
        Interval {
            handle,
            delay: tokio::time::delay_until(at.into()),
        }
    }
}

impl<T> Stream for Interval<T>
where
    T: AsRef<Handle>,
{
    type Item = Arc<T>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Arc<T>>> {
        let t = if let Some(handle) = self.handle.upgrade() {
            handle
        } else {
            return Poll::Ready(None);
        };
        let handle = (*t).as_ref();

        handle.waker.register(cx.waker());

        if let Some(next_tick) = handle.decode_next_tick() {
            if self.delay.deadline().into_std() < next_tick {
                self.delay.reset(next_tick.into());
            }
        }

        ready!(self.delay.poll_unpin(cx));

        let now = Instant::now();
        let next_tick = self.delay.deadline() + handle.period;
        self.delay.reset(cmp::max(next_tick, now.into()));

        Poll::Ready(Some(t))
    }
}

impl Handle {
    pub fn new(period: Duration) -> Self {
        Handle {
            period,
            next_tick: AtomicU64::new(0),
            waker: AtomicWaker::new(),
        }
    }

    /// Delays the next tick of the associated `Interval` to the specified time.
    ///
    /// Does nothing if the current next tick is after the specified time.
    pub fn delay(&self, after: u64) {
        self.next_tick.fetch_max(after, Ordering::Relaxed);
    }

    fn decode_next_tick(&self) -> Option<Instant> {
        let next_tick = self.next_tick.swap(0, Ordering::Relaxed);
        if next_tick == 0 {
            None
        } else {
            Some(util::instant_from_unix(Duration::from_secs(next_tick)))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.waker.wake();
    }
}
