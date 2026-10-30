//! Framed message I/O over any async byte stream (TLS over usbmux/TCP).

use anyhow::{anyhow, Result};
use sidecar_protocol::{FrameDecoder, Message};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub struct MsgReader<R> {
    inner: R,
    dec: FrameDecoder,
    buf: Vec<u8>,
}

impl<R: AsyncRead + Unpin> MsgReader<R> {
    pub fn new(inner: R) -> Self {
        MsgReader {
            inner,
            dec: FrameDecoder::new(),
            buf: vec![0u8; 64 * 1024],
        }
    }

    /// Next message; `Ok(None)` on clean EOF.
    pub async fn next(&mut self) -> Result<Option<Message>> {
        loop {
            if let Some((ty, payload)) = self.dec.next_frame()? {
                return Ok(Some(Message::decode(ty, &payload)?));
            }
            let n = self.inner.read(&mut self.buf).await?;
            if n == 0 {
                if self.dec.buffered() > 0 {
                    return Err(anyhow!("connection closed mid-frame"));
                }
                return Ok(None);
            }
            self.dec.push(&self.buf[..n]);
        }
    }

    /// Next message or error on EOF.
    pub async fn expect(&mut self) -> Result<Message> {
        self.next()
            .await?
            .ok_or_else(|| anyhow!("peer closed the connection"))
    }
}

pub struct MsgWriter<W> {
    inner: W,
    buf: Vec<u8>,
}

impl<W: AsyncWrite + Unpin> MsgWriter<W> {
    pub fn new(inner: W) -> Self {
        MsgWriter {
            inner,
            buf: Vec::with_capacity(4096),
        }
    }

    pub async fn send(&mut self, m: &Message) -> Result<()> {
        self.buf.clear();
        m.encode(&mut self.buf);
        self.inner.write_all(&self.buf).await?;
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn shutdown(&mut self) {
        let _ = self.inner.shutdown().await;
    }
}
