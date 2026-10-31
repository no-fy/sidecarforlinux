//! Where a video frame's time goes once it reaches the iPad, from the
//! iPad's `VideoFeedback`: received → decoded → on screen.
//!
//! `decoded_ns` and `displayed_ns` are 0 when the iPad can't know them. The
//! AVSampleBufferDisplayLayer renderer decodes and displays inside the
//! system and reports neither; the Metal renderer reports both (decode
//! finished, and the moment the frame actually reached the screen).
//!
//! decode and "decoded → on screen" are differences of two iPad
//! timestamps, so they don't depend on clock sync. Only "encoder out → on
//! screen" mixes the two clocks (error ± rtt/2, see clock.rs).

use crate::clock::LatencyStats;
use sidecar_protocol::VideoFeedback;

/// One reporting window. The video channel logs `summary()` every 10 s and
/// then clears it, so switching the iPad's renderer shows in the next line.
pub struct IpadVideoLatency {
    since: std::time::Instant,
    /// Feedback received in this window. While the screen keeps changing,
    /// frames / seconds is the frame rate the PC actually sends.
    frames: u64,
    /// Decoded but never shown: a newer frame took its place first.
    replaced: u64,
    /// Not decoded (decode error, or waiting for a keyframe).
    not_decoded: u64,
    decode: LatencyStats,
    to_screen: LatencyStats,
    received_to_screen: LatencyStats,
    encoder_to_screen: LatencyStats,
}

impl Default for IpadVideoLatency {
    fn default() -> Self {
        Self::new()
    }
}

impl IpadVideoLatency {
    pub fn new() -> Self {
        IpadVideoLatency {
            since: std::time::Instant::now(),
            frames: 0,
            replaced: 0,
            not_decoded: 0,
            decode: LatencyStats::new(4096),
            to_screen: LatencyStats::new(4096),
            received_to_screen: LatencyStats::new(4096),
            encoder_to_screen: LatencyStats::new(4096),
        }
    }

    /// `encoded_ns`: daemon clock when the frame left the encoder (if still
    /// remembered). `offset_ns`: iPad clock − daemon clock (if synced).
    pub fn push(&mut self, fb: &VideoFeedback, encoded_ns: Option<u64>, offset_ns: Option<i64>) {
        self.frames += 1;
        let (rx, dec, shown) = (fb.received_ns as i64, fb.decoded_ns, fb.displayed_ns);
        if fb.request_keyframe && dec == 0 {
            self.not_decoded += 1;
        }
        if dec != 0 {
            self.decode.push_ns(dec as i64 - rx);
            if shown == 0 {
                self.replaced += 1;
            }
        }
        if shown != 0 {
            if dec != 0 {
                self.to_screen.push_ns(shown as i64 - dec as i64);
            }
            self.received_to_screen.push_ns(shown as i64 - rx);
            if let (Some(t_out), Some(off)) = (encoded_ns, offset_ns) {
                self.encoder_to_screen
                    .push_ns(shown as i64 - off - t_out as i64);
            }
        }
    }

    pub fn clear(&mut self) {
        *self = Self::new();
    }

    /// Median "encoder out -> on screen" in this window, if the iPad
    /// reported display times (Metal renderer) and the clocks are synced.
    pub fn encoder_to_screen_p50_us(&self) -> Option<u32> {
        (!self.encoder_to_screen.is_empty()).then(|| self.encoder_to_screen.report(None).p50_us)
    }

    /// One log line, or None if no feedback arrived in this window.
    pub fn summary(&self) -> Option<String> {
        if self.frames == 0 {
            return None;
        }
        let window = format!(
            "{} frames in {:.1} s",
            self.frames,
            self.since.elapsed().as_secs_f64()
        );
        if self.decode.is_empty() && self.received_to_screen.is_empty() {
            return Some(format!(
                "video on the iPad: decode and display times not reported ({window}). \
                 The iPad's renderer is AVSampleBufferDisplayLayer, which doesn't expose them; \
                 choose Metal in the app's gear menu to measure"
            ));
        }
        let pair = |s: &LatencyStats| {
            if s.is_empty() {
                "n/a".to_string()
            } else {
                let r = s.report(None);
                format!(
                    "{:.1}/{:.1}",
                    r.p50_us as f64 / 1000.0,
                    r.p95_us as f64 / 1000.0
                )
            }
        };
        Some(format!(
            "video on the iPad (p50/p95 ms, {window}): decode {} | decoded -> on screen {} | \
             received -> on screen {} | encoder out -> on screen {} | {} replaced by a newer frame, {} not decoded",
            pair(&self.decode),
            pair(&self.to_screen),
            pair(&self.received_to_screen),
            pair(&self.encoder_to_screen),
            self.replaced,
            self.not_decoded,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MS: u64 = 1_000_000;

    fn fb(id: u32, rx: u64, dec: u64, shown: u64, req: bool) -> VideoFeedback {
        VideoFeedback {
            frame_id: id,
            request_keyframe: req,
            received_ns: rx,
            decoded_ns: dec,
            displayed_ns: shown,
        }
    }

    #[test]
    fn breaks_down_metal_feedback() {
        let mut l = IpadVideoLatency::new();
        // iPad clock = daemon clock + 1000 ms. Encoder out at 100 ms (daemon),
        // received 1 ms later, decoded 4 ms later, on screen 10 ms later.
        let off = 1000 * MS as i64;
        for i in 0..10u64 {
            let t = 100 * MS + i * 16 * MS;
            let rx = t + 1000 * MS + MS;
            l.push(
                &fb(i as u32, rx, rx + 4 * MS, rx + 14 * MS, false),
                Some(t),
                Some(off),
            );
        }
        // One frame replaced before display, one that failed to decode.
        l.push(&fb(98, 5000 * MS, 5003 * MS, 0, false), None, Some(off));
        l.push(&fb(99, 5010 * MS, 0, 0, true), None, Some(off));
        let s = l.summary().unwrap();
        assert!(s.contains("12 frames in "), "{s}");
        assert!(s.contains("decode 4.0/4.0"), "{s}");
        assert!(s.contains("decoded -> on screen 10.0/10.0"), "{s}");
        assert!(s.contains("received -> on screen 14.0/14.0"), "{s}");
        assert!(s.contains("encoder out -> on screen 15.0/15.0"), "{s}");
        assert!(
            s.contains("1 replaced by a newer frame, 1 not decoded"),
            "{s}"
        );
        l.clear();
        assert_eq!(l.summary(), None);
    }

    #[test]
    fn says_why_avsamplebufferdisplaylayer_reports_nothing() {
        let mut l = IpadVideoLatency::new();
        l.push(&fb(1, 10 * MS, 0, 0, false), Some(MS), Some(0));
        let s = l.summary().unwrap();
        assert!(s.contains("not reported (1 frames in "), "{s}");
        assert!(s.contains("AVSampleBufferDisplayLayer"), "{s}");
    }

    #[test]
    fn encoder_to_screen_needs_clock_sync() {
        let mut l = IpadVideoLatency::new();
        l.push(&fb(1, 10 * MS, 12 * MS, 20 * MS, false), Some(MS), None);
        let s = l.summary().unwrap();
        assert!(s.contains("decode 2.0/2.0"), "{s}");
        assert!(s.contains("encoder out -> on screen n/a"), "{s}");
    }
}
