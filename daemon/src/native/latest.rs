//! A one-slot hand-over where the newest value wins: `put` replaces a value
//! the taker hasn't picked up yet. Between capture and a slow consumer (the
//! hybrid backend's pipe to gst-launch) it means the consumer always starts
//! on the newest picture instead of one that waited behind it.
//!
//! The idea comes from OpenDisplay's `latest.rs` (GPL-3.0; read, not
//! copied). In the native backend PipeWire's own buffer queue plays this
//! role (the capture thread drains it to the newest buffer).

use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// What `Latest::take` got.
#[derive(Debug, PartialEq, Eq)]
pub enum Taken<T> {
    Value(T),
    TimedOut,
    /// Closed and empty: no more values will come.
    Closed,
}

pub struct Latest<T> {
    state: Mutex<(Option<T>, bool)>,
    ready: Condvar,
}

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Latest {
            state: Mutex::new((None, false)),
            ready: Condvar::new(),
        }
    }
}

impl<T> Latest<T> {
    /// Store `v`; returns the value it replaced (never taken), if any.
    pub fn put(&self, v: T) -> Option<T> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let old = s.0.replace(v);
        drop(s);
        self.ready.notify_one();
        old
    }

    /// No more values will come; wakes the taker.
    pub fn close(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).1 = true;
        self.ready.notify_all();
    }

    /// Wait up to `timeout` for a value.
    pub fn take(&self, timeout: Duration) -> Taken<T> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if s.0.is_none() && !s.1 {
            s = self
                .ready
                .wait_timeout_while(s, timeout, |s| s.0.is_none() && !s.1)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        match s.0.take() {
            Some(v) => Taken::Value(v),
            None if s.1 => Taken::Closed,
            None => Taken::TimedOut,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn newest_wins() {
        let l = Latest::default();
        assert_eq!(l.put(1), None);
        assert_eq!(l.put(2), Some(1));
        assert_eq!(l.take(Duration::ZERO), Taken::Value(2));
        assert_eq!(l.take(Duration::from_millis(5)), Taken::TimedOut);
    }

    #[test]
    fn close_after_draining() {
        let l = Latest::default();
        l.put(7);
        l.close();
        assert_eq!(l.take(Duration::ZERO), Taken::Value(7));
        assert_eq!(l.take(Duration::from_secs(5)), Taken::Closed);
    }

    #[test]
    fn wakes_a_waiting_taker() {
        let l = Arc::new(Latest::default());
        let l2 = l.clone();
        let t = std::thread::spawn(move || l2.take(Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(20));
        l.put("frame");
        assert_eq!(t.join().unwrap(), Taken::Value("frame"));
    }
}
