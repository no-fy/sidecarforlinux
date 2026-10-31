//! Finger touches → multitouch evdev frames, and palm rejection.

use sidecar_protocol::{sample_flags, TouchPhase, TouchSample};
use sidecard::events::*;
use sidecard::tablet::{Frame, TabletGeometry};
use sidecard::touch::{TouchConfig, TouchMapper, MT_TOOL_FINGER, MT_TOOL_PALM};

const GEO: TabletGeometry = TabletGeometry {
    x_max: 1000,
    y_max: 500,
    resolution: 10,
};

fn t(ts: u64, id: u16, phase: TouchPhase, x: f32, y: f32) -> TouchSample {
    TouchSample {
        timestamp_ns: ts,
        id,
        phase,
        flags: 0,
        x,
        y,
        major_radius_pt: 20.0,
    }
}

fn mapper() -> TouchMapper {
    TouchMapper::new(GEO, TouchConfig::default())
}

fn abs(code: u16, v: i32) -> RawEvent {
    RawEvent::abs(code, v)
}

fn has(f: &Frame, e: RawEvent) -> bool {
    f.contains(&e)
}

#[test]
fn two_fingers_down_together_share_one_frame() {
    let mut m = mapper();
    let frames = m.batch(&[
        t(1, 7, TouchPhase::Down, 0.1, 0.2),
        t(1, 9, TouchPhase::Down, 0.5, 0.5),
    ]);
    assert_eq!(frames.len(), 1, "{frames:?}");
    let f = &frames[0];
    assert_eq!(
        f[..5],
        [
            abs(ABS_MT_SLOT, 0),
            abs(ABS_MT_TRACKING_ID, 1),
            abs(ABS_MT_TOOL_TYPE, MT_TOOL_FINGER),
            abs(ABS_MT_POSITION_X, 100),
            abs(ABS_MT_POSITION_Y, 100),
        ]
    );
    assert!(has(f, abs(ABS_MT_SLOT, 1)));
    assert!(has(f, abs(ABS_MT_TRACKING_ID, 2)));
    assert!(has(f, abs(ABS_MT_POSITION_X, 500)));
    // Single-touch emulation follows slot 0.
    assert!(has(f, RawEvent::key(BTN_TOUCH, true)));
    assert!(has(f, abs(ABS_X, 100)));
    assert!(has(f, abs(ABS_Y, 100)));
    assert_eq!(m.active(), 2);
}

#[test]
fn a_pinch_moves_both_fingers_then_lifts() {
    let mut m = mapper();
    m.batch(&[
        t(1, 1, TouchPhase::Down, 0.4, 0.5),
        t(1, 2, TouchPhase::Down, 0.6, 0.5),
    ]);
    let moved = m.batch(&[
        t(2, 1, TouchPhase::Move, 0.3, 0.5),
        t(2, 2, TouchPhase::Move, 0.7, 0.5),
        // A coalesced earlier sample of the same event time is just another move.
        TouchSample {
            flags: sample_flags::COALESCED,
            ..t(3, 2, TouchPhase::Move, 0.7, 0.5)
        },
    ]);
    assert_eq!(
        moved,
        vec![vec![
            abs(ABS_MT_SLOT, 0),
            abs(ABS_MT_POSITION_X, 300),
            abs(ABS_MT_SLOT, 1),
            abs(ABS_MT_POSITION_X, 700),
            abs(ABS_X, 300),
        ]],
        "y unchanged and the repeated position are left out"
    );
    let up1 = m.batch(&[t(4, 1, TouchPhase::Up, 0.3, 0.5)]);
    assert_eq!(
        up1,
        vec![vec![
            abs(ABS_MT_SLOT, 0),
            abs(ABS_MT_TRACKING_ID, -1),
            abs(ABS_X, 700),
        ]]
    );
    let up2 = m.batch(&[t(5, 2, TouchPhase::Up, 0.7, 0.5)]);
    assert_eq!(
        up2,
        vec![vec![
            abs(ABS_MT_SLOT, 1),
            abs(ABS_MT_TRACKING_ID, -1),
            RawEvent::key(BTN_TOUCH, false),
        ]]
    );
    assert_eq!(m.active(), 0);
    assert_eq!(m.stats.fingers, 2);
}

#[test]
fn palms_are_ignored_from_start_to_lift() {
    let mut m = mapper();
    let palm = TouchSample {
        major_radius_pt: 80.0,
        ..t(1, 3, TouchPhase::Down, 0.5, 0.5)
    };
    assert!(m.batch(&[palm]).is_empty());
    assert!(m
        .batch(&[TouchSample {
            phase: TouchPhase::Move,
            ..palm
        }])
        .is_empty());
    assert!(m
        .batch(&[TouchSample {
            phase: TouchPhase::Up,
            ..palm
        }])
        .is_empty());
    assert_eq!(m.stats.palms, 1);
    assert_eq!(m.active(), 0);
    // palm_radius_pt = 0 turns the size check off.
    let mut off = TouchMapper::new(
        GEO,
        TouchConfig {
            palm_radius_pt: 0.0,
        },
    );
    assert!(!off.batch(&[palm]).is_empty());
}

#[test]
fn a_finger_that_spreads_into_a_palm_is_cancelled_not_lifted() {
    let mut m = mapper();
    m.batch(&[t(1, 4, TouchPhase::Down, 0.5, 0.5)]);
    let frames = m.batch(&[TouchSample {
        major_radius_pt: 90.0,
        ..t(2, 4, TouchPhase::Move, 0.52, 0.5)
    }]);
    assert_eq!(
        frames,
        vec![
            vec![abs(ABS_MT_SLOT, 0), abs(ABS_MT_TOOL_TYPE, MT_TOOL_PALM)],
            vec![
                abs(ABS_MT_SLOT, 0),
                abs(ABS_MT_TRACKING_ID, -1),
                RawEvent::key(BTN_TOUCH, false),
            ],
        ]
    );
    // Its later moves and lift change nothing.
    assert!(m.batch(&[t(3, 4, TouchPhase::Move, 0.6, 0.5)]).is_empty());
    assert!(m.batch(&[t(4, 4, TouchPhase::Up, 0.6, 0.5)]).is_empty());
    assert_eq!(m.stats.cancelled, 1);
}

#[test]
fn the_pencil_coming_near_cancels_fingers_and_blocks_new_ones() {
    let mut m = mapper();
    m.batch(&[
        t(1, 1, TouchPhase::Down, 0.2, 0.2),
        t(1, 2, TouchPhase::Down, 0.8, 0.8),
    ]);
    let cancel = m.set_pen_near(true);
    let palms: Vec<_> = cancel
        .iter()
        .filter(|f| has(f, abs(ABS_MT_TOOL_TYPE, MT_TOOL_PALM)))
        .collect();
    assert_eq!(palms.len(), 2, "{cancel:?}");
    assert!(has(cancel.last().unwrap(), RawEvent::key(BTN_TOUCH, false)));
    assert_eq!(m.active(), 0);
    assert!(m.set_pen_near(true).is_empty(), "only on the change");
    // A hand landing while the pencil is near.
    assert!(m.batch(&[t(2, 5, TouchPhase::Down, 0.5, 0.5)]).is_empty());
    assert_eq!(m.stats.pen_near, 1);
    // Pencil gone: those touches stay ignored until they lift...
    assert!(m.set_pen_near(false).is_empty());
    assert!(m.batch(&[t(3, 5, TouchPhase::Move, 0.6, 0.5)]).is_empty());
    assert!(m.batch(&[t(3, 1, TouchPhase::Move, 0.3, 0.2)]).is_empty());
    assert!(m.batch(&[t(4, 5, TouchPhase::Up, 0.6, 0.5)]).is_empty());
    // ...and a new finger works again.
    let f = m.batch(&[t(5, 6, TouchPhase::Down, 0.5, 0.5)]);
    assert!(has(&f[0], abs(ABS_MT_TRACKING_ID, 3)));
}

#[test]
fn ipados_cancel_becomes_a_palm_cancel() {
    let mut m = mapper();
    m.batch(&[t(1, 1, TouchPhase::Down, 0.5, 0.5)]);
    let f = m.batch(&[t(2, 1, TouchPhase::Cancel, 0.5, 0.5)]);
    assert_eq!(
        f[0],
        vec![abs(ABS_MT_SLOT, 0), abs(ABS_MT_TOOL_TYPE, MT_TOOL_PALM)]
    );
    assert!(has(&f[1], abs(ABS_MT_TRACKING_ID, -1)));
    // The slot's next finger is a finger again.
    let d = m.batch(&[t(3, 2, TouchPhase::Down, 0.5, 0.5)]);
    assert!(has(&d[0], abs(ABS_MT_TOOL_TYPE, MT_TOOL_FINGER)));
}

#[test]
fn more_fingers_than_slots_are_ignored() {
    let mut m = mapper();
    let downs: Vec<_> = (0..12)
        .map(|i| t(1, i, TouchPhase::Down, 0.05 * i as f32, 0.5))
        .collect();
    m.batch(&downs);
    assert_eq!(m.active(), 10);
    // The 11th finger's lift is harmless.
    assert!(m.batch(&[t(2, 10, TouchPhase::Up, 0.5, 0.5)]).is_empty());
}

#[test]
fn release_all_lifts_everything() {
    let mut m = mapper();
    m.batch(&[
        t(1, 1, TouchPhase::Down, 0.2, 0.2),
        t(1, 2, TouchPhase::Down, 0.8, 0.8),
    ]);
    let f = m.release_all();
    assert_eq!(f.len(), 1);
    assert!(has(&f[0], abs(ABS_MT_TRACKING_ID, -1)));
    assert!(has(&f[0], RawEvent::key(BTN_TOUCH, false)));
    assert!(!f[0].contains(&abs(ABS_MT_TOOL_TYPE, MT_TOOL_PALM)));
    assert!(m.release_all().is_empty());
}

#[test]
fn a_repeated_down_ends_the_stale_contact_first() {
    let mut m = mapper();
    m.batch(&[t(1, 1, TouchPhase::Down, 0.2, 0.2)]);
    let f = m.batch(&[t(2, 1, TouchPhase::Down, 0.4, 0.4)]);
    assert_eq!(
        f[0][..4],
        [
            abs(ABS_MT_SLOT, 0),
            abs(ABS_MT_TRACKING_ID, -1),
            abs(ABS_MT_SLOT, 0),
            abs(ABS_MT_TRACKING_ID, 2),
        ]
    );
    assert_eq!(m.active(), 1);
}
