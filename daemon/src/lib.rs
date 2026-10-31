//! sidecard: Linux side of sidecarforlinux.
//!
//! The library is split so the protocol/session/mapping logic can be tested
//! without an iPad and without /dev/uinput (see `tests/`).

pub mod clock;
pub mod config;
pub mod conn;
#[cfg(feature = "display")]
pub mod display;
pub mod events;
pub mod fake_ipad;
pub mod gestures;
pub mod identity;
pub mod kde;
pub mod keys;
pub mod native;
pub mod recording;
pub mod session;
pub mod sink;
pub mod tablet;
pub mod tls;
pub mod touch;
pub mod transport;
pub mod usbmux;
pub mod video;
pub mod videochan;
pub mod videostats;
