//! Length framing: `length:u32le type:u8 payload`.

use crate::ProtocolError;

/// Upper bound for a single frame (type + payload). Video keyframes of a
/// 2732x2048 HEVC stream stay well below this.
pub const MAX_FRAME_LEN: usize = 16 * 1024 * 1024;

/// Append one frame (header + payload) to `out`.
pub fn encode_frame(ty: u8, payload: &[u8], out: &mut Vec<u8>) {
    let len = payload.len() + 1;
    assert!(len <= MAX_FRAME_LEN, "frame too large");
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.push(ty);
    out.extend_from_slice(payload);
}

/// Incremental decoder: feed bytes as they arrive, pull complete frames.
#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    start: usize,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, bytes: &[u8]) {
        // Compact lazily so a long-lived connection doesn't grow forever.
        if self.start > 0 && self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start > 64 * 1024 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete frame as `(type, payload)`.
    pub fn next_frame(&mut self) -> Result<Option<(u8, Vec<u8>)>, ProtocolError> {
        let avail = &self.buf[self.start..];
        if avail.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes(avail[..4].try_into().unwrap()) as usize;
        if len == 0 {
            return Err(ProtocolError::EmptyFrame);
        }
        if len > MAX_FRAME_LEN {
            return Err(ProtocolError::FrameTooLarge(len));
        }
        if avail.len() < 4 + len {
            return Ok(None);
        }
        let ty = avail[4];
        let payload = avail[5..4 + len].to_vec();
        self.start += 4 + len;
        Ok(Some((ty, payload)))
    }

    /// Bytes buffered but not yet returned as frames.
    pub fn buffered(&self) -> usize {
        self.buf.len() - self.start
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_split_arbitrarily() {
        let mut stream = Vec::new();
        encode_frame(0x10, b"hello", &mut stream);
        encode_frame(0x11, b"", &mut stream);
        encode_frame(0x12, &[7u8; 300], &mut stream);
        for chunk in 1..stream.len() {
            let mut d = FrameDecoder::new();
            let mut got = Vec::new();
            for piece in stream.chunks(chunk) {
                d.push(piece);
                while let Some(f) = d.next_frame().unwrap() {
                    got.push(f);
                }
            }
            assert_eq!(got.len(), 3, "chunk size {chunk}");
            assert_eq!(got[0], (0x10, b"hello".to_vec()));
            assert_eq!(got[1], (0x11, vec![]));
            assert_eq!(got[2].1.len(), 300);
            assert_eq!(d.buffered(), 0);
        }
    }

    #[test]
    fn rejects_oversized_and_empty() {
        let mut d = FrameDecoder::new();
        d.push(&((MAX_FRAME_LEN as u32) + 1).to_le_bytes());
        assert!(matches!(
            d.next_frame(),
            Err(ProtocolError::FrameTooLarge(_))
        ));
        let mut d = FrameDecoder::new();
        d.push(&0u32.to_le_bytes());
        assert_eq!(d.next_frame(), Err(ProtocolError::EmptyFrame));
    }
}
