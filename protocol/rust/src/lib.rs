//! Wire protocol shared by the Linux daemon and the iPad app.
//!
//! The normative description lives in `docs/protocol.md`; this crate is the
//! reference implementation. Everything is little-endian, length-framed:
//!
//! ```text
//! frame   = length:u32le  type:u8  payload[length - 1]
//! ```
//!
//! Forward-compatibility rules (see the spec):
//! * unknown message types are skipped,
//! * known messages may carry trailing bytes added by later minor versions;
//!   those are ignored,
//! * payloads shorter than the v1.0 minimum are errors.

pub mod frame;
pub mod messages;
pub mod pairing;
mod wire;

pub use frame::{encode_frame, FrameDecoder, MAX_FRAME_LEN};
pub use messages::*;

/// Protocol major version. Peers with different majors must not talk.
pub const PROTOCOL_MAJOR: u16 = 1;
/// Protocol minor version. Higher minors only add optional data.
pub const PROTOCOL_MINOR: u16 = 0;

/// Magic bytes at the start of every `Hello`.
pub const HELLO_MAGIC: [u8; 4] = *b"SCLP";

/// TCP port the iPad app listens on (USB via usbmux and Wi-Fi).
pub const DEFAULT_PORT: u16 = 47800;

/// Bonjour / DNS-SD service type advertised by the iPad app over Wi-Fi.
pub const BONJOUR_SERVICE_TYPE: &str = "_sidecarlinux._tcp";

/// ALPN protocol id used in the TLS handshake.
pub const ALPN: &[u8] = b"sidecar/1";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(usize),
    #[error("empty frame")]
    EmptyFrame,
    #[error("message 0x{ty:02x} truncated: need {need} bytes, have {have}")]
    Truncated { ty: u8, need: usize, have: usize },
    #[error("invalid UTF-8 string")]
    BadString,
    #[error("bad hello magic")]
    BadMagic,
    #[error("invalid value {value} for field {field}")]
    BadValue { field: &'static str, value: u32 },
}
