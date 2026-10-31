//! Per-stage timing of the native video path, logged every 10 s next to
//! the existing "encoder out -> iPad" and "pen input -> frame encoded" lines.
//!
//! All timestamps are CLOCK_MONOTONIC nanoseconds (`clock::now_ns`). KWin
//! stamps its capture tick with `std::chrono::steady_clock`, the same clock
//! (docs/research/native-video.md §1).

use crate::clock::LatencyStats;
use std::collections::VecDeque;

/// When one frame passed each stage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StageTimes {
    /// The compositor's capture tick (KWin: `spa_meta_header.pts`), if the
    /// source gave a plausible one.
    pub tick_ns: Option<u64>,
    /// The daemon dequeued the buffer.
    pub arrived_ns: u64,
    /// The picture was usable on the GPU: the compositor's own GPU render
    /// into the DMA-BUF had finished, or our CPU upload (shared memory) was
    /// done. 0 = not measured (hybrid).
    pub ready_ns: u64,
    /// Colour conversion finished on the GPU; the compositor's buffer was
    /// handed back.
    pub converted_ns: u64,
    /// The encoded frame was in our hands.
    pub encoded_ns: u64,
    /// Newer buffers were waiting, so this many older ones were skipped
    /// just before this frame.
    pub stale_skipped: u32,
    /// A re-encode of the last picture (a requested keyframe while the
    /// screen is idle, or the 1 s keep-alive), not a new capture.
    pub repeat: bool,
}

impl StageTimes {
    /// A source timestamp is used only if it's on our clock: not in the
    /// future and not older than a second.
    pub fn plausible_tick(pts_ns: i64, arrived_ns: u64) -> Option<u64> {
        if pts_ns <= 0 {
            return None;
        }
        let pts = pts_ns as u64;
        (pts <= arrived_ns && arrived_ns - pts < 1_000_000_000).then_some(pts)
    }

    /// The earliest known moment of this frame.
    pub fn start_ns(&self) -> u64 {
        self.tick_ns.unwrap_or(self.arrived_ns)
    }
}

/// Frames per second leaving the encoder: the busiest second since the last
/// report, and the total. KWin only captures when the screen changes, so
/// with something animating on the iPad screen the peak is the capture rate
/// (at most the negotiated max framerate).
#[derive(Debug, Default)]
pub struct FpsMeter {
    recent: VecDeque<u64>,
    peak: u32,
    frames: u64,
}

impl FpsMeter {
    const WINDOW_NS: u64 = 1_000_000_000;

    pub fn on_frame(&mut self, t: u64) {
        self.frames += 1;
        self.recent.push_back(t);
        while let Some(&first) = self.recent.front() {
            if t.saturating_sub(first) < Self::WINDOW_NS {
                break;
            }
            self.recent.pop_front();
        }
        self.peak = self.peak.max(self.recent.len() as u32);
    }

    /// (most frames in any one second, frames in total) since the last call.
    pub fn take(&mut self) -> (u32, u64) {
        let r = (self.peak, self.frames);
        self.peak = 0;
        self.frames = 0;
        r
    }
}

/// Rolling per-stage statistics.
pub struct StageStats {
    tick_to_arrive: LatencyStats,
    ready: LatencyStats,
    convert: LatencyStats,
    encode: LatencyStats,
    send: LatencyStats,
    total: LatencyStats,
    stale: u64,
    fps: FpsMeter,
    /// Labels for the two middle stages (native: "convert"/"encode";
    /// hybrid: "copy out"/"gst encode").
    labels: (&'static str, &'static str),
}

impl StageStats {
    pub fn new(labels: (&'static str, &'static str)) -> Self {
        StageStats {
            tick_to_arrive: LatencyStats::new(2000),
            ready: LatencyStats::new(2000),
            convert: LatencyStats::new(2000),
            encode: LatencyStats::new(2000),
            send: LatencyStats::new(2000),
            total: LatencyStats::new(2000),
            stale: 0,
            fps: FpsMeter::default(),
            labels,
        }
    }

    /// The frame was written to the iPad's socket at `sent_ns`.
    pub fn on_sent(&mut self, t: &StageTimes, sent_ns: u64) {
        if t.repeat {
            return; // not a capture: would skew every stage
        }
        let d = |a: u64, b: u64| b.saturating_sub(a) as i64;
        if let Some(tick) = t.tick_ns {
            self.tick_to_arrive.push_ns(d(tick, t.arrived_ns));
        }
        let convert_from = if t.ready_ns != 0 {
            self.ready.push_ns(d(t.arrived_ns, t.ready_ns));
            t.ready_ns
        } else {
            t.arrived_ns
        };
        self.convert.push_ns(d(convert_from, t.converted_ns));
        self.encode.push_ns(d(t.converted_ns, t.encoded_ns));
        self.send.push_ns(d(t.encoded_ns, sent_ns));
        self.total.push_ns(d(t.start_ns(), sent_ns));
        self.stale += t.stale_skipped as u64;
        self.fps.on_frame(t.encoded_ns);
    }

    /// One log line for the frames since the last call, or None if there
    /// were none. Clears the window.
    pub fn report(&mut self) -> Option<String> {
        if self.convert.is_empty() {
            return None;
        }
        let ms = |l: &LatencyStats| {
            let r = l.report(None);
            format!(
                "{:.1}/{:.1}",
                r.p50_us as f64 / 1000.0,
                r.p95_us as f64 / 1000.0
            )
        };
        let n = self.convert.len();
        let tick = if self.tick_to_arrive.is_empty() {
            "no capture tick from the compositor".to_string()
        } else {
            format!("capture tick -> daemon {}", ms(&self.tick_to_arrive))
        };
        let (peak, _) = self.fps.take();
        let ready = if self.ready.is_empty() {
            String::new()
        } else {
            format!(", ready {}", ms(&self.ready))
        };
        let line = format!(
            "video stages p50/p95 ms over {n} frames: {tick}{ready}, {} {}, {} {}, send {}; {} -> sent {}; {} stale buffers skipped; peak {peak} frames/s",
            self.labels.0,
            ms(&self.convert),
            self.labels.1,
            ms(&self.encode),
            ms(&self.send),
            if self.tick_to_arrive.is_empty() { "arrival" } else { "tick" },
            ms(&self.total),
            self.stale,
        );
        for l in [
            &mut self.tick_to_arrive,
            &mut self.ready,
            &mut self.convert,
            &mut self.encode,
            &mut self.send,
            &mut self.total,
        ] {
            l.clear();
        }
        self.stale = 0;
        Some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const MS: u64 = 1_000_000;

    #[test]
    fn plausible_ticks_only() {
        assert_eq!(
            StageTimes::plausible_tick(100 * MS as i64, 103 * MS),
            Some(100 * MS)
        );
        // In the future, from another clock, or missing.
        assert_eq!(StageTimes::plausible_tick(104 * MS as i64, 103 * MS), None);
        assert_eq!(StageTimes::plausible_tick(1, 5_000 * MS), None);
        assert_eq!(StageTimes::plausible_tick(0, 5 * MS), None);
        assert_eq!(StageTimes::plausible_tick(-5, 5 * MS), None);
    }

    #[test]
    fn report_splits_the_stages() {
        let mut s = StageStats::new(("convert", "encode"));
        assert_eq!(s.report(), None);
        for i in 0..10u64 {
            let base = 1000 * MS + i * 16 * MS;
            s.on_sent(
                &StageTimes {
                    tick_ns: Some(base),
                    arrived_ns: base + 2 * MS,
                    converted_ns: base + 3 * MS,
                    encoded_ns: base + 7 * MS,
                    stale_skipped: (i == 3) as u32,
                    ..Default::default()
                },
                base + 7 * MS + MS / 2,
            );
        }
        let line = s.report().unwrap();
        assert!(line.contains("over 10 frames"), "{line}");
        assert!(line.contains("capture tick -> daemon 2.0/2.0"), "{line}");
        assert!(!line.contains("ready"), "no ready stage reported: {line}");
        assert!(line.contains("convert 1.0/1.0"), "{line}");
        assert!(line.contains("encode 4.0/4.0"), "{line}");
        assert!(line.contains("send 0.5/0.5"), "{line}");
        assert!(line.contains("tick -> sent 7.5/7.5"), "{line}");
        assert!(line.contains("1 stale buffers skipped"), "{line}");
        assert!(line.contains("peak 10 frames/s"), "{line}");
        assert_eq!(s.report(), None, "window cleared");
    }

    #[test]
    fn ready_stage_splits_the_wait_for_the_compositor_from_the_conversion() {
        let mut s = StageStats::new(("convert", "encode"));
        s.on_sent(
            &StageTimes {
                tick_ns: Some(100 * MS),
                arrived_ns: 101 * MS,
                ready_ns: 104 * MS,
                converted_ns: 105 * MS,
                encoded_ns: 109 * MS,
                ..Default::default()
            },
            110 * MS,
        );
        let line = s.report().unwrap();
        assert!(
            line.contains("ready 3.0/3.0, convert 1.0/1.0, encode 4.0/4.0"),
            "{line}"
        );
        assert!(line.contains("tick -> sent 10.0/10.0"), "{line}");
    }

    #[test]
    fn repeats_are_not_counted() {
        let mut s = StageStats::new(("convert", "encode"));
        s.on_sent(
            &StageTimes {
                arrived_ns: 10 * MS,
                converted_ns: 10 * MS,
                encoded_ns: 12 * MS,
                repeat: true,
                ..Default::default()
            },
            13 * MS,
        );
        assert_eq!(s.report(), None);
    }

    #[test]
    fn without_ticks_the_total_starts_at_arrival() {
        let mut s = StageStats::new(("copy out", "gst encode"));
        s.on_sent(
            &StageTimes {
                tick_ns: None,
                arrived_ns: 10 * MS,
                converted_ns: 11 * MS,
                encoded_ns: 20 * MS,
                ..Default::default()
            },
            21 * MS,
        );
        let line = s.report().unwrap();
        assert!(line.contains("no capture tick"), "{line}");
        assert!(line.contains("gst encode 9.0/9.0"), "{line}");
        assert!(line.contains("arrival -> sent 11.0/11.0"), "{line}");
    }

    #[test]
    fn fps_peak_is_the_busiest_second() {
        let mut m = FpsMeter::default();
        // 3 s at 60 fps, a pause, then 1 s at 120 fps.
        for i in 0..180u64 {
            m.on_frame(i * 1000 * MS / 60);
        }
        for i in 0..120u64 {
            m.on_frame(5000 * MS + i * 1000 * MS / 120);
        }
        assert_eq!(m.take(), (120, 300));
        assert_eq!(m.take(), (0, 0));
    }
}
