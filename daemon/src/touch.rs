//! Finger touches from the iPad → a virtual touchscreen (Linux multitouch
//! protocol, type B: one slot per finger), with palm rejection.
//!
//! Apps that understand touch then do their own gestures on the iPad's
//! screen: two-finger pan, pinch-zoom and rotation in Krita, pinch-zoom and
//! scrolling in browsers. Palms are kept out two ways:
//! * while the pencil is near (hovering or touching), new fingers are
//!   ignored and fingers already down are cancelled;
//! * a touch larger than `palm_radius_pt` is ignored (or cancelled when it
//!   grows into one).
//!
//! A cancelled touch is marked `MT_TOOL_PALM` before it ends, which libinput
//! turns into a touch *cancel* (apps drop the gesture) instead of a lift
//! (apps would act on it).

use crate::events::*;
use crate::tablet::{Frame, TabletGeometry};
use sidecar_protocol::{TouchPhase, TouchSample};
use std::collections::HashSet;

/// Fingers tracked at once (iPadOS reports up to 10 or so).
pub const MAX_SLOTS: usize = 10;
pub const MT_TOOL_FINGER: i32 = 0;
pub const MT_TOOL_PALM: i32 = 2;

#[derive(Debug, Clone)]
pub struct TouchConfig {
    /// Touches with a larger `UITouch.majorRadius` are palms. 0 = off.
    pub palm_radius_pt: f32,
}

impl Default for TouchConfig {
    fn default() -> Self {
        TouchConfig {
            palm_radius_pt: 45.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    /// The iPad's touch id.
    id: u16,
    x: i32,
    y: i32,
}

/// Counters for the session log.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TouchStats {
    pub fingers: u64,
    pub palms: u64,
    pub pen_near: u64,
    pub cancelled: u64,
}

pub struct TouchMapper {
    geo: TabletGeometry,
    cfg: TouchConfig,
    slots: [Option<Slot>; MAX_SLOTS],
    /// iPad ids we decided to ignore until they lift.
    ignored: HashSet<u16>,
    next_tracking_id: i32,
    pen_near: bool,
    /// Last single-touch emulation position (ABS_X/ABS_Y).
    last_xy: Option<(i32, i32)>,
    pub stats: TouchStats,
}

impl TouchMapper {
    pub fn new(geo: TabletGeometry, cfg: TouchConfig) -> Self {
        TouchMapper {
            geo,
            cfg,
            slots: [None; MAX_SLOTS],
            ignored: HashSet::new(),
            next_tracking_id: 1,
            pen_near: false,
            last_xy: None,
            stats: TouchStats::default(),
        }
    }

    pub fn active(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    fn to_abs(&self, t: &TouchSample) -> (i32, i32) {
        (
            (t.x.clamp(0.0, 1.0) as f64 * self.geo.x_max as f64).round() as i32,
            (t.y.clamp(0.0, 1.0) as f64 * self.geo.y_max as f64).round() as i32,
        )
    }

    fn is_palm(&self, t: &TouchSample) -> bool {
        self.cfg.palm_radius_pt > 0.0 && t.major_radius_pt > self.cfg.palm_radius_pt
    }

    fn slot_of(&self, id: u16) -> Option<usize> {
        self.slots
            .iter()
            .position(|s| s.is_some_and(|s| s.id == id))
    }

    /// The pencil came near (hover or contact) or left. Coming near cancels
    /// every finger: a hand resting on the screen while drawing must not
    /// pan or zoom.
    pub fn set_pen_near(&mut self, near: bool) -> Vec<Frame> {
        if near == self.pen_near {
            return Vec::new();
        }
        self.pen_near = near;
        if !near {
            return Vec::new();
        }
        let mut frames = Vec::new();
        for i in 0..MAX_SLOTS {
            if let Some(s) = self.slots[i] {
                self.ignored.insert(s.id);
                self.cancel_slot(i, &mut frames);
            }
        }
        frames
    }

    /// One TouchBatch, in order. Touches with the same timestamp (one UIKit
    /// event) share an evdev frame, so two fingers moving together arrive
    /// together.
    pub fn batch(&mut self, touches: &[TouchSample]) -> Vec<Frame> {
        let mut frames = Vec::new();
        let mut cur = Frame::new();
        let mut cur_ts = None;
        for t in touches {
            if cur_ts.is_some_and(|ts| ts != t.timestamp_ns) {
                self.close(&mut cur, &mut frames);
            }
            cur_ts = Some(t.timestamp_ns);
            self.touch(t, &mut cur, &mut frames);
        }
        self.close(&mut cur, &mut frames);
        frames
    }

    fn touch(&mut self, t: &TouchSample, cur: &mut Frame, frames: &mut Vec<Frame>) {
        match t.phase {
            TouchPhase::Down => {
                // A down for an id we still track means we missed its end.
                if let Some(i) = self.slot_of(t.id) {
                    self.end_slot(i, cur);
                }
                self.ignored.remove(&t.id);
                if self.pen_near {
                    self.stats.pen_near += 1;
                    self.ignored.insert(t.id);
                    return;
                }
                if self.is_palm(t) {
                    self.stats.palms += 1;
                    self.ignored.insert(t.id);
                    return;
                }
                let Some(i) = self.slots.iter().position(Option::is_none) else {
                    self.ignored.insert(t.id);
                    return;
                };
                self.stats.fingers += 1;
                let (x, y) = self.to_abs(t);
                let tracking = self.next_tracking_id;
                self.next_tracking_id = if tracking >= 0xFFFF { 1 } else { tracking + 1 };
                self.slots[i] = Some(Slot { id: t.id, x, y });
                cur.push(RawEvent::abs(ABS_MT_SLOT, i as i32));
                cur.push(RawEvent::abs(ABS_MT_TRACKING_ID, tracking));
                cur.push(RawEvent::abs(ABS_MT_TOOL_TYPE, MT_TOOL_FINGER));
                cur.push(RawEvent::abs(ABS_MT_POSITION_X, x));
                cur.push(RawEvent::abs(ABS_MT_POSITION_Y, y));
            }
            TouchPhase::Move => {
                if self.ignored.contains(&t.id) {
                    return;
                }
                let Some(i) = self.slot_of(t.id) else {
                    return;
                };
                if self.is_palm(t) {
                    // A finger that spreads into a palm: take it back.
                    self.stats.palms += 1;
                    self.ignored.insert(t.id);
                    self.close(cur, frames);
                    self.cancel_slot(i, frames);
                    return;
                }
                let (x, y) = self.to_abs(t);
                let s = self.slots[i].as_mut().expect("slot_of found it");
                if (s.x, s.y) == (x, y) {
                    return;
                }
                cur.push(RawEvent::abs(ABS_MT_SLOT, i as i32));
                if s.x != x {
                    cur.push(RawEvent::abs(ABS_MT_POSITION_X, x));
                }
                if s.y != y {
                    cur.push(RawEvent::abs(ABS_MT_POSITION_Y, y));
                }
                s.x = x;
                s.y = y;
            }
            TouchPhase::Up => {
                if self.ignored.remove(&t.id) {
                    return;
                }
                if let Some(i) = self.slot_of(t.id) {
                    self.end_slot(i, cur);
                }
            }
            TouchPhase::Cancel => {
                if self.ignored.remove(&t.id) {
                    return;
                }
                if let Some(i) = self.slot_of(t.id) {
                    self.close(cur, frames);
                    self.cancel_slot(i, frames);
                }
            }
        }
    }

    fn end_slot(&mut self, i: usize, f: &mut Frame) {
        self.slots[i] = None;
        f.push(RawEvent::abs(ABS_MT_SLOT, i as i32));
        f.push(RawEvent::abs(ABS_MT_TRACKING_ID, -1));
    }

    /// Mark the touch as a palm (libinput cancels it), then end it.
    fn cancel_slot(&mut self, i: usize, frames: &mut Vec<Frame>) {
        self.stats.cancelled += 1;
        frames.push(vec![
            RawEvent::abs(ABS_MT_SLOT, i as i32),
            RawEvent::abs(ABS_MT_TOOL_TYPE, MT_TOOL_PALM),
        ]);
        let mut end = Frame::new();
        self.end_slot(i, &mut end);
        self.emulate(&mut end);
        frames.push(end);
    }

    /// Append BTN_TOUCH and the single-touch position to `cur`, then push it.
    fn close(&mut self, cur: &mut Frame, frames: &mut Vec<Frame>) {
        if cur.is_empty() {
            return;
        }
        self.emulate(cur);
        frames.push(std::mem::take(cur));
    }

    /// Single-touch emulation, like the kernel's: BTN_TOUCH while any finger
    /// is down, ABS_X/ABS_Y follow the lowest active slot.
    fn emulate(&mut self, f: &mut Frame) {
        let first = self.slots.iter().flatten().next().map(|s| (s.x, s.y));
        match (self.last_xy, first) {
            (None, Some((x, y))) => {
                f.push(RawEvent::key(BTN_TOUCH, true));
                f.push(RawEvent::abs(ABS_X, x));
                f.push(RawEvent::abs(ABS_Y, y));
            }
            (Some((ox, oy)), Some((x, y))) => {
                if ox != x {
                    f.push(RawEvent::abs(ABS_X, x));
                }
                if oy != y {
                    f.push(RawEvent::abs(ABS_Y, y));
                }
            }
            (Some(_), None) => f.push(RawEvent::key(BTN_TOUCH, false)),
            (None, None) => {}
        }
        self.last_xy = first;
    }

    /// End every finger (session over, iPad gone): lifts, not cancels.
    pub fn release_all(&mut self) -> Vec<Frame> {
        let mut f = Frame::new();
        for i in 0..MAX_SLOTS {
            if self.slots[i].is_some() {
                self.end_slot(i, &mut f);
            }
        }
        self.ignored.clear();
        let mut frames = Vec::new();
        self.close(&mut f, &mut frames);
        frames
    }
}
