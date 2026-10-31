//! Clock sync between the iPad (UITouch/systemUptime clock) and the daemon
//! (CLOCK_MONOTONIC), NTP style, and latency statistics.

use sidecar_protocol::{LatencyReport, Pong};
use std::collections::VecDeque;

/// CLOCK_MONOTONIC in nanoseconds.
pub fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: valid pointer to a timespec.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[derive(Debug, Clone, Copy)]
struct SyncSample {
    rtt_ns: i64,
    /// iPad clock minus daemon clock.
    offset_ns: i64,
    at_ns: u64,
}

/// Keeps the last `window` ping/pong exchanges and trusts the one with the
/// smallest round-trip time (least queueing → tightest offset bound,
/// error ≤ rtt/2).
pub struct ClockSync {
    samples: VecDeque<SyncSample>,
    window: usize,
}

impl Default for ClockSync {
    fn default() -> Self {
        Self::new(16)
    }
}

impl ClockSync {
    pub fn new(window: usize) -> Self {
        ClockSync {
            samples: VecDeque::new(),
            window,
        }
    }

    /// `t4_ns`: daemon clock when the pong arrived.
    pub fn on_pong(&mut self, p: &Pong, t4_ns: u64) {
        let (t1, t2, t3, t4) = (
            p.t1_ns as i128,
            p.t2_ns as i128,
            p.t3_ns as i128,
            t4_ns as i128,
        );
        let rtt = (t4 - t1) - (t3 - t2);
        if rtt < 0 {
            return; // clock weirdness (or a forged pong); ignore
        }
        let offset = ((t2 - t1) + (t3 - t4)) / 2;
        self.samples.push_back(SyncSample {
            rtt_ns: rtt as i64,
            offset_ns: offset as i64,
            at_ns: t4_ns,
        });
        while self.samples.len() > self.window {
            self.samples.pop_front();
        }
    }

    fn best(&self) -> Option<&SyncSample> {
        self.samples.iter().min_by_key(|s| s.rtt_ns)
    }

    /// iPad clock − daemon clock, if known.
    pub fn offset_ns(&self) -> Option<i64> {
        self.best().map(|s| s.offset_ns)
    }

    pub fn rtt_ns(&self) -> Option<i64> {
        self.best().map(|s| s.rtt_ns)
    }

    pub fn last_sync_age_ns(&self, now: u64) -> Option<u64> {
        self.samples.back().map(|s| now.saturating_sub(s.at_ns))
    }

    /// Convert an iPad timestamp into the daemon clock.
    pub fn to_local(&self, ipad_ns: u64) -> Option<i64> {
        self.offset_ns().map(|o| ipad_ns as i64 - o)
    }
}

/// Rolling latency window in microseconds.
pub struct LatencyStats {
    values: VecDeque<u32>,
    cap: usize,
    pub total: u64,
}

impl LatencyStats {
    pub fn new(cap: usize) -> Self {
        LatencyStats {
            values: VecDeque::with_capacity(cap),
            cap,
            total: 0,
        }
    }

    pub fn push_ns(&mut self, ns: i64) {
        let us = (ns.max(0) / 1000).min(u32::MAX as i64) as u32;
        if self.values.len() == self.cap {
            self.values.pop_front();
        }
        self.values.push_back(us);
        self.total += 1;
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn clear(&mut self) {
        self.values.clear();
    }

    pub fn percentile(sorted: &[u32], p: f64) -> u32 {
        if sorted.is_empty() {
            return 0;
        }
        let idx = ((p / 100.0) * (sorted.len() - 1) as f64).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    pub fn report(&self, rtt_ns: Option<i64>) -> LatencyReport {
        let mut v: Vec<u32> = self.values.iter().copied().collect();
        v.sort_unstable();
        LatencyReport {
            samples: v.len() as u32,
            p50_us: Self::percentile(&v, 50.0),
            p95_us: Self::percentile(&v, 95.0),
            p99_us: Self::percentile(&v, 99.0),
            max_us: v.last().copied().unwrap_or(0),
            rtt_us: rtt_ns.map(|r| (r / 1000) as u32).unwrap_or(0),
        }
    }

    /// Text histogram (1 ms buckets up to 30 ms) for the latency tool.
    pub fn histogram(&self) -> String {
        let mut buckets = [0usize; 31];
        for v in &self.values {
            buckets[((*v / 1000) as usize).min(30)] += 1;
        }
        let max = buckets.iter().copied().max().unwrap_or(0).max(1);
        let mut s = String::new();
        for (i, n) in buckets.iter().enumerate() {
            if *n == 0 {
                continue;
            }
            let bar = "#".repeat((n * 50).div_ceil(max));
            let label = if i == 30 {
                ">=30 ms".to_string()
            } else {
                format!("{i:>2}-{:<2} ms", i + 1)
            };
            s.push_str(&format!("{label:>9} {n:>6} {bar}\n"));
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_from_min_rtt_sample() {
        let mut c = ClockSync::new(8);
        let offset: i64 = 5_000_000_000; // iPad clock is 5 s ahead
                                         // Symmetric 1 ms legs, then a congested exchange with asymmetric delay.
        let mk = |t1: u64, up: u64, down: u64| {
            let t2 = (t1 as i64 + up as i64 + offset) as u64;
            let t3 = t2 + 10_000;
            let t4 = (t3 as i64 - offset) as u64 + down;
            (
                Pong {
                    id: 0,
                    t1_ns: t1,
                    t2_ns: t2,
                    t3_ns: t3,
                },
                t4,
            )
        };
        let (p, t4) = mk(1_000_000, 500_000, 500_000);
        c.on_pong(&p, t4);
        let (p, t4) = mk(2_000_000, 9_000_000, 300_000);
        c.on_pong(&p, t4);
        assert_eq!(c.offset_ns(), Some(offset));
        assert_eq!(c.rtt_ns(), Some(1_000_000));
        assert_eq!(c.to_local(offset as u64 + 42), Some(42));
    }

    #[test]
    fn percentiles() {
        let mut l = LatencyStats::new(1000);
        for i in 1..=100 {
            l.push_ns(i * 1_000_000);
        }
        let r = l.report(Some(2_000_000));
        assert_eq!(r.samples, 100);
        assert_eq!(r.p50_us, 51_000);
        assert_eq!(r.p95_us, 95_000);
        assert_eq!(r.max_us, 100_000);
        assert_eq!(r.rtt_us, 2000);
        assert!(l.histogram().contains(">=30 ms"));
    }
}

/// Measures pen input → first encoded frame after an idle screen: the PC's
/// share of the second-screen lag (compositor redraw, screen capture,
/// encoder), which the encoder-out → iPad metric doesn't see.
///
/// KWin only sends frames when the virtual monitor changes. So the first
/// frame after a quiet spell answers the first input of the burst: moving the
/// pen after a pause moves the cursor, and that redraw is the next frame. The
/// 1 s capture keep-alive can occasionally pair up by chance; medians absorb it.
#[derive(Debug, Default)]
pub struct InputProbe {
    last_frame_ns: std::sync::atomic::AtomicU64,
    marker_ns: std::sync::atomic::AtomicU64,
}

impl InputProbe {
    /// No frame for this long = the screen was idle.
    const IDLE_NS: u64 = 150_000_000;
    /// A frame later than this answers something else.
    const MAX_NS: u64 = 500_000_000;

    /// A pen sample was written to uinput at `t` (daemon clock).
    pub fn on_input(&self, t: u64) {
        use std::sync::atomic::Ordering::Relaxed;
        if self.marker_ns.load(Relaxed) != 0 {
            return;
        }
        if t.saturating_sub(self.last_frame_ns.load(Relaxed)) >= Self::IDLE_NS {
            self.marker_ns.store(t, Relaxed);
        }
    }

    /// A frame left the encoder at `t`; returns input → frame if it answers a
    /// marked input.
    pub fn on_frame(&self, t: u64) -> Option<u64> {
        use std::sync::atomic::Ordering::Relaxed;
        self.last_frame_ns.store(t, Relaxed);
        let m = self.marker_ns.swap(0, Relaxed);
        if m == 0 || t < m {
            return None;
        }
        Some(t - m).filter(|d| *d <= Self::MAX_NS)
    }

    /// Like `on_frame`, for sources that know when the compositor captured
    /// the frame (`tick`). A frame captured before the marked input can't
    /// show it, so it doesn't consume the marker. Returns (input -> tick,
    /// input -> encoded).
    pub fn on_frame_split(&self, tick: Option<u64>, t: u64) -> Option<(Option<u64>, u64)> {
        use std::sync::atomic::Ordering::Relaxed;
        let Some(tick) = tick else {
            return self.on_frame(t).map(|d| (None, d));
        };
        self.last_frame_ns.store(t, Relaxed);
        let m = self.marker_ns.load(Relaxed);
        if m == 0 || tick < m {
            return None;
        }
        if self
            .marker_ns
            .compare_exchange(m, 0, Relaxed, Relaxed)
            .is_err()
        {
            return None;
        }
        Some((Some(tick - m), t - m)).filter(|(_, d)| *d <= Self::MAX_NS)
    }
}

#[cfg(test)]
mod input_probe_tests {
    use super::InputProbe;
    const MS: u64 = 1_000_000;

    #[test]
    fn first_frame_after_idle_answers_the_first_input() {
        let p = InputProbe::default();
        assert_eq!(p.on_frame(1000 * MS), None);
        // Idle 300 ms, then a burst of input; the next frame comes 40 ms later.
        p.on_input(1300 * MS);
        p.on_input(1304 * MS);
        assert_eq!(p.on_frame(1340 * MS), Some(40 * MS));
        // Frames keep coming while moving: input right after a frame isn't marked.
        p.on_input(1345 * MS);
        assert_eq!(p.on_frame(1357 * MS), None);
    }

    #[test]
    fn split_probe_waits_for_a_frame_captured_after_the_input() {
        let p = InputProbe::default();
        p.on_frame_split(Some(990 * MS), 1000 * MS);
        p.on_input(1300 * MS);
        // Captured at 1295 (before the input), encoded at 1310: not the answer.
        assert_eq!(p.on_frame_split(Some(1295 * MS), 1310 * MS), None);
        // Captured at 1316, encoded at 1322.
        assert_eq!(
            p.on_frame_split(Some(1316 * MS), 1322 * MS),
            Some((Some(16 * MS), 22 * MS))
        );
        assert_eq!(p.on_frame_split(Some(1332 * MS), 1338 * MS), None);
        // Without a tick it behaves like on_frame.
        p.on_input(1600 * MS);
        assert_eq!(p.on_frame_split(None, 1640 * MS), Some((None, 40 * MS)));
    }

    #[test]
    fn stale_markers_are_dropped() {
        let p = InputProbe::default();
        p.on_input(1000 * MS);
        assert_eq!(p.on_frame(1800 * MS), None);
        assert_eq!(p.on_frame(1816 * MS), None);
    }
}
