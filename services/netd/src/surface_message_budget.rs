//! Assembly deadline below tungstenite, which does not yield partial messages.
//! Opt-in only. Idle sockets are allowed; a Ping cannot extend a partial message.
use std::{
    future::Future,
    io,
    pin::Pin,
    task::{Context, Poll},
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    time::{Instant, Sleep},
};
const MESSAGE_LIMIT: u64 = 1024 * 1024;
const ASSEMBLY_LIMIT: Duration = Duration::from_secs(5);
#[derive(Default)]
struct Frames {
    header: [u8; 14],
    used: usize,
    target: usize,
    remaining: u64,
    fragmented: bool,
    fin: bool,
    opcode: u8,
    message_bytes: u64,
    started: Option<Instant>,
}
impl Frames {
    fn finish(&mut self) {
        if self.opcode <= 2 && self.fin {
            self.fragmented = false;
            self.message_bytes = 0;
        }
        if !self.fragmented {
            self.started = None;
        }
        self.used = 0;
        self.target = 2;
    }
    fn observe(&mut self, mut bytes: &[u8], now: Instant) -> io::Result<()> {
        while !bytes.is_empty() {
            if self.remaining > 0 {
                let take = bytes.len().min(self.remaining as usize);
                self.remaining -= take as u64;
                bytes = &bytes[take..];
                if self.remaining == 0 {
                    self.finish();
                }
                continue;
            }
            if self.used == 0 {
                self.target = 2;
                self.started.get_or_insert(now);
            }
            let take = (self.target - self.used).min(bytes.len());
            self.header[self.used..self.used + take].copy_from_slice(&bytes[..take]);
            self.used += take;
            bytes = &bytes[take..];
            if self.used < self.target {
                continue;
            }
            if self.used == 2 {
                self.fin = self.header[0] & 128 != 0;
                self.opcode = self.header[0] & 15;
                if self.header[0] & 112 != 0
                    || self.header[1] & 128 == 0
                    || ![0, 1, 2, 8, 9, 10].contains(&self.opcode)
                {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                let length = self.header[1] & 127;
                if self.opcode >= 8 && (!self.fin || length > 125) {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                self.target =
                    2 + match length {
                        126 => 2,
                        127 => 8,
                        _ => 0,
                    } + 4;
                continue;
            }
            let length = match self.header[1] & 127 {
                126 => u16::from_be_bytes(self.header[2..4].try_into().unwrap()) as u64,
                127 => u64::from_be_bytes(self.header[2..10].try_into().unwrap()),
                n => n as u64,
            };
            if length > MESSAGE_LIMIT {
                return Err(io::ErrorKind::InvalidData.into());
            }
            match self.opcode {
                0 if !self.fragmented => return Err(io::ErrorKind::InvalidData.into()),
                0 => {
                    self.message_bytes = self
                        .message_bytes
                        .checked_add(length)
                        .ok_or(io::ErrorKind::InvalidData)?;
                }
                1 | 2 if self.fragmented => return Err(io::ErrorKind::InvalidData.into()),
                1 | 2 => {
                    self.message_bytes = length;
                    self.fragmented = !self.fin;
                }
                _ => {}
            }
            if self.message_bytes > MESSAGE_LIMIT {
                return Err(io::ErrorKind::InvalidData.into());
            }
            self.remaining = length;
            if length == 0 {
                self.finish();
            }
        }
        Ok(())
    }
}

pub(crate) struct BoundedSocket<S> {
    inner: S,
    frames: Option<Frames>,
    deadline: Option<Pin<Box<Sleep>>>,
    http_pending: bool,
    http_bytes: usize,
    http_tail: u32,
    http_started: Option<Instant>,
}
impl<S> BoundedSocket<S> {
    pub(crate) fn new(inner: S, enabled: bool) -> Self {
        Self {
            inner,
            frames: enabled.then(Frames::default),
            deadline: None,
            http_pending: enabled,
            http_bytes: 0,
            http_tail: 0,
            http_started: None,
        }
    }
    #[cfg(test)]
    fn start_frames(&mut self) {
        self.http_pending = false;
    }
    fn observe(&mut self, mut bytes: &[u8], now: Instant) -> io::Result<()> {
        if self.http_pending {
            while let Some(byte) = bytes.first() {
                self.http_started.get_or_insert(now);
                self.http_bytes += 1;
                if self.http_bytes > 65536 {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                self.http_tail = (self.http_tail << 8) | u32::from(*byte);
                bytes = &bytes[1..];
                if self.http_tail == 0x0d0a0d0a {
                    self.http_pending = false;
                    self.http_started = None;
                    break;
                }
            }
        }
        if !self.http_pending
            && let Some(frames) = &mut self.frames
        {
            frames.observe(bytes, now)?;
        }
        Ok(())
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for BoundedSocket<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if let Some(deadline) = &mut this.deadline
            && deadline.as_mut().poll(cx).is_ready()
        {
            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
        }
        let before = buf.filled().len();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            Poll::Ready(Ok(())) => {
                if let Err(error) = this.observe(&buf.filled()[before..], Instant::now()) {
                    return Poll::Ready(Err(error));
                }
                let started = this
                    .http_started
                    .or_else(|| this.frames.as_ref().and_then(|frames| frames.started));
                match started {
                    Some(started) => {
                        let end = started + ASSEMBLY_LIMIT;
                        if this
                            .deadline
                            .as_ref()
                            .is_none_or(|timer| timer.deadline() != end)
                        {
                            this.deadline = Some(Box::pin(tokio::time::sleep_until(end)));
                        }
                        if this.deadline.as_mut().unwrap().as_mut().poll(cx).is_ready() {
                            return Poll::Ready(Err(io::ErrorKind::TimedOut.into()));
                        }
                    }
                    None => this.deadline = None,
                }
                Poll::Ready(Ok(()))
            }
            other => other,
        }
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for BoundedSocket<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, bytes)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_message_budget_survives_interleaved_ping() {
        let mut frames = Frames::default();
        let now = Instant::now();
        frames.observe(&[2, 129, 0, 0, 0, 0, b'a'], now).unwrap();
        assert_eq!(frames.started, Some(now));
        frames
            .observe(&[137, 128, 0, 0, 0, 0], now + Duration::from_secs(4))
            .unwrap();
        assert_eq!(frames.started, Some(now));
        frames
            .observe(&[128, 129, 0, 0, 0, 0, b'b'], now + Duration::from_secs(4))
            .unwrap();
        assert_eq!(frames.started, None);
    }
    #[test]
    fn partial_header_and_payload_are_bounded_without_copying_body() {
        let mut frames = Frames::default();
        let now = Instant::now();
        frames.observe(&[130], now).unwrap();
        assert_eq!(frames.started, Some(now));
        frames.observe(&[129, 0, 0, 0, 0], now).unwrap();
        assert_eq!(frames.started, Some(now));
        frames.observe(b"a", now).unwrap();
        assert_eq!(frames.started, None);
        assert!(
            frames
                .observe(&[130, 255, 0, 0, 0, 0, 0, 16, 0, 1, 0, 0, 0, 0], now)
                .is_err()
        );
    }
    #[tokio::test]
    async fn reader_deadline_wakes_even_when_websocket_message_is_incomplete() {
        use tokio::io::AsyncWriteExt;
        let (mut writer, reader) = tokio::io::duplex(64);
        let mut bounded = BoundedSocket::new(reader, true);
        bounded.start_frames();
        writer.write_all(&[2, 129, 0, 0, 0, 0, b'a']).await.unwrap();
        let mut ws = tokio_tungstenite::WebSocketStream::from_raw_socket(
            bounded,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;
        use futures_util::StreamExt;
        let result = tokio::time::timeout(Duration::from_secs(6), ws.next())
            .await
            .expect("fixed frame deadline must wake");
        assert!(result.unwrap().is_err());
    }
    #[tokio::test]
    async fn upgrade_read_ahead_keeps_initial_fragment_deadline() {
        let (mut writer, reader) = tokio::io::duplex(256);
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut bounded = BoundedSocket::new(reader, true);
        writer
            .write_all(b"GET / HTTP/1.1\r\n\r\n\x02\x81\0\0\0\0a")
            .await
            .unwrap();
        let mut bytes = [0; 128];
        assert!(bounded.read(&mut bytes).await.unwrap() > 0);
        assert!(!bounded.http_pending);
        assert!(bounded.frames.as_ref().unwrap().fragmented);
        assert!(bounded.deadline.is_some());
        writer.write_all(&[137, 128, 0, 0, 0, 0]).await.unwrap();
        let end = bounded.deadline.as_ref().unwrap().deadline();
        assert!(bounded.read(&mut bytes).await.unwrap() > 0);
        assert_eq!(bounded.deadline.as_ref().unwrap().deadline(), end);
    }
}
