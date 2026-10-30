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
