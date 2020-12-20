use std::convert::TryInto;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::{Buf, BufMut, Bytes};
use futures::{ready, Future};
use http_body::Body;
use pin_project::pin_project;

#[pin_project]
pub struct ConcatBody<B> {
    #[pin]
    body: B,
    state: State,
}

enum State {
    Init,
    Once(Bytes),
    Streaming(Vec<u8>),
}

impl<B: Body> ConcatBody<B> {
    pub fn new(body: B) -> Self {
        ConcatBody {
            body,
            state: State::Init,
        }
    }
}

impl<B: Body> Future for ConcatBody<B> {
    type Output = Result<Bytes, B::Error>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut this = self.project();

        while let Some(mut data) = ready!(this.body.as_mut().poll_data(cx)?) {
            match this.state {
                State::Init => *this.state = State::Once(data.to_bytes()),
                State::Once(first) => {
                    let cap = first.remaining()
                        + data.remaining()
                        + this.body.size_hint().lower().try_into().unwrap_or(0);
                    let mut buf = Vec::with_capacity(cap);
                    buf.put(first);
                    buf.put(data);
                    *this.state = State::Streaming(buf);
                }
                State::Streaming(ref mut buf) => buf.put(data),
            }
        }

        match mem::replace(this.state, State::Init) {
            State::Init => Poll::Ready(Ok(Bytes::new())),
            State::Once(buf) => Poll::Ready(Ok(buf)),
            State::Streaming(buf) => Poll::Ready(Ok(buf.into())),
        }
    }
}
