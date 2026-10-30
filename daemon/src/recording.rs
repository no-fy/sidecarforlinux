//! Pen recordings (JSON Lines): captured from real sessions with
//! `sidecard run --record`, replayed by `sidecard replay` / the fake iPad,
//! and synthesised by `sidecard gen-demo` for tests.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sidecar_protocol::*;
use std::f32::consts::{FRAC_PI_2, PI};
use std::io::{BufRead, BufWriter, Write};
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecEvent {
    Surface {
        width_pt: f32,
        height_pt: f32,
        scale: f32,
        points_per_inch: f32,
    },
    Pen {
        /// Milliseconds since the start of the recording.
        t_ms: f64,
        phase: String,
        x: f32,
        y: f32,
        #[serde(default)]
        pressure: f32,
        altitude: f32,
        azimuth: f32,
        #[serde(default)]
        roll: Option<f32>,
        #[serde(default)]
        z: f32,
        #[serde(default)]
        predicted: bool,
    },
    Gesture {
        t_ms: f64,
        kind: String,
        phase: String,
        #[serde(default)]
        pose: Option<[f32; 6]>,
    },
}

impl RecEvent {
    pub fn t_ms(&self) -> f64 {
        match self {
            RecEvent::Surface { .. } => 0.0,
            RecEvent::Pen { t_ms, .. } | RecEvent::Gesture { t_ms, .. } => *t_ms,
        }
    }
}

pub fn phase_name(p: PenPhase) -> &'static str {
    match p {
        PenPhase::Hover => "hover",
        PenPhase::Down => "down",
        PenPhase::Move => "move",
        PenPhase::Up => "up",
        PenPhase::Cancel => "cancel",
        PenPhase::Out => "out",
    }
}

pub fn parse_phase(s: &str) -> Result<PenPhase> {
    Ok(match s {
        "hover" => PenPhase::Hover,
        "down" => PenPhase::Down,
        "move" => PenPhase::Move,
        "up" => PenPhase::Up,
        "cancel" => PenPhase::Cancel,
        "out" => PenPhase::Out,
        _ => bail!("unknown pen phase {s:?}"),
    })
}

fn gesture_names(g: &PencilGesture) -> (&'static str, &'static str) {
    let k = match g.kind {
        GestureKind::DoubleTap => "double_tap",
        GestureKind::Squeeze => "squeeze",
    };
    let p = match g.phase {
        GesturePhase::Discrete => "discrete",
        GesturePhase::Began => "began",
        GesturePhase::Changed => "changed",
        GesturePhase::Ended => "ended",
        GesturePhase::Cancelled => "cancelled",
    };
    (k, p)
}

impl RecEvent {
    pub fn from_sample(s: &PenSample, t_ms: f64) -> RecEvent {
        RecEvent::Pen {
            t_ms,
            phase: phase_name(s.phase).into(),
            x: s.x,
            y: s.y,
            pressure: s.pressure,
            altitude: s.altitude,
            azimuth: s.azimuth,
            roll: s.has(sample_flags::ROLL_VALID).then_some(s.roll),
            z: s.z_offset,
            predicted: s.has(sample_flags::PREDICTED),
        }
    }

    /// Converts a pen event to a protocol sample with the given clock.
    pub fn to_sample(&self, timestamp_ns: u64, seq: u32) -> Result<Option<PenSample>> {
        let RecEvent::Pen {
            phase,
            x,
            y,
            pressure,
            altitude,
            azimuth,
            roll,
            z,
            predicted,
            ..
        } = self
        else {
            return Ok(None);
        };
        let mut flags = 0;
        if roll.is_some() {
            flags |= sample_flags::ROLL_VALID;
        }
        if *predicted {
            flags |= sample_flags::PREDICTED;
        }
        let phase = parse_phase(phase)?;
        if phase == PenPhase::Hover {
            flags |= sample_flags::FROM_HOVER;
        }
        Ok(Some(PenSample {
            timestamp_ns,
            seq,
            phase,
            flags,
            x: *x,
            y: *y,
            pressure: *pressure,
            altitude: *altitude,
            azimuth: *azimuth,
            roll: roll.unwrap_or(0.0),
            z_offset: *z,
            estimation_index: 0,
        }))
    }

    pub fn to_gesture(&self, timestamp_ns: u64) -> Result<Option<PencilGesture>> {
        let RecEvent::Gesture {
            kind, phase, pose, ..
        } = self
        else {
            return Ok(None);
        };
        let kind = match kind.as_str() {
            "double_tap" => GestureKind::DoubleTap,
            "squeeze" => GestureKind::Squeeze,
            k => bail!("unknown gesture kind {k:?}"),
        };
        let phase = match phase.as_str() {
            "discrete" => GesturePhase::Discrete,
            "began" => GesturePhase::Began,
            "changed" => GesturePhase::Changed,
            "ended" => GesturePhase::Ended,
            "cancelled" => GesturePhase::Cancelled,
            p => bail!("unknown gesture phase {p:?}"),
        };
        Ok(Some(PencilGesture {
            timestamp_ns,
            kind,
            phase,
            preferred_action: 0,
            hover_pose: pose.map(|p| HoverPose {
                x: p[0],
                y: p[1],
                z_offset: p[2],
                altitude: p[3],
                azimuth: p[4],
                roll: p[5],
            }),
        }))
    }
}

pub fn read(path: &Path) -> Result<Vec<RecEvent>> {
    let f = std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut out = Vec::new();
    for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = line?;
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        out.push(serde_json::from_str(t).with_context(|| format!("{}:{}", path.display(), i + 1))?);
    }
    Ok(out)
}

pub fn write_all(events: &[RecEvent], w: &mut dyn Write) -> Result<()> {
    for e in events {
        serde_json::to_writer(&mut *w, e)?;
        w.write_all(b"\n")?;
    }
    Ok(())
}

/// Appends events from a live session to a JSONL file.
pub struct Recorder {
    out: BufWriter<std::fs::File>,
    t0: Option<u64>,
}

impl Recorder {
    pub fn create(path: &Path) -> Result<Recorder> {
        let f =
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
        Ok(Recorder {
            out: BufWriter::new(f),
            t0: None,
        })
    }
    fn t_ms(&mut self, ts: u64) -> f64 {
        let t0 = *self.t0.get_or_insert(ts);
        ts.saturating_sub(t0) as f64 / 1e6
    }
    fn put(&mut self, e: &RecEvent) -> Result<()> {
        serde_json::to_writer(&mut self.out, e)?;
        self.out.write_all(b"\n")?;
        Ok(())
    }
    pub fn surface(&mut self, s: &SurfaceInfo) -> Result<()> {
        self.put(&RecEvent::Surface {
            width_pt: s.width_pt,
            height_pt: s.height_pt,
            scale: s.scale,
            points_per_inch: s.points_per_inch,
        })
    }
    pub fn pen(&mut self, s: &PenSample) -> Result<()> {
        let t = self.t_ms(s.timestamp_ns);
        self.put(&RecEvent::from_sample(s, t))
    }
    pub fn gesture(&mut self, g: &PencilGesture) -> Result<()> {
        let t = self.t_ms(g.timestamp_ns);
        let (k, p) = gesture_names(g);
        let e = RecEvent::Gesture {
            t_ms: t,
            kind: k.into(),
            phase: p.into(),
            pose: g
                .hover_pose
                .map(|h| [h.x, h.y, h.z_offset, h.altitude, h.azimuth, h.roll]),
        };
        self.put(&e)?;
        self.out.flush()?;
        Ok(())
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.out.flush();
    }
}

/// Synthetic session exercising every signal: hover approach, a stroke with
/// pressure ramp + tilt sweep + full barrel roll, lift, hover away,
/// double-tap and a squeeze, at 240 Hz.
pub fn demo() -> Vec<RecEvent> {
    let mut ev = vec![RecEvent::Surface {
        width_pt: 1180.0,
        height_pt: 820.0,
        scale: 2.0,
        points_per_inch: 132.0,
    }];
    let dt = 1000.0 / 240.0;
    let mut t = 0.0;
    let pen =
        |t: f64, phase: &str, x: f32, y: f32, p: f32, alt: f32, az: f32, roll: f32, z: f32| {
            RecEvent::Pen {
                t_ms: t,
                phase: phase.into(),
                x,
                y,
                pressure: p,
                altitude: alt,
                azimuth: az,
                roll: Some(roll),
                z,
                predicted: false,
            }
        };
    // Hover approach: 0.25 s, z from 1 to 0.05.
    for i in 0..60 {
        let k = i as f32 / 59.0;
        ev.push(pen(
            t,
            "hover",
            0.2,
            0.5,
            0.0,
            1.0,
            FRAC_PI_2 * 0.5,
            0.0,
            1.0 - 0.95 * k,
        ));
        t += dt;
    }
    // Stroke: 1 s, S-curve left to right.
    let n = 240;
    for i in 0..=n {
        let k = i as f32 / n as f32;
        let x = 0.2 + 0.6 * k;
        let y = 0.5 + 0.2 * (k * 2.0 * PI).sin();
        let p = (k * PI).sin().max(0.02); // 0 → 1 → 0
        let alt = FRAC_PI_2 - k * (FRAC_PI_2 - 0.5); // vertical → 0.5 rad
        let az = -PI + 2.0 * PI * k; // full sweep
        let roll = -PI + 2.0 * PI * k;
        let phase = match i {
            0 => "down",
            i if i == n => "up",
            _ => "move",
        };
        ev.push(pen(
            t,
            phase,
            x,
            y,
            if phase == "up" { 0.0 } else { p },
            alt,
            az,
            roll,
            0.0,
        ));
        t += dt;
    }
    // Hover away and leave.
    for i in 0..60 {
        let k = i as f32 / 59.0;
        ev.push(pen(
            t,
            "hover",
            0.8,
            0.5,
            0.0,
            1.2,
            0.3,
            0.0,
            0.05 + 0.95 * k,
        ));
        t += dt;
    }
    ev.push(pen(t, "out", 0.8, 0.5, 0.0, 1.2, 0.3, 0.0, 1.0));
    t += 200.0;
    let pose = Some([0.5, 0.5, 0.4, 1.1, 0.2, 0.0]);
    ev.push(RecEvent::Gesture {
        t_ms: t,
        kind: "double_tap".into(),
        phase: "discrete".into(),
        pose,
    });
    t += 300.0;
    ev.push(RecEvent::Gesture {
        t_ms: t,
        kind: "squeeze".into(),
        phase: "began".into(),
        pose: None,
    });
    t += 150.0;
    ev.push(RecEvent::Gesture {
        t_ms: t,
        kind: "squeeze".into(),
        phase: "ended".into(),
        pose: None,
    });
    t += 200.0;
    // Second, short eraser stroke (double-tap toggled to the eraser).
    for i in 0..=48 {
        let k = i as f32 / 48.0;
        let phase = match i {
            0 => "down",
            48 => "up",
            _ => "move",
        };
        ev.push(pen(
            t,
            phase,
            0.3 + 0.3 * k,
            0.3,
            if i == 48 { 0.0 } else { 0.6 },
            1.0,
            0.8,
            0.0,
            0.0,
        ));
        t += dt;
    }
    ev.push(pen(t, "out", 0.6, 0.3, 0.0, 1.0, 0.8, 0.0, 1.0));
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_roundtrips_through_jsonl() {
        let d = demo();
        let mut buf = Vec::new();
        write_all(&d, &mut buf).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("demo.jsonl");
        std::fs::write(&p, &buf).unwrap();
        assert_eq!(read(&p).unwrap(), d);
        let s = d[1].to_sample(5, 1).unwrap().unwrap();
        assert_eq!(s.phase, PenPhase::Hover);
        assert!(s.has(sample_flags::ROLL_VALID));
    }
}
