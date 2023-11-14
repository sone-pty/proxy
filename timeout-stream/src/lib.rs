use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};

use pin_project_lite::pin_project;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::{sleep_until, Instant, Sleep},
};

pin_project! {
    #[derive(Debug)]
    struct TimeoutState {
        timeout: Option<Duration>,
        #[pin]
        cur: Sleep,
        active: bool,
    }
}

impl TimeoutState {
    #[inline]
    fn new() -> TimeoutState {
        TimeoutState {
            timeout: None,
            cur: sleep_until(Instant::now()),
            active: false,
        }
    }

    #[inline]
    fn timeout(&self) -> Option<Duration> {
        self.timeout
    }

    #[inline]
    fn set_timeout(&mut self, timeout: Option<Duration>) {
        // since this takes &mut self, we can't yet be active
        self.timeout = timeout;
    }

    #[inline]
    fn set_timeout_pinned(mut self: Pin<&mut Self>, timeout: Option<Duration>) {
        *self.as_mut().project().timeout = timeout;
        self.reset();
    }

    #[inline]
    fn reset(self: Pin<&mut Self>) {
        let this = self.project();

        if *this.active {
            *this.active = false;
            this.cur.reset(Instant::now());
        }
    }

    #[inline]
    fn poll_check(self: Pin<&mut Self>, cx: &mut Context<'_>) -> std::io::Result<()> {
        let mut this = self.project();

        let timeout = match this.timeout {
            Some(timeout) => *timeout,
            None => return Ok(()),
        };

        if !*this.active {
            this.cur.as_mut().reset(Instant::now() + timeout);
            *this.active = true;
        }

        match this.cur.poll(cx) {
            Poll::Ready(()) => Err(std::io::Error::from(std::io::ErrorKind::TimedOut)),
            Poll::Pending => Ok(()),
        }
    }
}

pin_project! {
    pub struct TimeoutStream<S> {
        #[pin]
        stream: S,
        state: TimeoutState,
    }
}

impl<S> TimeoutStream<S>
where
    S: AsyncRead + AsyncWrite,
{
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            state: TimeoutState::new(),
        }
    }
}
