//! Pairing math: certificate fingerprints, nonce commitment and the 6-digit
//! short authentication string (SAS) both screens show.
//!
//! Flow (inside an already established TLS 1.3 session, see docs/protocol.md):
//!
//! 1. iPad → daemon  `PairCommit(C)` with `C = commitment(fp_ipad, N_i)`
//! 2. daemon → iPad  `PairNonce(N_d)`
//! 3. iPad → daemon  `PairReveal(N_i)`; daemon checks `C`
//! 4. both display `sas(fp_daemon, fp_ipad, N_d, N_i)` and send `PairDecision`.
//!
//! The commitment stops a man-in-the-middle from grinding certificates or
//! nonces until the two codes match: the side that reveals last (the iPad) is
//! bound to its nonce before it sees the daemon's. This is the same structure
//! as Bluetooth LE Secure Connections "numeric comparison".

use sha2::{Digest, Sha256};

pub type Fingerprint = [u8; 32];
pub type Nonce = [u8; 32];

const COMMIT_DOMAIN: &[u8] = b"sidecarforlinux pair commit v1";
const SAS_DOMAIN: &[u8] = b"sidecarforlinux pair sas v1";

/// SHA-256 of the DER-encoded certificate.
pub fn fingerprint(cert_der: &[u8]) -> Fingerprint {
    Sha256::digest(cert_der).into()
}

pub fn commitment(fp_ipad: &Fingerprint, nonce_ipad: &Nonce) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(COMMIT_DOMAIN);
    h.update(fp_ipad);
    h.update(nonce_ipad);
    h.finalize().into()
}

/// 6-digit code, 0..=999_999.
pub fn sas(
    fp_daemon: &Fingerprint,
    fp_ipad: &Fingerprint,
    nonce_daemon: &Nonce,
    nonce_ipad: &Nonce,
) -> u32 {
    let mut h = Sha256::new();
    h.update(SAS_DOMAIN);
    h.update(fp_daemon);
    h.update(fp_ipad);
    h.update(nonce_daemon);
    h.update(nonce_ipad);
    let d = h.finalize();
    u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 1_000_000
}

/// "123 456"
pub fn format_sas(code: u32) -> String {
    format!("{:03} {:03}", code / 1000, code % 1000)
}

/// Lower-case hex, colon separated pairs ("ab:cd:...") for display.
pub fn format_fingerprint(fp: &Fingerprint) -> String {
    fp.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// Plain lower-case hex (storage format).
pub fn hex_fingerprint(fp: &Fingerprint) -> String {
    fp.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn parse_hex_fingerprint(s: &str) -> Option<Fingerprint> {
    let s: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if s.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in s.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sas_is_six_digits_and_order_sensitive() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let n1 = [3u8; 32];
        let n2 = [4u8; 32];
        let s = sas(&a, &b, &n1, &n2);
        assert!(s < 1_000_000);
        assert_ne!(s, sas(&b, &a, &n1, &n2));
        assert_eq!(format_sas(42), "000 042");
        assert_eq!(format_sas(123456), "123 456");
    }

    #[test]
    fn fingerprint_hex_roundtrip() {
        let fp = fingerprint(b"cert");
        assert_eq!(parse_hex_fingerprint(&hex_fingerprint(&fp)), Some(fp));
        assert_eq!(parse_hex_fingerprint(&format_fingerprint(&fp)), Some(fp));
        assert_eq!(parse_hex_fingerprint("abc"), None);
    }
}
