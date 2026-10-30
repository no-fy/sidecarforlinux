//! Pen sample → evdev event mapping (the heart of phase 1).
//!
//! Pure logic: `PenMapper` turns protocol samples and pencil gestures into
//! frames of evdev events (each frame is terminated by SYN_REPORT by the
//! sink). Device creation lives in `uinput.rs`.

use crate::events::*;
use sidecar_protocol::{sample_flags, PenPhase, PenSample, SurfaceInfo};
use std::f64::consts::PI;

/// ABS_PRESSURE range: 0..=PRESSURE_MAX (8192 levels; spec asks for ≥ 4096).
pub const PRESSURE_MAX: i32 = 8191;
/// ABS_TILT_X/Y are in degrees, -90..=90.
pub const TILT_MAX_DEG: i32 = 90;
/// Resolution of the tilt axes in units per radian (kernel convention; the
/// Wacom driver uses 57 too). 1 unit ≈ 1°.
pub const TILT_RESOLUTION: i32 = 57;
/// ABS_Z (barrel roll): tenths of a degree in [-1800, 1799].
pub const ROLL_MIN: i32 = -1800;
pub const ROLL_MAX: i32 = 1799;
/// ABS_DISTANCE: hover height, 0 = touching, DISTANCE_MAX = top of hover range.
pub const DISTANCE_MAX: i32 = 1023;
/// ABS_X/ABS_Y resolution in units per millimetre.
pub const XY_RESOLUTION: i32 = 100;

/// Coordinate space of the virtual tablet, derived from the iPad surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabletGeometry {
    pub x_max: i32,
    pub y_max: i32,
    /// Units per millimetre on both axes.
    pub resolution: i32,
}

impl TabletGeometry {
    pub fn from_surface(s: &SurfaceInfo) -> Self {
        let ppi = if s.points_per_inch > 1.0 { s.points_per_inch } else { 132.0 };
        let w_mm = s.width_pt as f64 / ppi as f64 * 25.4;
        let h_mm = s.height_pt as f64 / ppi as f64 * 25.4;
        TabletGeometry {
            x_max: ((w_mm * XY_RESOLUTION as f64).round() as i32).max(1),
            y_max: ((h_mm * XY_RESOLUTION as f64).round() as i32).max(1),
            resolution: XY_RESOLUTION,
        }
    }
}

/// Converts Apple's altitude/azimuth (radians, azimuth in view coordinates
/// with y pointing down) into Linux tilt angles in degrees.
///
/// The pencil axis, from tip to cap, is
/// `v = (cos(alt)·cos(az), cos(alt)·sin(az), sin(alt))`. Linux tilt X is the
/// angle of `v` from the screen normal in the x/z plane (positive = cap leans
/// to +x, right), tilt Y likewise in the y/z plane (positive = towards the
/// bottom / the user):
///
/// `tiltX = atan(cos(az)/tan(alt))`, `tiltY = atan(sin(az)/tan(alt))`.
///
/// We use `atan2` so alt → 0 gives ±90° instead of dividing by zero.
pub fn tilt_degrees(altitude: f64, azimuth: f64) -> (f64, f64) {
    let (s, c) = altitude.sin_cos();
    let tx = (azimuth.cos() * c).atan2(s).to_degrees();
    let ty = (azimuth.sin() * c).atan2(s).to_degrees();
    (tx.clamp(-90.0, 90.0), ty.clamp(-90.0, 90.0))
}

/// Barrel roll radians → ABS_Z value (tenths of a degree, wrapped to
/// [-180°, 180°)).
pub fn roll_to_abs(roll: f64) -> i32 {
    let mut deg = roll.to_degrees() % 360.0;
    if deg >= 180.0 {
        deg -= 360.0;
    } else if deg < -180.0 {
        deg += 360.0;
    }
    ((deg * 10.0).round() as i32).clamp(ROLL_MIN, ROLL_MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Eraser,
}

impl Tool {
    pub fn code(self) -> u16 {
        match self {
            Tool::Pen => BTN_TOOL_PEN,
            Tool::Eraser => BTN_TOOL_RUBBER,
        }
    }
}

#[derive(Debug, Clone)]
pub struct MapperConfig {
    /// Sub-rectangle of the tablet's coordinate space the iPad surface maps
    /// to, normalised (x, y, w, h). Whatever the compositor maps the whole
    /// tablet to (one output, or the whole workspace), the iPad covers this
    /// part of it.
    pub area: [f64; 4],
    /// Inject predicted touches (they can't be taken back; off by default).
    pub use_predicted: bool,
    /// While the tip touches, pressure never drops below this fraction of
    /// the range, so a feather-light touch is still seen as contact by
    /// consumers that derive contact from pressure (libinput does).
    pub min_contact_pressure: f64,
}

impl Default for MapperConfig {
    fn default() -> Self {
        MapperConfig { area: [0.0, 0.0, 1.0, 1.0], use_predicted: false, min_contact_pressure: 0.0 }
    }
}

pub type Frame = Vec<RawEvent>;

/// Tracks pen state and emits minimal, well-ordered evdev frames.
pub struct PenMapper {
    geo: TabletGeometry,
    cfg: MapperConfig,
    tool: Tool,
    pending_tool: Option<Tool>,
    in_prox: bool,
    touching: bool,
    buttons: [(u16, bool); 3],
    last_abs: [Option<i32>; 8],
    last_pos: Option<(f64, f64)>,
    /// We forced proximity-in only to deliver a button press.
    synthetic_prox: bool,
}

const ABS_SLOTS: [u16; 7] = [ABS_X, ABS_Y, ABS_PRESSURE, ABS_DISTANCE, ABS_TILT_X, ABS_TILT_Y, ABS_Z];

fn slot(code: u16) -> usize {
    ABS_SLOTS.iter().position(|c| *c == code).unwrap()
}

impl PenMapper {
    pub fn new(geo: TabletGeometry, cfg: MapperConfig) -> Self {
        PenMapper {
            geo,
            cfg,
            tool: Tool::Pen,
            pending_tool: None,
            in_prox: false,
            touching: false,
            buttons: [(BTN_STYLUS, false), (BTN_STYLUS2, false), (BTN_STYLUS3, false)],
            last_abs: [None; 8],
            last_pos: None,
            synthetic_prox: false,
        }
    }

    pub fn geometry(&self) -> TabletGeometry {
        self.geo
    }
    pub fn tool(&self) -> Tool {
        self.pending_tool.unwrap_or(self.tool)
    }
    pub fn in_proximity(&self) -> bool {
        self.in_prox
    }
    pub fn touching(&self) -> bool {
        self.touching
    }
    pub fn last_position(&self) -> Option<(f64, f64)> {
        self.last_pos
    }

    /// Forget cached axis values (e.g. after the uinput device is recreated).
    pub fn reset_cache(&mut self) {
        self.last_abs = [None; 8];
    }

    fn push_abs(&mut self, f: &mut Frame, code: u16, value: i32) {
        let s = slot(code);
        if self.last_abs[s] != Some(value) {
            self.last_abs[s] = Some(value);
            f.push(RawEvent::abs(code, value));
        }
    }

    fn pos_to_abs(&self, x: f64, y: f64) -> (i32, i32) {
        let [ax, ay, aw, ah] = self.cfg.area;
        let nx = (ax + x.clamp(0.0, 1.0) * aw).clamp(0.0, 1.0);
        let ny = (ay + y.clamp(0.0, 1.0) * ah).clamp(0.0, 1.0);
        (
            (nx * self.geo.x_max as f64).round() as i32,
            (ny * self.geo.y_max as f64).round() as i32,
        )
    }

    fn pressure_to_abs(&self, p: f64, contact: bool) -> i32 {
        if !contact {
            return 0;
        }
        let floor = self.cfg.min_contact_pressure.clamp(0.0, 1.0);
        let p = p.clamp(0.0, 1.0);
        let v = floor + p * (1.0 - floor);
        ((v * PRESSURE_MAX as f64).round() as i32).clamp(if floor > 0.0 { 1 } else { 0 }, PRESSURE_MAX)
    }

    /// Position + angle axes shared by every sample type.
    fn push_pose(&mut self, f: &mut Frame, x: f64, y: f64, alt: f64, az: f64, roll: Option<f64>) {
        let (ax, ay) = self.pos_to_abs(x, y);
        self.push_abs(f, ABS_X, ax);
        self.push_abs(f, ABS_Y, ay);
        let (tx, ty) = tilt_degrees(alt, az);
        self.push_abs(f, ABS_TILT_X, tx.round() as i32);
        self.push_abs(f, ABS_TILT_Y, ty.round() as i32);
        if let Some(r) = roll {
            self.push_abs(f, ABS_Z, roll_to_abs(r));
        }
        self.last_pos = Some((x, y));
    }

    fn prox_in(&mut self, frames: &mut Vec<Frame>, mut f: Frame) {
        if let Some(t) = self.pending_tool.take() {
            self.tool = t;
        }
        f.push(RawEvent::key(self.tool.code(), true));
        self.in_prox = true;
        frames.push(f);
    }

    fn tip_up(&mut self, frames: &mut Vec<Frame>) {
        if self.touching {
            let mut f = Frame::new();
            self.push_abs(&mut f, ABS_PRESSURE, 0);
            f.push(RawEvent::key(BTN_TOUCH, false));
            self.touching = false;
            frames.push(f);
        }
    }

    fn prox_out(&mut self, frames: &mut Vec<Frame>) {
        self.tip_up(frames);
        let mut f = Frame::new();
        for i in 0..self.buttons.len() {
            if self.buttons[i].1 {
                self.buttons[i].1 = false;
                f.push(RawEvent::key(self.buttons[i].0, false));
            }
        }
        if self.in_prox {
            self.push_abs(&mut f, ABS_DISTANCE, DISTANCE_MAX);
            f.push(RawEvent::key(self.tool.code(), false));
            self.in_prox = false;
        }
        if !f.is_empty() {
            frames.push(f);
        }
    }

    /// Map one pen sample. Returns the frames to emit (possibly none).
    pub fn sample(&mut self, s: &PenSample) -> Vec<Frame> {
        let mut frames = Vec::new();
        if s.has(sample_flags::PREDICTED) && !self.cfg.use_predicted {
            return frames;
        }
        let roll = s.has(sample_flags::ROLL_VALID).then_some(s.roll as f64);
        let (x, y, alt, az) = (s.x as f64, s.y as f64, s.altitude as f64, s.azimuth as f64);
        if s.phase != PenPhase::Out {
            // A real sample: proximity is genuine from now on.
            self.synthetic_prox = false;
        }
        match s.phase {
            PenPhase::Hover => {
                self.tip_up(&mut frames);
                let mut f = Frame::new();
                self.push_pose(&mut f, x, y, alt, az, roll);
                let d = ((s.z_offset as f64).clamp(0.0, 1.0) * DISTANCE_MAX as f64).round() as i32;
                self.push_abs(&mut f, ABS_DISTANCE, d);
                self.push_abs(&mut f, ABS_PRESSURE, 0);
                if self.in_prox {
                    if !f.is_empty() {
                        frames.push(f);
                    }
                } else {
                    self.prox_in(&mut frames, f);
                }
            }
            PenPhase::Down | PenPhase::Move => {
                if !self.in_prox {
                    // Pencil arrived without hover (hover unsupported, or it
                    // was too fast): proximity-in at the contact point first.
                    let mut f = Frame::new();
                    self.push_pose(&mut f, x, y, alt, az, roll);
                    self.push_abs(&mut f, ABS_DISTANCE, 0);
                    self.prox_in(&mut frames, f);
                }
                let mut f = Frame::new();
                self.push_pose(&mut f, x, y, alt, az, roll);
                self.push_abs(&mut f, ABS_DISTANCE, 0);
                let p = self.pressure_to_abs(s.pressure as f64, true);
                self.push_abs(&mut f, ABS_PRESSURE, p);
                if !self.touching {
                    f.push(RawEvent::key(BTN_TOUCH, true));
                    self.touching = true;
                }
                if !f.is_empty() {
                    frames.push(f);
                }
            }
            PenPhase::Up | PenPhase::Cancel => {
                if self.in_prox {
                    let mut f = Frame::new();
                    self.push_pose(&mut f, x, y, alt, az, roll);
                    if !f.is_empty() {
                        frames.push(f);
                    }
                }
                self.tip_up(&mut frames);
                self.apply_pending_tool(&mut frames);
            }
            PenPhase::Out => {
                self.prox_out(&mut frames);
            }
        }
        frames
    }

    fn apply_pending_tool(&mut self, frames: &mut Vec<Frame>) {
        if let Some(t) = self.pending_tool {
            if t != self.tool && self.in_prox && !self.touching {
                self.pending_tool = None;
                let mut f = Frame::new();
                f.push(RawEvent::key(self.tool.code(), false));
                frames.push(f);
                self.tool = t;
                frames.push(vec![RawEvent::key(t.code(), true)]);
            }
        }
    }

    /// Switch between pen and eraser. Takes effect immediately when the pen
    /// is hovering, at the end of the stroke when touching, or at the next
    /// proximity-in when away.
    pub fn set_tool(&mut self, t: Tool) -> Vec<Frame> {
        let mut frames = Vec::new();
        if !self.in_prox {
            self.tool = t;
            self.pending_tool = None;
            return frames;
        }
        self.pending_tool = Some(t);
        if !self.touching {
            self.apply_pending_tool(&mut frames);
        }
        frames
    }

    pub fn toggle_eraser(&mut self) -> (Tool, Vec<Frame>) {
        let next = match self.tool() {
            Tool::Pen => Tool::Eraser,
            Tool::Eraser => Tool::Pen,
        };
        (next, self.set_tool(next))
    }

    /// Press or release a stylus button. Buttons are only meaningful while
    /// the tool is in proximity (libinput drops them otherwise), so if the
    /// pen is away we bring it into proximity at `at` (or the last known
    /// position) for the duration of the press.
    pub fn button(&mut self, code: u16, down: bool, at: Option<(f64, f64, f64, f64)>) -> Vec<Frame> {
        let mut frames = Vec::new();
        let i = match self.buttons.iter().position(|(c, _)| *c == code) {
            Some(i) => i,
            None => return frames,
        };
        if self.buttons[i].1 == down {
            return frames;
        }
        if down && !self.in_prox {
            let (x, y, alt, az) = at
                .or_else(|| self.last_pos.map(|(x, y)| (x, y, PI / 2.0, 0.0)))
                .unwrap_or((0.5, 0.5, PI / 2.0, 0.0));
            let mut f = Frame::new();
            self.push_pose(&mut f, x, y, alt, az, None);
            self.push_abs(&mut f, ABS_DISTANCE, DISTANCE_MAX);
            self.prox_in(&mut frames, f);
            self.synthetic_prox = true;
        }
        self.buttons[i].1 = down;
        frames.push(vec![RawEvent::key(code, down)]);
        if !down && self.synthetic_prox && !self.buttons.iter().any(|b| b.1) {
            self.synthetic_prox = false;
            self.prox_out(&mut frames);
        }
        frames
    }

    /// Click = press + release.
    pub fn click(&mut self, code: u16, at: Option<(f64, f64, f64, f64)>) -> Vec<Frame> {
        let mut frames = self.button(code, true, at);
        frames.extend(self.button(code, false, at));
        frames
    }

    /// Release everything (connection lost, daemon stopping).
    pub fn release_all(&mut self) -> Vec<Frame> {
        let mut frames = Vec::new();
        self.synthetic_prox = false;
        self.prox_out(&mut frames);
        frames
    }
}
