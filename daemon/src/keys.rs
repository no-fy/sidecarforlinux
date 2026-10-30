//! Keyboard shortcut parsing for the companion keyboard device.

use anyhow::{anyhow, Result};
use std::str::FromStr;

/// Parses "CTRL+SHIFT+Z" / "KEY_LEFTCTRL+KEY_Z" into Linux key codes.
pub fn parse_key_combo(s: &str) -> Result<Vec<u16>> {
    let mut out = Vec::new();
    for part in s.split('+').map(str::trim).filter(|p| !p.is_empty()) {
        out.push(parse_key(part)?);
    }
    if out.is_empty() {
        return Err(anyhow!("empty key combo"));
    }
    Ok(out)
}

pub fn parse_key(name: &str) -> Result<u16> {
    let up = name.to_ascii_uppercase();
    let alias = match up.as_str() {
        "CTRL" | "CONTROL" => "KEY_LEFTCTRL",
        "SHIFT" => "KEY_LEFTSHIFT",
        "ALT" => "KEY_LEFTALT",
        "META" | "SUPER" | "WIN" => "KEY_LEFTMETA",
        "ALTGR" => "KEY_RIGHTALT",
        "ESC" => "KEY_ESC",
        "DEL" => "KEY_DELETE",
        "[" => "KEY_LEFTBRACE",
        "]" => "KEY_RIGHTBRACE",
        _ => "",
    };
    let full = if !alias.is_empty() {
        alias.to_string()
    } else if up.starts_with("KEY_") || up.starts_with("BTN_") {
        up.clone()
    } else {
        format!("KEY_{up}")
    };
    evdev::KeyCode::from_str(&full)
        .map(|k| k.code())
        .map_err(|_| anyhow!("unknown key {name:?} (tried {full})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn combos() {
        assert_eq!(parse_key_combo("ctrl+z").unwrap(), vec![29, 44]);
        assert_eq!(parse_key_combo("KEY_LEFTSHIFT + KEY_E").unwrap(), vec![42, 18]);
        assert_eq!(parse_key_combo("ctrl+]").unwrap(), vec![29, 27]);
        assert!(parse_key_combo("ctrl+nosuchkey").is_err());
    }
}
