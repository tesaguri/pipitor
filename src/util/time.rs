use std::time::{Duration, Instant, SystemTime};

use cfg_if::cfg_if;

cfg_if! {
    if #[cfg(test)] {
        use std::future::Future;
        use std::pin::Pin;
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::task::{Context, Poll};
        use std::{env, thread};

        use futures::ready;
        use futures::task::AtomicWaker;
        use pin_project::pin_project;

        /// A sleep `Future` that just works even when `tokio::time::pause()` has been called.
        pub struct Sleep {
            inner: Arc<SleepInner>,
        }

        struct SleepInner {
            elapsed: AtomicBool,
            waker: AtomicWaker,
        }

        impl Sleep {
            pub fn new(duration: Duration) -> Self {
                let inner = Arc::new(SleepInner {
                    elapsed: AtomicBool::new(false),
                    waker: AtomicWaker::new(),
                });
                let inner2 = Arc::clone(&inner);
                thread::spawn(move || {
                    thread::sleep(duration);
                    inner2.elapsed.store(true, Ordering::SeqCst);
                    inner2.waker.wake();
                });
                Sleep { inner }
            }
        }

        impl Future for Sleep {
            type Output = ();

            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
                if self.inner.elapsed.load(Ordering::SeqCst) {
                    Poll::Ready(())
                } else {
                    self.inner.waker.register(cx.waker());
                    Poll::Pending
                }
            }
        }

        #[pin_project]
        pub struct Timeout<F> {
            #[pin]
            future: F,
            #[pin]
            sleep: Sleep,
        }

        pub trait FutureTimeoutExt: Future + Sized {
            /// Make the `Future` panic after a timeout.
            fn timeout(self) -> Timeout<Self> {
                let duration = env::var("PIPITOR_TEST_TIMEOUT")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .map(Duration::from_millis)
                    .unwrap_or_else(|| Duration::from_millis(500));
                Timeout {
                    future: self,
                    sleep: Sleep::new(duration),
                }
            }
        }

        impl<F: Future> FutureTimeoutExt for F {}

        impl<F: Future> Future for Timeout<F> {
            type Output = F::Output;

            #[track_caller]
            fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
                let this = self.project();
                if let Poll::Ready(x) = this.future.poll(cx) {
                    return Poll::Ready(x);
                }
                ready!(this.sleep.poll(cx));
                panic!("`Future` timed out");
            }
        }

        pub fn instant_now() -> Instant {
            tokio::time::Instant::now().into()
        }

        pub fn system_time_now() -> SystemTime {
            let instant = Instant::now();
            let sys = SystemTime::now();
            let mocked = instant_now();
            sys + if mocked > instant {
                mocked - instant
            } else {
                instant - mocked
            }
        }
    } else {
        pub fn instant_now() -> Instant {
            Instant::now()
        }

        pub fn system_time_now() -> SystemTime {
            SystemTime::now()
        }
    }
}

/// Converts a `Duration` representing a Unix time to an `Instant`.
pub fn instant_from_unix(unix: Duration) -> Instant {
    let now_i = instant_now();
    let now_unix = now_unix();
    // Do not add the `Duration`s directly to the `Instant` to mitigate the risk of overflowing.
    if now_unix < unix {
        now_i + (unix - now_unix)
    } else {
        now_i - (now_unix - unix)
    }
}

/// Returns the Unix time representation of "now" as a `Duration`.
pub fn now_unix() -> Duration {
    system_time_now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
}

#[cfg(test)]
mod tests {
    use std::cmp::PartialOrd;
    use std::ops::Sub;

    use super::*;

    const EPSILON: Duration = Duration::from_millis(10);

    #[tokio::test]
    async fn advance() {
        let delta = Duration::from_secs(42);

        tokio::time::pause();
        let start = system_time_now();
        tokio::time::advance(delta).await;
        let end = system_time_now();

        assert!(diff_abs(end.duration_since(start).unwrap(), delta) < EPSILON);
    }

    fn diff_abs<T: PartialOrd + Sub<Output = T>>(x: T, y: T) -> T {
        if x > y {
            x - y
        } else {
            y - x
        }
    }
}
