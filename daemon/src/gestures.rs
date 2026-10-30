//! Pencil double-tap / squeeze → configurable actions.

use crate::events::*;
use crate::keys::parse_key_combo;
use crate::tablet::{Frame, PenMapper, Tool};
use anyhow::{bail, Result};
use sidecar_protocol::{GestureKind, GesturePhase, PencilGesture};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    /// Toggle BTN_TOOL_PEN ↔ BTN_TOOL_RUBBER (Krita & co. switch to their
    /// eraser preset automatically), like the iPadOS default for double-tap.
    ToggleEraser,
    /// A stylus barrel button (BTN_STYLUS / BTN_STYLUS2 / BTN_STYLUS3).
    Button(u16),
    /// A keyboard shortcut on the companion keyboard device.
    Keys(Vec<u16>),
}

impl Action {
    /// Parses `none`, `eraser`, `stylus`, `stylus2`, `stylus3`,
    /// `keys:CTRL+Z` (any `KEY_*` name, case-insensitive, `KEY_` optional).
    pub fn parse(s: &str) -> Result<Action> {
        let t = s.trim();
        Ok(match t.to_ascii_lowercase().as_str() {
            "none" | "" => Action::None,
            "eraser" | "toggle-eraser" => Action::ToggleEraser,
            "stylus" | "stylus1" | "button1" => Action::Button(BTN_STYLUS),
            "stylus2" | "button2" => Action::Button(BTN_STYLUS2),
            "stylus3" | "button3" => Action::Button(BTN_STYLUS3),
            l if l.starts_with("keys:") => Action::Keys(parse_key_combo(&t[5..])?),
            _ => bail!("unknown pencil action {s:?} (none|eraser|stylus|stylus2|stylus3|keys:CTRL+Z)"),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqueezeMode {
    /// Fire once when the squeeze is released (Apple's recommendation).
    Click,
    /// Hold the button/keys for as long as the pencil is squeezed.
    Hold,
}

#[derive(Debug, Clone)]
pub struct GestureConfig {
    pub double_tap: Action,
    pub squeeze: Action,
    pub squeeze_mode: SqueezeMode,
}

impl Default for GestureConfig {
    fn default() -> Self {
        GestureConfig {
            double_tap: Action::ToggleEraser,
            squeeze: Action::Button(BTN_STYLUS2),
            squeeze_mode: SqueezeMode::Click,
        }
    }
}

/// Result of handling a gesture.
#[derive(Debug, Default)]
pub struct GestureOutput {
    pub pen_frames: Vec<Frame>,
    pub key_frames: Vec<Frame>,
    /// Tool changed → the iPad should play a haptic.
    pub tool_changed: Option<Tool>,
}

fn key_frames(keys: &[u16], down: bool) -> Vec<Frame> {
    if down {
        keys.iter().map(|k| vec![RawEvent::key(*k, true)]).collect()
    } else {
        keys.iter().rev().map(|k| vec![RawEvent::key(*k, false)]).collect()
    }
}

pub fn handle(cfg: &GestureConfig, pen: &mut PenMapper, g: &PencilGesture) -> GestureOutput {
    let mut out = GestureOutput::default();
    let at = g
        .hover_pose
        .map(|p| (p.x as f64, p.y as f64, p.altitude as f64, p.azimuth as f64));
    let (action, fire_down, fire_up) = match g.kind {
        GestureKind::DoubleTap => (&cfg.double_tap, true, true),
        GestureKind::Squeeze => match (cfg.squeeze_mode, g.phase) {
            (SqueezeMode::Click, GesturePhase::Ended | GesturePhase::Discrete) => (&cfg.squeeze, true, true),
            (SqueezeMode::Click, _) => return out,
            (SqueezeMode::Hold, GesturePhase::Began) => (&cfg.squeeze, true, false),
            (SqueezeMode::Hold, GesturePhase::Ended | GesturePhase::Cancelled) => (&cfg.squeeze, false, true),
            (SqueezeMode::Hold, _) => return out,
        },
    };
    match action {
        Action::None => {}
        Action::ToggleEraser => {
            // Toggle on press only, so a held squeeze doesn't double toggle.
            if fire_down {
                let (tool, frames) = pen.toggle_eraser();
                out.pen_frames = frames;
                out.tool_changed = Some(tool);
            }
        }
        Action::Button(code) => {
            if fire_down {
                out.pen_frames.extend(pen.button(*code, true, at));
            }
            if fire_up {
                out.pen_frames.extend(pen.button(*code, false, at));
            }
        }
        Action::Keys(keys) => {
            if fire_down {
                out.key_frames.extend(key_frames(keys, true));
            }
            if fire_up {
                out.key_frames.extend(key_frames(keys, false));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_actions() {
        assert_eq!(Action::parse("eraser").unwrap(), Action::ToggleEraser);
        assert_eq!(Action::parse("stylus2").unwrap(), Action::Button(BTN_STYLUS2));
        assert_eq!(Action::parse("none").unwrap(), Action::None);
        assert_eq!(Action::parse("keys:ctrl+z").unwrap(), Action::Keys(vec![29, 44]));
        assert!(Action::parse("explode").is_err());
    }
}
