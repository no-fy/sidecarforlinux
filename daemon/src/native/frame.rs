//! The interface between a capture source and the encoder, independent of
//! where the pixels come from. Today the source is KWin through PipeWire
//! (`pw.rs`); a wlroots compositor's `wlr_export_dmabuf` / `ext-image-copy-
//! capture` frames (Wayfire) would be a second source feeding the same sink.
//!
//! Pixel formats are DRM fourccs (what DMA-BUF APIs use); the PipeWire
//! source maps SPA video formats at its edge.

use super::stats::StageTimes;
use std::os::fd::RawFd;

pub const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    a as u32 | (b as u32) << 8 | (c as u32) << 16 | (d as u32) << 24
}

/// DRM fourccs of the 32-bit RGB layouts compositors hand out. Names are
/// little-endian words: XRGB8888 is B, G, R, X in memory (SPA "BGRx").
pub const DRM_XRGB8888: u32 = fourcc(b'X', b'R', b'2', b'4');
pub const DRM_ARGB8888: u32 = fourcc(b'A', b'R', b'2', b'4');
pub const DRM_XBGR8888: u32 = fourcc(b'X', b'B', b'2', b'4');
pub const DRM_ABGR8888: u32 = fourcc(b'A', b'B', b'2', b'4');

pub const DRM_FORMAT_MOD_LINEAR: u64 = 0;
pub const DRM_FORMAT_MOD_INVALID: u64 = 0x00ff_ffff_ffff_ffff;

pub fn fourcc_name(f: u32) -> String {
    f.to_le_bytes().iter().map(|&b| b as char).collect()
}

/// One plane of a DMA-BUF. The fd stays owned by the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Plane {
    pub fd: RawFd,
    pub offset: u32,
    pub pitch: u32,
}

#[derive(Debug)]
pub enum Pixels<'a> {
    DmaBuf {
        fourcc: u32,
        modifier: u64,
        planes: &'a [Plane],
    },
    Shm {
        fourcc: u32,
        data: &'a [u8],
        stride: u32,
    },
}

#[derive(Debug)]
pub struct Frame<'a> {
    pub width: u32,
    pub height: u32,
    /// The rate the compositor will send at most (0 = unknown): the
    /// encoder's level and rate-control frame budget.
    pub max_fps: u32,
    pub pixels: Pixels<'a>,
    /// Filled in by the source: tick, arrival, stale buffers skipped.
    pub times: StageTimes,
}

/// What `FrameSink::push` did with a frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Converted {
    /// The sink has read the pixels (into its own picture); the source may
    /// release the buffer. The encoded frame follows on the sink's output.
    Done,
    /// Not usable (e.g. a format the sink can't take); nothing to encode.
    Skipped(String),
    /// The DMA-BUF couldn't be imported: the source should stop offering
    /// this format/modifier pair and renegotiate.
    ImportFailed { fourcc: u32, modifier: u64 },
}

/// An encoded access unit, Annex-B, with its stage times.
#[derive(Debug)]
pub struct Encoded {
    pub annexb: Vec<u8>,
    /// The picture size that was encoded (the iPad gets it in VideoConfig).
    pub width: u32,
    pub height: u32,
    pub times: StageTimes,
}

/// An encoder fed by a capture source. Sinks deliver encoded frames on the
/// output channel they were made with (`pw::Event::Frame`), possibly from
/// another thread: the VA-API encoder has two frames in flight, so frame
/// N+1's colour conversion overlaps frame N's encode.
pub trait FrameSink {
    /// DRM fourcc → modifiers this sink can import from DMA-BUFs (empty:
    /// SHM only). The source offers these to the compositor.
    fn dmabuf_formats(&self) -> Vec<(u32, Vec<u64>)>;

    /// The source renegotiated: its buffers are new, forget cached imports.
    fn buffers_changed(&mut self) {}

    /// Block until the sink can take another frame. The source calls this
    /// before picking the newest buffer, so a busy encoder starts on the
    /// newest picture rather than one that waited for it.
    fn wait_ready(&mut self) -> anyhow::Result<()> {
        Ok(())
    }

    /// Take a new picture and start encoding it (an IDR if `force_idr`).
    /// When it returns the sink has read the source's pixels and the source
    /// may release its buffer.
    fn push(&mut self, frame: &Frame, force_idr: bool) -> anyhow::Result<Converted>;

    /// Encode the last picture again (`times.repeat`): a requested keyframe
    /// while the screen is idle, or the keep-alive. False if there is no
    /// picture yet or the sink can't (hybrid).
    fn repeat(&mut self, force_idr: bool) -> anyhow::Result<bool>;
}

/// Picks the newest usable item from a queue, handing the others back
/// (dropping them) as it goes. Returns it and how many usable but older
/// items were skipped.
pub fn newest_usable<T>(
    items: impl Iterator<Item = T>,
    mut usable: impl FnMut(&mut T) -> bool,
) -> (Option<T>, u32) {
    let mut best = None;
    let mut skipped = 0;
    for mut it in items {
        if usable(&mut it) && best.replace(it).is_some() {
            skipped += 1;
        }
    }
    (best, skipped)
}

/// DMA-BUF formats and modifiers still worth offering: starts with what the
/// sink can import, loses a pair whenever an import fails.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModifierBook(Vec<(u32, Vec<u64>)>);

impl ModifierBook {
    pub fn new(formats: Vec<(u32, Vec<u64>)>) -> Self {
        let mut b = ModifierBook(formats);
        b.0.retain(|(_, m)| !m.is_empty());
        b
    }

    pub fn formats(&self) -> &[(u32, Vec<u64>)] {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Forget a failed pair. True if it was offered.
    pub fn remove(&mut self, fourcc: u32, modifier: u64) -> bool {
        let mut removed = false;
        for (f, mods) in &mut self.0 {
            if *f == fourcc {
                let n = mods.len();
                mods.retain(|&m| m != modifier);
                removed |= mods.len() != n;
            }
        }
        self.0.retain(|(_, m)| !m.is_empty());
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourccs_match_drm_fourcc_h() {
        // drm_fourcc.h: #define DRM_FORMAT_XRGB8888 fourcc_code('X', 'R', '2', '4')
        assert_eq!(DRM_XRGB8888, 0x3432_5258);
        assert_eq!(DRM_ARGB8888, 0x3432_5241);
        assert_eq!(fourcc_name(DRM_XBGR8888), "XB24");
    }

    #[test]
    fn newest_usable_skips_old_and_corrupted() {
        // (seq, corrupted)
        let q = vec![(1, false), (2, false), (3, true), (4, false), (5, true)];
        let (best, skipped) = newest_usable(q.into_iter(), |b: &mut (i32, bool)| !b.1);
        assert_eq!(best, Some((4, false)));
        assert_eq!(skipped, 2);
        let (none, n) = newest_usable(vec![(1, true)].into_iter(), |b: &mut (i32, bool)| !b.1);
        assert_eq!((none, n), (None, 0));
    }

    #[test]
    fn modifier_book_drops_failed_pairs() {
        let y_tiled = 0x0100_0000_0000_0002;
        let mut b = ModifierBook::new(vec![
            (DRM_XRGB8888, vec![y_tiled, DRM_FORMAT_MOD_LINEAR]),
            (DRM_ARGB8888, vec![DRM_FORMAT_MOD_LINEAR]),
            (DRM_XBGR8888, vec![]),
        ]);
        assert_eq!(b.formats().len(), 2);
        assert!(b.remove(DRM_XRGB8888, y_tiled));
        assert!(!b.remove(DRM_XRGB8888, y_tiled));
        assert!(b.remove(DRM_ARGB8888, DRM_FORMAT_MOD_LINEAR));
        assert_eq!(b.formats(), &[(DRM_XRGB8888, vec![DRM_FORMAT_MOD_LINEAR])]);
        assert!(b.remove(DRM_XRGB8888, DRM_FORMAT_MOD_LINEAR));
        assert!(b.is_empty());
    }
}
