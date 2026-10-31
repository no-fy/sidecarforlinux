//! Native second-screen video: capture and encode inside the daemon
//! (`[video] backend = "native"`, DECISIONS D20). The pieces here that don't
//! touch PipeWire or VA-API build and are tested on every system; the rest
//! is Linux-only behind the `native-video` cargo feature.

pub mod frame;
pub mod h264;
pub mod latest;
pub mod stats;
