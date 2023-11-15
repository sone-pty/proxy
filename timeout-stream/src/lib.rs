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

#[allow(dead_code)]
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
        #[pin]
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

    pub fn set_timeout(&mut self, timeout: Option<Duration>) {
        self.state.set_timeout(timeout);
    }
}

impl<S> AsyncRead for TimeoutStream<S>
where
    S: AsyncRead,
{
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.project();
        let r = this.stream.poll_read(cx, buf);
        match r {
            Poll::Pending => this.state.poll_check(cx)?,
            _ => this.state.reset(),
        }
        r
    }
}

impl<S> AsyncWrite for TimeoutStream<S>
where
    S: AsyncWrite,
{
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        let this = self.project();
        let r = this.stream.poll_flush(cx);
        match r {
            Poll::Pending => this.state.poll_check(cx)?,
            _ => this.state.reset(),
        }
        r
    }

    fn is_write_vectored(&self) -> bool {
        self.stream.is_write_vectored()
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        let this = self.project();
        let r = this.stream.poll_shutdown(cx);
        match r {
            Poll::Pending => this.state.poll_check(cx)?,
            _ => this.state.reset(),
        }
        r
    }

    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        let this = self.project();
        let r = this.stream.poll_write(cx, buf);
        match r {
            Poll::Pending => this.state.poll_check(cx)?,
            _ => this.state.reset(),
        }
        r
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<Result<usize, std::io::Error>> {
        let this = self.project();
        let r = this.stream.poll_write_vectored(cx, bufs);
        match r {
            Poll::Pending => this.state.poll_check(cx)?,
            _ => this.state.reset(),
        }
        r
    }
}
