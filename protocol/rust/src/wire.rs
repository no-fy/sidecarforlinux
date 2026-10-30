//! Little-endian reader/writer helpers.

use crate::ProtocolError;

pub(crate) struct Writer<'a> {
    pub buf: &'a mut Vec<u8>,
}

impl<'a> Writer<'a> {
    pub fn new(buf: &'a mut Vec<u8>) -> Self {
        Self { buf }
    }
    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    pub fn bytes(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    /// u16 length prefix + UTF-8 bytes (truncated to 65535 bytes on a char boundary).
    pub fn string(&mut self, s: &str) {
        let mut end = s.len().min(u16::MAX as usize);
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        self.u16(end as u16);
        self.bytes(&s.as_bytes()[..end]);
    }
    /// u16 length prefix + raw bytes.
    pub fn blob16(&mut self, b: &[u8]) {
        let len = b.len().min(u16::MAX as usize);
        self.u16(len as u16);
        self.bytes(&b[..len]);
    }
}

pub(crate) struct Reader<'a> {
    pub ty: u8,
    pub data: &'a [u8],
    pub pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(ty: u8, data: &'a [u8]) -> Self {
        Self { ty, data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        if self.data.len() - self.pos < n {
            return Err(ProtocolError::Truncated {
                ty: self.ty,
                need: self.pos + n,
                have: self.data.len(),
            });
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn u8(&mut self) -> Result<u8, ProtocolError> {
        Ok(self.take(1)?[0])
    }
    pub fn u16(&mut self) -> Result<u16, ProtocolError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    pub fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    pub fn f32(&mut self) -> Result<f32, ProtocolError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        Ok(self.take(N)?.try_into().unwrap())
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        self.take(n)
    }
    pub fn string(&mut self) -> Result<String, ProtocolError> {
        let len = self.u16()? as usize;
        let b = self.take(len)?;
        String::from_utf8(b.to_vec()).map_err(|_| ProtocolError::BadString)
    }
    pub fn blob16(&mut self) -> Result<Vec<u8>, ProtocolError> {
        let len = self.u16()? as usize;
        Ok(self.take(len)?.to_vec())
    }
    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.data[self.pos..];
        self.pos = self.data.len();
        s
    }
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }
}
