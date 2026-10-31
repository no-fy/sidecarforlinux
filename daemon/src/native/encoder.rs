//! VA-API colour conversion + H.264 encoder (`[video] backend = "native"`).
//! Placeholder until the next commit; `backend = "hybrid"` works meanwhile.

use super::frame::{Converted, Encoded, Frame, FrameSink};
use anyhow::{bail, Result};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct VaOptions {
    pub device: Option<PathBuf>,
    pub low_power: bool,
    pub kbps: u32,
    pub fps: u32,
    pub target_usage: u8,
}

pub struct VaEncoder;

impl VaEncoder {
    pub fn open(_: VaOptions) -> Result<VaEncoder> {
        bail!("the in-daemon VA-API encoder is not in this build yet; use backend = \"hybrid\"")
    }
}

impl FrameSink for VaEncoder {
    fn dmabuf_formats(&self) -> Vec<(u32, Vec<u64>)> {
        Vec::new()
    }
    fn convert(&mut self, _: &Frame) -> Result<Converted> {
        bail!("unreachable")
    }
    fn encode(&mut self, _: bool) -> Result<Option<Encoded>> {
        Ok(None)
    }
}
