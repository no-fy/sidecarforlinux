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
