use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use futures::task::AtomicWaker;
use pin_project::pin_project;

/// A future-like object that waits until all of its handles get dropped.
#[derive(Default)]
pub struct Shutdown {
    handle: Handle,
}

#[derive(Clone, Default)]
pub struct Handle {
    inner: Arc<Inner>,
}

#[pin_project]
pub struct HandleFuture<F> {
    handle: Handle,
    #[pin]
    future: F,
}

#[derive(Default)]
struct Inner {
    waker: AtomicWaker,
}

impl Shutdown {
    pub fn poll(&self, cx: &mut Context<'_>) -> Poll<()> {
        if Arc::strong_count(&self.handle.inner) == 1 {
            Poll::Ready(())
        } else {
            self.handle.inner.waker.register(cx.waker());
            Poll::Pending
        }
    }

    pub fn handle(&self) -> &Handle {
        &self.handle
    }
}

impl Future for Shutdown {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        (*self).poll(cx)
    }
}

impl Handle {
    /// Associate the handle with a future so that the handle will be dropped
    /// when the future completes.
    pub fn wrap_future<F: Future>(self, future: F) -> HandleFuture<F> {
        HandleFuture {
            handle: self,
            future,
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 2 {
            self.inner.waker.wake()
        }
    }
}

impl<F: Future> Future for HandleFuture<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        self.project().future.poll(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test() {
        let shutdown = Shutdown::default();
        let handle = shutdown.handle.clone();
        let mut task = tokio_test::task::spawn(shutdown);

        task.enter(|cx, shutdown| assert!(shutdown.poll(cx).is_pending()));

        let cloned = handle.clone();
        drop(handle);
        task.enter(|cx, shutdown| assert!(shutdown.poll(cx).is_pending()));

        drop(cloned);
        task.enter(|cx, shutdown| assert!(shutdown.poll(cx).is_ready()));
    }
}
