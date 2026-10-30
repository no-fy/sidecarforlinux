//! Unit tests for pen sample → evdev mapping. No uinput needed.

use sidecar_protocol::{
    sample_flags, GestureKind, GesturePhase, HoverPose, PenPhase, PenSample, PencilGesture,
    SurfaceInfo,
};
use sidecard::events::*;
use sidecard::gestures::{self, Action, GestureConfig, SqueezeMode};
use sidecard::tablet::*;
use std::f64::consts::{FRAC_PI_2, FRAC_PI_4, PI};

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn geo() -> TabletGeometry {
    TabletGeometry {
        x_max: 10000,
        y_max: 5000,
        resolution: 100,
    }
}

/// Mapper with pressure passed through unchanged (no contact floor) so the
/// expected values are easy to read; the floor has its own test.
fn mapper() -> PenMapper {
    PenMapper::new(
        geo(),
        MapperConfig {
            min_contact_pressure: 0.0,
            ..Default::default()
        },
    )
}

fn s(phase: PenPhase, x: f32, y: f32, p: f32) -> PenSample {
    PenSample {
        timestamp_ns: 0,
        seq: 0,
        phase,
        flags: sample_flags::ROLL_VALID,
        x,
        y,
        pressure: p,
        altitude: FRAC_PI_2 as f32,
        azimuth: 0.0,
        roll: 0.0,
        z_offset: if phase == PenPhase::Hover { 0.5 } else { 0.0 },
        estimation_index: 0,
    }
}

fn find(frames: &[Frame], ty: u16, code: u16) -> Option<i32> {
    frames
        .iter()
        .flatten()
        .filter(|e| e.ty == ty && e.code == code)
        .map(|e| e.value)
        .next_back()
}

#[test]
fn tilt_formula_reference_values() {
    let (tx, ty) = tilt_degrees(FRAC_PI_2, 1.234);
    assert!(
        approx(tx, 0.0) && approx(ty, 0.0),
        "vertical pen has no tilt"
    );
    let (tx, ty) = tilt_degrees(FRAC_PI_4, 0.0);
    assert!(approx(tx, 45.0) && approx(ty, 0.0), "cap towards +x");
    let (tx, ty) = tilt_degrees(FRAC_PI_4, FRAC_PI_2);
    assert!(
        approx(tx, 0.0) && approx(ty, 45.0),
        "cap towards +y (down/user)"
    );
    let (tx, ty) = tilt_degrees(FRAC_PI_4, PI);
    assert!(approx(tx, -45.0) && approx(ty, 0.0), "cap towards -x");
    let (tx, ty) = tilt_degrees(0.0, 0.0);
    assert!(
        approx(tx, 90.0) && approx(ty, 0.0),
        "flat pen, no division by zero"
    );
    // Diagonal: atan(cos45°/tan45°) = atan(0.7071) = 35.264°
    let (tx, ty) = tilt_degrees(FRAC_PI_4, FRAC_PI_4);
    assert!((tx - 35.26439).abs() < 1e-4 && (ty - 35.26439).abs() < 1e-4);
    // Matches the formula from the spec literally away from the singularity.
    for alt in [0.2, 0.5, 0.9, 1.2, 1.5] {
        for az in [-3.0, -1.0, 0.3, 1.9, 2.8] {
            let (tx, ty) = tilt_degrees(alt, az);
            let ex = (f64::cos(az) / f64::tan(alt)).atan().to_degrees();
            let ey = (f64::sin(az) / f64::tan(alt)).atan().to_degrees();
            assert!(approx(tx, ex) && approx(ty, ey), "alt {alt} az {az}");
        }
    }
}

#[test]
fn roll_is_compensated_for_libinput_offset() {
    // Raw values: roll − 90°, in tenths of a degree, wrapped to [0, 3600).
    assert_eq!(roll_to_abs(0.0), 2700);
    assert_eq!(roll_to_abs(FRAC_PI_2), 0);
    assert_eq!(roll_to_abs(PI), 900);
    assert_eq!(roll_to_abs(-FRAC_PI_2), 1800);
    // What applications see through libinput equals the pencil roll.
    for deg in [
        0.0, 1.0, 45.0, 90.0, 179.9, 180.0, 270.0, 359.0, -30.0, 725.0,
    ] {
        let got = libinput_rotation_deg(roll_to_abs(f64::to_radians(deg)));
        let want = f64::rem_euclid(deg, 360.0);
        let diff = (got - want).abs();
        assert!(
            diff < 0.051 || (360.0 - diff) < 0.051,
            "{deg}: libinput sees {got}"
        );
    }
}

#[test]
fn geometry_from_ipad_air_11_landscape() {
    // iPad Air 11" (M2/M3): 1180 x 820 pt, 264 ppi @2x = 132 pt/inch.
    let g = TabletGeometry::from_surface(&SurfaceInfo {
        width_pt: 1180.0,
        height_pt: 820.0,
        scale: 2.0,
        points_per_inch: 132.0,
    });
    assert_eq!(g.x_max, 22706); // 227.06 mm
    assert_eq!(g.y_max, 15779); // 157.79 mm
    assert_eq!(g.resolution, 100);
}

#[test]
fn hover_touch_lift_leave_sequence() {
    let mut m = mapper();
    // Hover in: axes first, tool last, in one frame.
    let f = m.sample(&s(PenPhase::Hover, 0.5, 0.5, 0.0));
    assert_eq!(f.len(), 1);
    assert_eq!(*f[0].last().unwrap(), RawEvent::key(BTN_TOOL_PEN, true));
    assert_eq!(find(&f, EV_ABS, ABS_X), Some(5000));
    assert_eq!(find(&f, EV_ABS, ABS_Y), Some(2500));
    assert_eq!(find(&f, EV_ABS, ABS_DISTANCE), Some(512));
    assert_eq!(find(&f, EV_KEY, BTN_TOUCH), None, "hover must not touch");
    assert!(m.in_proximity() && !m.touching());

    // Touch down.
    let f = m.sample(&s(PenPhase::Down, 0.5, 0.5, 0.25));
    assert_eq!(f.len(), 1);
    assert_eq!(find(&f, EV_KEY, BTN_TOUCH), Some(1));
    assert_eq!(find(&f, EV_ABS, ABS_PRESSURE), Some(2048));
    assert_eq!(find(&f, EV_ABS, ABS_DISTANCE), Some(0));
    assert_eq!(find(&f, EV_KEY, BTN_TOOL_PEN), None, "already in proximity");

    // Move: only changed axes.
    let f = m.sample(&s(PenPhase::Move, 0.6, 0.5, 0.5));
    assert_eq!(
        f,
        vec![vec![
            RawEvent::abs(ABS_X, 6000),
            RawEvent::abs(ABS_PRESSURE, 4096)
        ]]
    );

    // Up: pressure 0 and BTN_TOUCH 0, tool stays in proximity.
    let f = m.sample(&s(PenPhase::Up, 0.6, 0.5, 0.0));
    assert_eq!(find(&f, EV_KEY, BTN_TOUCH), Some(0));
    assert_eq!(find(&f, EV_ABS, ABS_PRESSURE), Some(0));
    assert!(m.in_proximity() && !m.touching());

    // Hover again then leave.
    m.sample(&s(PenPhase::Hover, 0.6, 0.5, 0.0));
    let f = m.sample(&s(PenPhase::Out, 0.6, 0.5, 0.0));
    assert_eq!(find(&f, EV_KEY, BTN_TOOL_PEN), Some(0));
    assert!(!m.in_proximity());
}

#[test]
fn touch_without_hover_enters_proximity_first() {
    let mut m = mapper();
    let f = m.sample(&s(PenPhase::Down, 0.1, 0.2, 1.0));
    assert_eq!(f.len(), 2, "prox-in frame then tip-down frame: {f:?}");
    assert_eq!(find(&f[..1], EV_KEY, BTN_TOOL_PEN), Some(1));
    assert_eq!(find(&f[..1], EV_KEY, BTN_TOUCH), None);
    assert_eq!(find(&f[1..], EV_KEY, BTN_TOUCH), Some(1));
    assert_eq!(find(&f[1..], EV_ABS, ABS_PRESSURE), Some(PRESSURE_MAX));
}

#[test]
fn out_while_touching_lifts_first() {
    let mut m = mapper();
    m.sample(&s(PenPhase::Down, 0.1, 0.2, 0.5));
    let f = m.sample(&s(PenPhase::Out, 0.1, 0.2, 0.0));
    assert_eq!(f.len(), 2);
    assert_eq!(find(&f[..1], EV_KEY, BTN_TOUCH), Some(0));
    assert_eq!(find(&f[1..], EV_KEY, BTN_TOOL_PEN), Some(0));
}

#[test]
fn predicted_samples_ignored_unless_enabled() {
    let mut m = mapper();
    let mut p = s(PenPhase::Hover, 0.5, 0.5, 0.0);
    p.flags |= sample_flags::PREDICTED;
    assert!(m.sample(&p).is_empty());
    let mut m = PenMapper::new(
        geo(),
        MapperConfig {
            use_predicted: true,
            min_contact_pressure: 0.0,
            ..Default::default()
        },
    );
    assert!(!m.sample(&p).is_empty());
}

#[test]
fn tilt_and_roll_axes_emitted() {
    let mut m = mapper();
    let mut x = s(PenPhase::Down, 0.5, 0.5, 0.5);
    x.altitude = FRAC_PI_4 as f32;
    x.azimuth = FRAC_PI_2 as f32;
    x.roll = FRAC_PI_2 as f32;
    let f = m.sample(&x);
    assert_eq!(find(&f, EV_ABS, ABS_TILT_X), Some(0));
    assert_eq!(find(&f, EV_ABS, ABS_TILT_Y), Some(45));
    assert_eq!(find(&f, EV_ABS, ABS_Z), Some(0)); // 90° roll, see roll test
                                                  // Without ROLL_VALID (non-Pro pencil) ABS_Z is left alone.
    let mut m = mapper();
    x.flags = 0;
    let f = m.sample(&x);
    assert_eq!(find(&f, EV_ABS, ABS_Z), None);
}

#[test]
fn area_maps_into_subrectangle() {
    let mut m = PenMapper::new(
        geo(),
        MapperConfig {
            area: [0.5, 0.0, 0.5, 1.0],
            min_contact_pressure: 0.0,
            ..Default::default()
        },
    );
    let f = m.sample(&s(PenPhase::Hover, 0.0, 0.0, 0.0));
    assert_eq!(find(&f, EV_ABS, ABS_X), Some(5000));
    let f = m.sample(&s(PenPhase::Hover, 1.0, 1.0, 0.0));
    assert_eq!(find(&f, EV_ABS, ABS_X), Some(10000));
    assert_eq!(find(&f, EV_ABS, ABS_Y), Some(5000));
    // Out-of-range input is clamped, never outside the device range.
    let f = m.sample(&s(PenPhase::Hover, 1.7, -0.3, 0.0));
    assert_eq!(find(&f, EV_ABS, ABS_Y), Some(0));
}

#[test]
fn min_contact_pressure_floor() {
    // Default config: feather-light touch still above libinput's 5% tip-down threshold.
    let mut m = PenMapper::new(geo(), MapperConfig::default());
    let f = m.sample(&s(PenPhase::Down, 0.5, 0.5, 0.0));
    let p = find(&f, EV_ABS, ABS_PRESSURE).unwrap();
    assert_eq!(
        p,
        (DEFAULT_MIN_CONTACT_PRESSURE * PRESSURE_MAX as f64).round() as i32
    );
    assert!(p as f64 >= 0.05 * PRESSURE_MAX as f64);
    let f = m.sample(&s(PenPhase::Move, 0.5, 0.5, 1.0));
    assert_eq!(find(&f, EV_ABS, ABS_PRESSURE), Some(PRESSURE_MAX));
}

#[test]
fn eraser_toggle_while_hovering_and_touching() {
    let mut m = mapper();
    m.sample(&s(PenPhase::Hover, 0.5, 0.5, 0.0));
    let (t, f) = m.toggle_eraser();
    assert_eq!(t, Tool::Eraser);
    assert_eq!(
        f,
        vec![
            vec![RawEvent::key(BTN_TOOL_PEN, false)],
            vec![RawEvent::key(BTN_TOOL_RUBBER, true)]
        ]
    );

    // While touching: deferred to the end of the stroke.
    m.sample(&s(PenPhase::Down, 0.5, 0.5, 0.5));
    let (t, f) = m.toggle_eraser();
    assert_eq!(t, Tool::Pen);
    assert!(f.is_empty());
    let f = m.sample(&s(PenPhase::Up, 0.5, 0.5, 0.0));
    let flat: Vec<RawEvent> = f.into_iter().flatten().collect();
    let touch_up = flat
        .iter()
        .position(|e| *e == RawEvent::key(BTN_TOUCH, false))
        .unwrap();
    let rubber_out = flat
        .iter()
        .position(|e| *e == RawEvent::key(BTN_TOOL_RUBBER, false))
        .unwrap();
    let pen_in = flat
        .iter()
        .position(|e| *e == RawEvent::key(BTN_TOOL_PEN, true))
        .unwrap();
    assert!(touch_up < rubber_out && rubber_out < pen_in);

    // Away from the screen: just remembered for the next proximity-in.
    m.sample(&s(PenPhase::Out, 0.5, 0.5, 0.0));
    let (t, f) = m.toggle_eraser();
    assert_eq!(t, Tool::Eraser);
    assert!(f.is_empty());
    let f = m.sample(&s(PenPhase::Hover, 0.5, 0.5, 0.0));
    assert_eq!(find(&f, EV_KEY, BTN_TOOL_RUBBER), Some(1));
}

#[test]
fn button_out_of_proximity_uses_synthetic_proximity() {
    let mut m = mapper();
    let f = m.click(BTN_STYLUS2, Some((0.25, 0.25, FRAC_PI_2, 0.0)));
    let flat: Vec<RawEvent> = f.iter().flatten().copied().collect();
    let order: Vec<u16> = flat
        .iter()
        .filter(|e| e.ty == EV_KEY)
        .map(|e| e.code)
        .collect();
    assert_eq!(
        order,
        vec![BTN_TOOL_PEN, BTN_STYLUS2, BTN_STYLUS2, BTN_TOOL_PEN]
    );
    assert_eq!(find(&f, EV_ABS, ABS_X), Some(2500));
    assert!(!m.in_proximity());

    // While hovering, no proximity games. Each button change is followed by
    // its own 1-unit X move so KWin sends the client a tablet frame (it
    // forwards buttons without one, and Qt only applies buttons on frames).
    m.sample(&s(PenPhase::Hover, 0.5, 0.5, 0.0));
    let f = m.click(BTN_STYLUS, None);
    assert_eq!(
        f,
        vec![
            vec![RawEvent::key(BTN_STYLUS, true)],
            vec![RawEvent::abs(ABS_X, 5001)],
            vec![RawEvent::key(BTN_STYLUS, false)],
            vec![RawEvent::abs(ABS_X, 5000)]
        ]
    );
    assert!(m.in_proximity());
}

#[test]
fn release_all_leaves_nothing_pressed() {
    let mut m = mapper();
    m.sample(&s(PenPhase::Down, 0.5, 0.5, 0.5));
    m.button(BTN_STYLUS, true, None);
    let f = m.release_all();
    let keys: Vec<RawEvent> = f
        .iter()
        .flatten()
        .filter(|e| e.ty == EV_KEY)
        .copied()
        .collect();
    assert!(keys.contains(&RawEvent::key(BTN_TOUCH, false)));
    assert!(keys.contains(&RawEvent::key(BTN_STYLUS, false)));
    assert_eq!(*keys.last().unwrap(), RawEvent::key(BTN_TOOL_PEN, false));
    assert!(m.release_all().is_empty(), "idempotent");
}

fn gesture(kind: GestureKind, phase: GesturePhase) -> PencilGesture {
    PencilGesture {
        timestamp_ns: 0,
        kind,
        phase,
        preferred_action: 0,
        hover_pose: Some(HoverPose {
            x: 0.5,
            y: 0.5,
            z_offset: 0.3,
            altitude: 1.2,
            azimuth: 0.0,
            roll: 0.0,
        }),
    }
}

#[test]
fn default_gestures_double_tap_eraser_squeeze_button2() {
    let cfg = GestureConfig::default();
    let mut m = mapper();
    m.sample(&s(PenPhase::Hover, 0.5, 0.5, 0.0));
    let out = gestures::handle(
        &cfg,
        &mut m,
        &gesture(GestureKind::DoubleTap, GesturePhase::Discrete),
    );
    assert_eq!(out.tool_changed, Some(Tool::Eraser));
    // Squeeze in click mode: nothing on began, click on ended.
    let out = gestures::handle(
        &cfg,
        &mut m,
        &gesture(GestureKind::Squeeze, GesturePhase::Began),
    );
    assert!(out.pen_frames.is_empty());
    let out = gestures::handle(
        &cfg,
        &mut m,
        &gesture(GestureKind::Squeeze, GesturePhase::Ended),
    );
    assert_eq!(
        out.pen_frames,
        vec![
            vec![RawEvent::key(BTN_STYLUS2, true)],
            vec![RawEvent::abs(ABS_X, 5001)],
            vec![RawEvent::key(BTN_STYLUS2, false)],
            vec![RawEvent::abs(ABS_X, 5000)]
        ]
    );
}

#[test]
fn squeeze_hold_mode_and_keys() {
    let cfg = GestureConfig {
        double_tap: Action::parse("keys:ctrl+z").unwrap(),
        squeeze: Action::Button(BTN_STYLUS),
        squeeze_mode: SqueezeMode::Hold,
    };
    let mut m = mapper();
    m.sample(&s(PenPhase::Hover, 0.5, 0.5, 0.0));
    let out = gestures::handle(
        &cfg,
        &mut m,
        &gesture(GestureKind::Squeeze, GesturePhase::Began),
    );
    assert_eq!(
        out.pen_frames,
        vec![
            vec![RawEvent::key(BTN_STYLUS, true)],
            vec![RawEvent::abs(ABS_X, 5001)]
        ]
    );
    let out = gestures::handle(
        &cfg,
        &mut m,
        &gesture(GestureKind::Squeeze, GesturePhase::Changed),
    );
    assert!(out.pen_frames.is_empty());
    let out = gestures::handle(
        &cfg,
        &mut m,
        &gesture(GestureKind::Squeeze, GesturePhase::Cancelled),
    );
    assert_eq!(
        out.pen_frames,
        vec![
            vec![RawEvent::key(BTN_STYLUS, false)],
            vec![RawEvent::abs(ABS_X, 5000)]
        ]
    );

    let out = gestures::handle(
        &cfg,
        &mut m,
        &gesture(GestureKind::DoubleTap, GesturePhase::Discrete),
    );
    let k: Vec<RawEvent> = out.key_frames.into_iter().flatten().collect();
    assert_eq!(
        k,
        vec![
            RawEvent::key(29, true),
            RawEvent::key(44, true),
            RawEvent::key(44, false),
            RawEvent::key(29, false)
        ]
    );
}
