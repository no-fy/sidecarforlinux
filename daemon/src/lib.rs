//! sidecard: Linux side of sidecarforlinux.
//!
//! The library is split so the protocol/session/mapping logic can be tested
//! without an iPad and without /dev/uinput (see `tests/`).

pub mod events;
pub mod gestures;
pub mod keys;
pub mod sink;
pub mod tablet;
