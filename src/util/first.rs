use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::future;
use pin_project::pin_project;

#[pin_project]
pub struct First<A, B> {
    #[pin]
    a: A,
    #[pin]
    b: B,
}

impl<A: Future, B: Future> Future for First<A, B> {
    type Output = future::Either<A::Output, B::Output>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();
        match this.a.poll(cx) {
            Poll::Ready(x) => Poll::Ready(future::Either::Left(x)),
            Poll::Pending => match this.b.poll(cx) {
                Poll::Ready(x) => Poll::Ready(future::Either::Right(x)),
                Poll::Pending => Poll::Pending,
            },
        }
    }
}

/// Like `futures::future::select`, but accepts `!Unpin` futures and resolves to
/// `Either<A::Output, B::Output>` instead of `Either<(_, _), (_, _)>`.
pub fn first<A, B>(a: A, b: B) -> First<A, B> {
    First { a, b }
}
