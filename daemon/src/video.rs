//! Phase 2 video plumbing that doesn't need a GPU: turn an encoder's Annex-B
//! byte stream (H.264 or HEVC) into protocol `VideoConfig` + `VideoFrame`
//! messages. Frames carry AVCC/HVCC samples (4-byte big-endian NAL lengths),
//! which iOS's VideoToolbox consumes directly; parameter sets travel in
//! `VideoConfig` and are re-sent whenever they change.

use sidecar_protocol::{VideoCodec, VideoConfig, VideoFrame};

/// Incremental Annex-B splitter: yields NAL units without start codes.
#[derive(Default)]
pub struct AnnexBSplitter {
    buf: Vec<u8>,
}

fn find_start_code(b: &[u8], from: usize) -> Option<(usize, usize)> {
    // Returns (position, start code length) of the next 00 00 01 / 00 00 00 01.
    let mut i = from;
    while i + 3 <= b.len() {
        if b[i] == 0 && b[i + 1] == 0 {
            if b[i + 2] == 1 {
                if i > 0 && b[i - 1] == 0 {
                    return Some((i - 1, 4));
                }
                return Some((i, 3));
            }
            if b[i + 2] == 0 {
                i += 1;
                continue;
            }
            i += 3;
            continue;
        }
        i += 1;
    }
    None
}

impl AnnexBSplitter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns every NAL unit that is complete (followed by the
    /// next start code).
    pub fn push(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let Some((mut start, mut sc_len)) = find_start_code(&self.buf, 0) else {
            return out;
        };
        loop {
            let body = start + sc_len;
            match find_start_code(&self.buf, body) {
                Some((next, next_len)) => {
                    let mut end = next;
                    // trailing_zero_8bits belong to no NAL
                    while end > body && self.buf[end - 1] == 0 {
                        end -= 1;
                    }
                    if end > body {
                        out.push(self.buf[body..end].to_vec());
                    }
                    start = next;
                    sc_len = next_len;
                }
                None => break,
            }
        }
        self.buf.drain(..start);
        out
    }

    /// End of stream: the last buffered NAL.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        let (start, sc_len) = find_start_code(&self.buf, 0)?;
        let body = start + sc_len;
        let mut end = self.buf.len();
        while end > body && self.buf[end - 1] == 0 {
            end -= 1;
        }
        let nal = (end > body).then(|| self.buf[body..end].to_vec());
        self.buf.clear();
        nal
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NalKind {
    Aud,
    ParamVps,
    ParamSps,
    ParamPps,
    /// VCL NAL; `first` = first slice of a picture, `key` = IDR/IRAP.
    Slice {
        first: bool,
        key: bool,
    },
    Other,
}

fn classify(codec: VideoCodec, nal: &[u8]) -> NalKind {
    if nal.is_empty() {
        return NalKind::Other;
    }
    match codec {
        VideoCodec::H264 => match nal[0] & 0x1f {
            9 => NalKind::Aud,
            7 => NalKind::ParamSps,
            8 => NalKind::ParamPps,
            t @ 1..=5 => NalKind::Slice {
                // first_mb_in_slice is ue(v): value 0 <=> first bit set.
                first: nal.get(1).is_some_and(|b| b & 0x80 != 0),
                key: t == 5,
            },
            _ => NalKind::Other,
        },
        VideoCodec::Hevc => {
            let t = (nal[0] >> 1) & 0x3f;
            match t {
                35 => NalKind::Aud,
                32 => NalKind::ParamVps,
                33 => NalKind::ParamSps,
                34 => NalKind::ParamPps,
                0..=31 => NalKind::Slice {
                    // first_slice_segment_in_pic_flag: first bit after the 2-byte header.
                    first: nal.get(2).is_some_and(|b| b & 0x80 != 0),
                    key: (16..=21).contains(&t),
                },
                _ => NalKind::Other,
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AccessUnit {
    pub keyframe: bool,
    /// AVCC/HVCC sample: NAL units with 4-byte big-endian lengths.
    pub sample: Vec<u8>,
}

/// Groups NAL units into access units and tracks parameter sets.
pub struct AuAssembler {
    codec: VideoCodec,
    vps: Option<Vec<u8>>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    sent_sets: Option<Vec<Vec<u8>>>,
    current: Vec<Vec<u8>>,
    current_key: bool,
    has_slice: bool,
}

/// What the assembler produced for one NAL.
#[derive(Debug, PartialEq)]
pub enum Output {
    /// Parameter sets changed: (re)send this before the next frame.
    Config(Vec<Vec<u8>>),
    Frame(AccessUnit),
}

impl AuAssembler {
    pub fn new(codec: VideoCodec) -> Self {
        AuAssembler {
            codec,
            vps: None,
            sps: None,
            pps: None,
            sent_sets: None,
            current: Vec::new(),
            current_key: false,
            has_slice: false,
        }
    }

    fn param_sets(&self) -> Option<Vec<Vec<u8>>> {
        match self.codec {
            VideoCodec::H264 => Some(vec![self.sps.clone()?, self.pps.clone()?]),
            VideoCodec::Hevc => Some(vec![
                self.vps.clone()?,
                self.sps.clone()?,
                self.pps.clone()?,
            ]),
        }
    }

    fn close_au(&mut self, out: &mut Vec<Output>) {
        if !self.has_slice {
            return;
        }
        let mut sample = Vec::new();
        for n in self.current.drain(..) {
            sample.extend_from_slice(&(n.len() as u32).to_be_bytes());
            sample.extend_from_slice(&n);
        }
        out.push(Output::Frame(AccessUnit {
            keyframe: self.current_key,
            sample,
        }));
        self.current_key = false;
        self.has_slice = false;
    }

    pub fn push(&mut self, nal: Vec<u8>) -> Vec<Output> {
        let mut out = Vec::new();
        match classify(self.codec, &nal) {
            NalKind::Aud => self.close_au(&mut out),
            NalKind::ParamVps | NalKind::ParamSps | NalKind::ParamPps => {
                // Parameter sets precede the picture they belong to.
                self.close_au(&mut out);
                match classify(self.codec, &nal) {
                    NalKind::ParamVps => self.vps = Some(nal),
                    NalKind::ParamSps => self.sps = Some(nal),
                    _ => self.pps = Some(nal),
                }
            }
            NalKind::Slice { first, key } => {
                if first {
                    self.close_au(&mut out);
                }
                if !self.has_slice {
                    // First slice of a picture: make sure the decoder has
                    // current parameter sets before it sees it.
                    if let Some(sets) = self.param_sets() {
                        if self.sent_sets.as_ref() != Some(&sets) {
                            self.sent_sets = Some(sets.clone());
                            out.push(Output::Config(sets));
                        }
                    }
                }
                self.has_slice = true;
                self.current_key |= key;
                self.current.push(nal);
            }
            NalKind::Other => {
                // SEI etc.: VideoToolbox doesn't need them; drop.
            }
        }
        out
    }

    pub fn finish(&mut self) -> Vec<Output> {
        let mut out = Vec::new();
        self.close_au(&mut out);
        out
    }
}

/// Converts a whole Annex-B buffer into protocol messages (tests, tools).
pub fn annexb_to_messages(
    codec: VideoCodec,
    width: u32,
    height: u32,
    fps_milli: u32,
    stream: &[u8],
) -> Vec<sidecar_protocol::Message> {
    use sidecar_protocol::Message;
    let mut split = AnnexBSplitter::new();
    let mut asm = AuAssembler::new(codec);
    let mut outs = Vec::new();
    for nal in split.push(stream) {
        outs.extend(asm.push(nal));
    }
    if let Some(n) = split.finish() {
        outs.extend(asm.push(n));
    }
    outs.extend(asm.finish());
    let frame_ns = 1_000_000_000_000u64 / fps_milli.max(1) as u64;
    let mut id = 0u32;
    outs.into_iter()
        .map(|o| match o {
            Output::Config(sets) => Message::VideoConfig(VideoConfig {
                codec,
                width,
                height,
                fps_milli,
                parameter_sets: sets,
            }),
            Output::Frame(au) => {
                id += 1;
                Message::VideoFrame(VideoFrame {
                    frame_id: id,
                    keyframe: au.keyframe,
                    capture_ns: id as u64 * frame_ns,
                    encoded_ns: id as u64 * frame_ns + 1_000_000,
                    data: au.sample,
                })
            }
        })
        .collect()
}

/// Reads GStreamer's GDP framing (`gdppay`): a 62-byte header (type u16 BE
/// at offset 4, payload length u32 BE at offset 6) before every buffer, caps
/// and event. The encoder pipelines end in `h264parse ... alignment=au !
/// gdppay`, so each buffer payload is exactly one access unit.
///
/// Without it the Annex-B splitter only knows a NAL ended when the next start
/// code arrives, so every frame waited for the next one: one frame interval
/// while drawing, and up to the 1 s keepalive once the screen stopped
/// changing. Found on the real machine as "latency issues".
#[derive(Debug, Default)]
pub struct GdpReader {
    buf: Vec<u8>,
}

pub const GDP_HEADER_LEN: usize = 62;
const GDP_PAYLOAD_BUFFER: u16 = 1;
/// A sanity bound: a 4K keyframe at high bitrate is well under this.
const GDP_MAX_PAYLOAD: usize = 64 << 20;

impl GdpReader {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed bytes; returns the payload of every complete *buffer* packet
    /// (caps and events are skipped).
    pub fn push(&mut self, data: &[u8]) -> anyhow::Result<Vec<Vec<u8>>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let mut pos = 0;
        while self.buf.len() - pos >= GDP_HEADER_LEN {
            let h = &self.buf[pos..pos + GDP_HEADER_LEN];
            if h[0] != 1 {
                anyhow::bail!(
                    "unexpected GDP version {}.{} (is gdppay in the pipeline?)",
                    h[0],
                    h[1]
                );
            }
            let ty = u16::from_be_bytes([h[4], h[5]]);
            let len = u32::from_be_bytes([h[6], h[7], h[8], h[9]]) as usize;
            if len > GDP_MAX_PAYLOAD {
                anyhow::bail!("GDP packet of {len} bytes, refusing");
            }
            if self.buf.len() - pos < GDP_HEADER_LEN + len {
                break;
            }
            let body = pos + GDP_HEADER_LEN;
            if ty == GDP_PAYLOAD_BUFFER {
                out.push(self.buf[body..body + len].to_vec());
            }
            pos = body + len;
        }
        self.buf.drain(..pos);
        Ok(out)
    }
}

/// One complete access unit in Annex-B (from a GDP buffer) → outputs, without
/// waiting for the next frame.
pub fn complete_au(split: &mut AnnexBSplitter, asm: &mut AuAssembler, au: &[u8]) -> Vec<Output> {
    let mut out = Vec::new();
    for nal in split.push(au) {
        out.extend(asm.push(nal));
    }
    if let Some(nal) = split.finish() {
        out.extend(asm.push(nal));
    }
    out.extend(asm.finish());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h264_stream() -> Vec<u8> {
        let mut s = Vec::new();
        let nal = |s: &mut Vec<u8>, b: &[u8]| {
            s.extend_from_slice(&[0, 0, 0, 1]);
            s.extend_from_slice(b);
        };
        nal(&mut s, &[0x09, 0x10]); // AUD
        nal(&mut s, &[0x67, 0x42, 0xc0, 0x15]); // SPS
        nal(&mut s, &[0x68, 0xce, 0x3c, 0x80]); // PPS
        nal(&mut s, &[0x06, 0x05, 0x01]); // SEI (dropped)
        nal(&mut s, &[0x65, 0x88, 0x01, 0x02]); // IDR slice, first_mb=0
        nal(&mut s, &[0x65, 0x40, 0x03]); // IDR slice, first_mb!=0 (same picture)
        s.extend_from_slice(&[0, 0, 1]); // 3-byte start code
        s.extend_from_slice(&[0x09, 0x30]); // AUD
        nal(&mut s, &[0x41, 0x9a, 0x04]); // P slice, first
        nal(&mut s, &[0x41, 0x9a, 0x05, 0x00, 0x00, 0x03, 0x01]); // P slice, first => new AU (no AUD)
        s
    }

    #[test]
    fn splitter_handles_any_chunking() {
        let s = h264_stream();
        for chunk in 1..s.len() {
            let mut sp = AnnexBSplitter::new();
            let mut nals = Vec::new();
            for c in s.chunks(chunk) {
                nals.extend(sp.push(c));
            }
            nals.extend(sp.finish());
            let types: Vec<u8> = nals.iter().map(|n| n[0] & 0x1f).collect();
            assert_eq!(types, vec![9, 7, 8, 6, 5, 5, 9, 1, 1], "chunk {chunk}");
            assert_eq!(
                nals.last().unwrap(),
                &vec![0x41, 0x9a, 0x05, 0x00, 0x00, 0x03, 0x01]
            );
        }
    }

    #[test]
    fn assembler_builds_avcc_access_units() {
        let msgs = annexb_to_messages(VideoCodec::H264, 16, 16, 30000, &h264_stream());
        use sidecar_protocol::Message;
        assert_eq!(msgs.len(), 4, "{msgs:?}");
        match &msgs[0] {
            Message::VideoConfig(c) => {
                assert_eq!(
                    c.parameter_sets,
                    vec![vec![0x67, 0x42, 0xc0, 0x15], vec![0x68, 0xce, 0x3c, 0x80]]
                );
            }
            m => panic!("{m:?}"),
        }
        match &msgs[1] {
            Message::VideoFrame(f) => {
                assert!(f.keyframe);
                // two slices, each 4-byte BE length-prefixed; SEI/AUD/SPS/PPS dropped
                assert_eq!(
                    f.data,
                    vec![0, 0, 0, 4, 0x65, 0x88, 0x01, 0x02, 0, 0, 0, 3, 0x65, 0x40, 0x03]
                );
            }
            m => panic!("{m:?}"),
        }
        match (&msgs[2], &msgs[3]) {
            (Message::VideoFrame(a), Message::VideoFrame(b)) => {
                assert!(!a.keyframe && !b.keyframe);
                assert_eq!(a.data, vec![0, 0, 0, 3, 0x41, 0x9a, 0x04]);
                assert_eq!(
                    b.data,
                    vec![0, 0, 0, 7, 0x41, 0x9a, 0x05, 0x00, 0x00, 0x03, 0x01]
                );
                assert_eq!((a.frame_id, b.frame_id), (2, 3));
            }
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn hevc_classification() {
        // VPS(32) SPS(33) PPS(34) IDR_W_RADL(19, first slice) TRAIL_R(1, first slice)
        let mut s = Vec::new();
        for n in [
            vec![0x40, 0x01, 0x0c],
            vec![0x42, 0x01, 0x01],
            vec![0x44, 0x01, 0xc0],
            vec![0x26, 0x01, 0xaf],
            vec![0x02, 0x01, 0xd0],
        ] {
            s.extend_from_slice(&[0, 0, 0, 1]);
            s.extend_from_slice(&n);
        }
        let msgs = annexb_to_messages(VideoCodec::Hevc, 16, 16, 60000, &s);
        use sidecar_protocol::Message;
        assert_eq!(msgs.len(), 3);
        assert!(matches!(&msgs[0], Message::VideoConfig(c) if c.parameter_sets.len() == 3));
        assert!(matches!(&msgs[1], Message::VideoFrame(f) if f.keyframe));
        assert!(matches!(&msgs[2], Message::VideoFrame(f) if !f.keyframe));
    }

    fn gdp_packet(ty: u16, payload: &[u8]) -> Vec<u8> {
        let mut h = vec![0u8; GDP_HEADER_LEN];
        h[0] = 1;
        h[4..6].copy_from_slice(&ty.to_be_bytes());
        h[6..10].copy_from_slice(&(payload.len() as u32).to_be_bytes());
        h.extend_from_slice(payload);
        h
    }

    #[test]
    fn gdp_reader_returns_buffers_skips_caps_and_events_any_chunking() {
        let mut stream = gdp_packet(2, b"video/x-h264\0");
        stream.extend(gdp_packet(64 + 10, b"event"));
        stream.extend(gdp_packet(1, b"AU-one"));
        stream.extend(gdp_packet(1, b""));
        stream.extend(gdp_packet(1, b"AU-three"));
        for chunk in [1, 7, 61, 62, 63, 1000] {
            let mut r = GdpReader::new();
            let mut got = Vec::new();
            for c in stream.chunks(chunk) {
                got.extend(r.push(c).unwrap());
            }
            assert_eq!(
                got,
                vec![b"AU-one".to_vec(), Vec::new(), b"AU-three".to_vec()],
                "chunk {chunk}"
            );
        }
    }

    #[test]
    fn gdp_reader_rejects_non_gdp_input() {
        let mut r = GdpReader::new();
        let annexb = [0u8, 0, 0, 1, 0x67, 0x42].repeat(20);
        assert!(r.push(&annexb).is_err());
    }

    #[test]
    fn complete_au_emits_the_frame_without_waiting_for_the_next() {
        // SPS, PPS, IDR slice (first_mb_in_slice = 0 -> ue(v) bit "1").
        let sps = [0x67u8, 0x42, 0x00, 0x1f, 0xe9];
        let pps = [0x68u8, 0xce, 0x38, 0x80];
        let idr = [0x65u8, 0x88, 0x84, 0x00, 0x33];
        let mut au = Vec::new();
        for n in [&sps[..], &pps[..], &idr[..]] {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(n);
        }
        let mut split = AnnexBSplitter::new();
        let mut asm = AuAssembler::new(VideoCodec::H264);
        let out = complete_au(&mut split, &mut asm, &au);
        let frames: Vec<_> = out
            .iter()
            .filter_map(|o| match o {
                Output::Frame(f) => Some(f),
                _ => None,
            })
            .collect();
        assert_eq!(frames.len(), 1, "{out:?}");
        assert!(frames[0].keyframe);
        let mut want = (idr.len() as u32).to_be_bytes().to_vec();
        want.extend_from_slice(&idr);
        assert_eq!(frames[0].sample, want);
        assert!(matches!(out[0], Output::Config(_)));
    }
}
