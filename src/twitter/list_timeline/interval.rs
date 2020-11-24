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
        Self::at(util::instant_now(), handle)
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

        let now = util::instant_now();
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

impl AsRef<Self> for Handle {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.waker.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PERIOD: Duration = Duration::from_secs(42);

    #[tokio::test]
    async fn interval() {
        tokio::time::pause();

        let handle = Arc::new(Handle::new(PERIOD));
        let interval = Interval::new(Arc::downgrade(&handle));
        let mut task = tokio_test::task::spawn(interval);

        task.enter(|cx, interval| assert!(interval.poll_next(cx).is_pending()));
        assert!(!task.is_woken());

        // Advance the time a little because `Delay` does not yield when `now == deadline` exactly.
        tokio::time::advance(Duration::from_nanos(1)).await;

        // `interval` created by `Interval::new` should yield immediately (+ 1 nsec).
        assert!(task.is_woken());
        task.enter(|cx, mut interval| {
            assert!(interval.as_mut().poll_next(cx).is_ready());
            assert!(interval.poll_next(cx).is_pending());
        });

        tokio::time::advance(PERIOD).await;
        assert!(task.is_woken());
        task.enter(|cx, mut interval| {
            assert!(interval.as_mut().poll_next(cx).is_ready());
            assert!(interval.poll_next(cx).is_pending());
        });

        tokio::time::advance(PERIOD / 2).await;
        assert!(!task.is_woken());
        task.enter(|cx, interval| assert!(interval.poll_next(cx).is_pending()));

        tokio::time::advance(PERIOD / 2).await;
        assert!(task.is_woken());
        task.enter(|cx, mut interval| {
            assert!(interval.as_mut().poll_next(cx).is_ready());
            assert!(interval.poll_next(cx).is_pending());
        });

        // FIXME: `interval` should yield only once here, but currently it yields twice.
        // tokio::time::advance(2 * PERIOD).await;
        // assert!(task.is_woken());
        // task.enter(|cx, mut interval| {
        //     assert!(interval.as_mut().poll_next(cx).is_ready());
        //     assert!(interval.poll_next(cx).is_pending());
        // });
    }

    #[tokio::test]
    async fn overdue_multiple_periods() {
        tokio::time::pause();

        let handle = Arc::new(Handle::new(PERIOD));
        let interval = Interval::new(Arc::downgrade(&handle));
        let mut task = tokio_test::task::spawn(interval);

        task.enter(|cx, interval| assert!(interval.poll_next(cx).is_pending()));

        // `interval` should yield only once even when multiple of `PERIOD` of time has passed
        // since the last `poll`.
        tokio::time::advance(3 * PERIOD).await;
        assert!(task.is_woken());
        task.enter(|cx, mut interval| {
            assert!(interval.as_mut().poll_next(cx).is_ready());
            assert!(interval.poll_next(cx).is_pending());
        });
    }

    #[tokio::test]
    async fn delay() {
        tokio::time::pause();

        let handle = Arc::new(Handle::new(PERIOD));
        let interval = Interval::at(util::instant_now() + PERIOD, Arc::downgrade(&handle));
        let mut task = tokio_test::task::spawn(interval);

        tokio::time::advance(Duration::from_nanos(1)).await;

        task.enter(|cx, interval| assert!(interval.poll_next(cx).is_pending()));

        handle.delay((util::now_unix() + 3 * PERIOD).as_secs());
        // `handle.delay` does not wake the `task`.
        assert!(!task.is_woken());
        tokio::time::advance(2 * PERIOD).await;
        // The `advance` should wake the `task` because the inner `Delay`'s deadline has not been
        // `reset()` yet.
        assert!(task.is_woken());
        task.enter(|cx, interval| assert!(interval.poll_next(cx).is_pending()));

        tokio::time::advance(PERIOD).await;
        assert!(task.is_woken());
        task.enter(|cx, mut interval| {
            assert!(interval.as_mut().poll_next(cx).is_ready());
            assert!(interval.poll_next(cx).is_pending());
        });
    }
}
