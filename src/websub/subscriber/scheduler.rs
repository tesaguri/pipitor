use std::future::Future;
use std::marker::Unpin;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use futures::task::AtomicWaker;
use futures::{ready, FutureExt};

use crate::util::instant_from_unix;

/// A `Future` that executes the specified function in a scheduled manner.
//
// This employs a similar idea as `crate::twitter::list_timeline::interval::Interval`
// with a few differences: unlike `Interval`, `Scheduler` resets the next tick based on the
// WebSub subscription's expiration time in the database, so it takes a closure to "inject" a logic
// to determine the next tick.
pub struct Scheduler<T, F> {
    handle: Weak<T>,
    delay: Option<tokio::time::Delay>,
    get_next_tick: F,
}

pub struct Handle {
    next_tick: AtomicU64,
    task: AtomicWaker,
}

impl<T, F> Scheduler<T, F>
where
    T: AsRef<Handle>,
    F: FnMut(&Arc<T>) -> Option<u64> + Unpin,
{
    pub fn new(handle: &Arc<T>, get_next_tick: F) -> Self {
        Scheduler {
            delay: (**handle)
                .as_ref()
                .decode_next_tick()
                .map(|next_tick| tokio::time::delay_until(next_tick.into())),
            handle: Arc::downgrade(handle),
            get_next_tick,
        }
    }
}

impl<T, F> Future for Scheduler<T, F>
where
    T: AsRef<Handle>,
    F: FnMut(&Arc<T>) -> Option<u64> + Unpin,
{
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        trace_fn!(Scheduler::<T, F>::poll);

        let this = &mut *self;

        let t = if let Some(t) = this.handle.upgrade() {
            t
        } else {
            return Poll::Ready(());
        };
        let handle = (*t).as_ref();

        handle.task.register(cx.waker());

        let delay = if let Some(ref mut delay) = this.delay {
            if let Some(next_tick) = handle.decode_next_tick() {
                if next_tick < delay.deadline().into_std() {
                    delay.reset(next_tick.into());
                }
            }
            delay
        } else if let Some(next_tick) = handle.decode_next_tick() {
            this.delay = Some(tokio::time::delay_until(next_tick.into()));
            this.delay.as_mut().unwrap()
        } else {
            return Poll::Pending;
        };

        ready!(delay.poll_unpin(cx));

        if let Some(next_tick) = (this.get_next_tick)(&t) {
            handle.next_tick.store(next_tick, Ordering::Relaxed);
            let next_tick = instant_from_unix(Duration::from_secs(next_tick));
            delay.reset(next_tick.into());
        } else {
            handle.next_tick.store(u64::MAX, Ordering::Relaxed);
            this.delay = None;
        }

        Poll::Pending
    }
}

impl Handle {
    pub fn new(first_tick: Option<u64>) -> Self {
        Handle {
            next_tick: AtomicU64::new(first_tick.unwrap_or(u64::MAX)),
            task: AtomicWaker::new(),
        }
    }

    /// Hastens the next tick of the associated `Scheduler` to the specified time.
    ///
    /// Does nothing if the current next tick is after the specified time.
    pub fn hasten(&self, next_tick: u64) {
        let prev = self.next_tick.fetch_min(next_tick, Ordering::Relaxed);
        if next_tick < prev {
            // Wake the associated scheduler task so that it can reset the `Delay`.
            self.task.wake();
        }
    }

    fn decode_next_tick(&self) -> Option<Instant> {
        let next_tick = self.next_tick.load(Ordering::Relaxed);
        if next_tick == u64::MAX {
            None
        } else {
            Some(instant_from_unix(Duration::from_secs(next_tick)))
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.task.wake();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use crate::util;

    use super::*;

    const PERIOD: Duration = Duration::from_secs(42);

    struct Handle {
        inner: super::Handle,
        count: AtomicU32,
    }

    impl Handle {
        fn new(first_tick: Option<u64>) -> Self {
            Handle {
                inner: super::Handle::new(first_tick),
                count: AtomicU32::new(0),
            }
        }

        fn hasten(&self, next_tick: u64) {
            self.inner.hasten(next_tick)
        }

        fn count(&self) -> u32 {
            self.count.load(Ordering::SeqCst)
        }

        fn incr_count(&self) -> u32 {
            self.count.fetch_add(1, Ordering::SeqCst) + 1
        }
    }

    impl AsRef<super::Handle> for Handle {
        fn as_ref(&self) -> &super::Handle {
            &self.inner
        }
    }

    #[tokio::test]
    async fn scheduler() {
        tokio::time::pause();

        let handle = Arc::new(Handle::new(Some(util::now_unix().as_secs() + 1)));
        let scheduler = Scheduler::new(&handle, move |handle| {
            let count = handle.incr_count();
            Some((util::now_unix() + count * PERIOD).as_secs())
        });
        let mut task = tokio_test::task::spawn(scheduler);

        task.enter(|cx, mut scheduler| {
            let _ = scheduler.as_mut().poll(cx);
            assert_eq!(handle.count(), 0);
        });
        assert!(!task.is_woken());

        tokio::time::advance(Duration::from_secs(1)).await;
        assert!(task.is_woken());
        task.enter(|cx, mut scheduler| {
            let _ = scheduler.as_mut().poll(cx);
            assert_eq!(handle.count(), 1);
            let _ = scheduler.poll(cx);
            assert_eq!(handle.count(), 1);
        });

        tokio::time::advance(PERIOD).await;
        assert!(task.is_woken());
        task.enter(|cx, mut scheduler| {
            let _ = scheduler.as_mut().poll(cx);
            assert_eq!(handle.count(), 2);
            let _ = scheduler.poll(cx);
            assert_eq!(handle.count(), 2);
        });

        tokio::time::advance(PERIOD).await;
        assert!(!task.is_woken());
        task.enter(|cx, scheduler| {
            let _ = scheduler.poll(cx);
            assert_eq!(handle.count(), 2);
        });

        tokio::time::advance(PERIOD).await;
        assert!(task.is_woken());
        task.enter(|cx, mut scheduler| {
            let _ = scheduler.as_mut().poll(cx);
            assert_eq!(handle.count(), 3);
            let _ = scheduler.poll(cx);
            assert_eq!(handle.count(), 3);
        });
    }

    #[tokio::test]
    async fn hasten() {
        tokio::time::pause();

        let handle = Arc::new(Handle::new(None));
        let scheduler = Scheduler::new(&handle, move |handle| {
            let count = handle.incr_count();
            Some((util::now_unix() + count * PERIOD).as_secs())
        });
        let mut task = tokio_test::task::spawn(scheduler);

        task.enter(|cx, mut scheduler| {
            let _ = scheduler.as_mut().poll(cx);
            assert_eq!(handle.count(), 0);
        });

        handle.hasten(util::now_unix().as_secs());
        // `handle.hasten` should wake the `task` to let it reset the inner `Delay`.
        assert!(task.is_woken());
        task.enter(|cx, mut scheduler| {
            let _ = scheduler.as_mut().poll(cx);
            assert_eq!(handle.count(), 1);
            let _ = scheduler.poll(cx);
            assert_eq!(handle.count(), 1);
        });

        tokio::time::advance(PERIOD / 2).await;
        assert!(!task.is_woken());
        task.enter(|cx, mut scheduler| {
            let _ = scheduler.as_mut().poll(cx);
            assert_eq!(handle.count(), 1);
        });

        handle.hasten(util::now_unix().as_secs());
        assert!(task.is_woken());
        task.enter(|cx, mut scheduler| {
            let _ = scheduler.as_mut().poll(cx);
            assert_eq!(handle.count(), 2);
            let _ = scheduler.poll(cx);
            assert_eq!(handle.count(), 2);
        });

        handle.hasten((util::now_unix() + PERIOD).as_secs());
        assert!(task.is_woken());
        tokio::time::advance(PERIOD).await;
        task.enter(|cx, mut scheduler| {
            let _ = scheduler.as_mut().poll(cx);
            assert_eq!(handle.count(), 3);
            let _ = scheduler.poll(cx);
            assert_eq!(handle.count(), 3);
        });
    }
}
