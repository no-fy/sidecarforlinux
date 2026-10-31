//! Native second-screen video: capture and encode inside the daemon
//! (`[video] backend = "native"`, DECISIONS D20). The pieces here that don't
//! touch PipeWire or VA-API build and are tested on every system; the rest
//! is Linux-only behind the `native-video` cargo feature.

pub mod frame;
pub mod h264;
pub mod latest;
pub mod stats;

#[cfg(all(feature = "native-video", target_os = "linux"))]
pub mod encoder;
#[cfg(all(feature = "native-video", target_os = "linux"))]
pub mod hybrid;
#[cfg(all(feature = "native-video", target_os = "linux"))]
pub mod pw;
#[cfg(all(feature = "native-video", target_os = "linux"))]
pub mod va;
#[cfg(all(feature = "native-video", target_os = "linux"))]
mod va_sys;

/// A video configuration that can never work on this machine (no encoder
/// with the requested entry point, no usable rate control, a VA device that
/// doesn't exist, ...). The video channel reports it once and stays off
/// instead of retrying forever; transient errors still retry.
#[derive(Debug)]
pub struct Unsupported(pub String);

impl std::fmt::Display for Unsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unsupported {}

/// True if `e` (or anything it wraps) is `Unsupported`.
pub fn is_unsupported(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.is::<Unsupported>())
}
