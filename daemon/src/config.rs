//! `~/.config/sidecarforlinux/config.toml`.

use crate::gestures::{Action, GestureConfig, SqueezeMode};
use crate::keys::parse_key_combo;
use crate::tablet::{MapperConfig, DEFAULT_MIN_CONTACT_PRESSURE};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const EXAMPLE: &str = r#"# sidecarforlinux daemon configuration
# Every key is optional; these are the defaults.

[connection]
# auto = USB first, then Wi-Fi. Also: usb, wifi, tcp (needs `address`).
transport = "auto"
# Pick a specific iPad by UDID when several are plugged in.
udid = ""
address = ""
port = 47811

[tablet]
# Part of the tablet's coordinate space the iPad surface covers, normalised
# x, y, width, height. The compositor decides what the whole tablet maps to
# (KDE: System Settings > Drawing Tablet, or [kde] output below).
area = [0.0, 0.0, 1.0, 1.0]
# Advertise a screen tablet (INPUT_PROP_DIRECT) like a Cintiq.
direct = true
# Inject UIKit predicted touches (lower apparent latency, may overshoot).
use_predicted = false
# Pressure floor while touching (fraction). libinput needs >= 5% to see contact.
min_contact_pressure = 0.06

[pencil]
# none | eraser | stylus | stylus2 | stylus3 | keys:CTRL+Z
double_tap = "eraser"
squeeze = "stylus2"
# click = fire when released; hold = pressed while squeezing
squeeze_mode = "click"
# Buzz the Pencil Pro when the tool changes (pen <-> eraser).
haptic_on_tool_switch = true

[kde]
# Map the pen to this output via KWin's D-Bus API (e.g. "DP-1",
# "Virtual-ipad"). Empty = leave KDE's own tablet settings alone.
output = ""

[video]
# Phase 2 (experimental): off | virtual (KWin virtual monitor sized like the
# iPad) | test-pattern | file:/path/to/clip.h264
mode = "off"
# vaapi (GPU, Mesa) | x264 (software)
encoder = "vaapi"
bitrate_kbps = 20000
fps = 60
"#;

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub connection: ConnectionCfg,
    pub tablet: TabletCfg,
    pub pencil: PencilCfg,
    pub kde: KdeCfg,
    pub video: VideoCfg,
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoCfg {
    pub mode: String,
    pub encoder: String,
    pub bitrate_kbps: u32,
    pub fps: u32,
}

impl Default for VideoCfg {
    fn default() -> Self {
        VideoCfg {
            mode: "off".into(),
            encoder: "vaapi".into(),
            bitrate_kbps: 20000,
            fps: 60,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectionCfg {
    pub transport: String,
    pub udid: String,
    pub address: String,
    pub port: u16,
}

impl Default for ConnectionCfg {
    fn default() -> Self {
        ConnectionCfg {
            transport: "auto".into(),
            udid: String::new(),
            address: String::new(),
            port: sidecar_protocol::DEFAULT_PORT,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TabletCfg {
    pub area: [f64; 4],
    pub direct: bool,
    pub use_predicted: bool,
    pub min_contact_pressure: f64,
}

impl Default for TabletCfg {
    fn default() -> Self {
        TabletCfg {
            area: [0.0, 0.0, 1.0, 1.0],
            direct: true,
            use_predicted: false,
            min_contact_pressure: DEFAULT_MIN_CONTACT_PRESSURE,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PencilCfg {
    pub double_tap: String,
    pub squeeze: String,
    pub squeeze_mode: String,
    pub haptic_on_tool_switch: bool,
}

impl Default for PencilCfg {
    fn default() -> Self {
        PencilCfg {
            double_tap: "eraser".into(),
            squeeze: "stylus2".into(),
            squeeze_mode: "click".into(),
            haptic_on_tool_switch: true,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct KdeCfg {
    pub output: String,
}

pub fn config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("SIDECAR_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    directories::ProjectDirs::from("", "", "sidecarforlinux")
        .map(|p| p.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".sidecarforlinux"))
}

impl Config {
    pub fn load(path: &Path) -> Result<Config> {
        if !path.exists() {
            return Ok(Config::default());
        }
        let text = std::fs::read_to_string(path)?;
        let c: Config =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        c.validate()?;
        Ok(c)
    }

    pub fn validate(&self) -> Result<()> {
        let [x, y, w, h] = self.tablet.area;
        if !(0.0..=1.0).contains(&x)
            || !(0.0..=1.0).contains(&y)
            || w <= 0.0
            || h <= 0.0
            || x + w > 1.0001
            || y + h > 1.0001
        {
            bail!(
                "tablet.area must be inside [0,1] with positive size, got {:?}",
                self.tablet.area
            );
        }
        if !(0.0..0.5).contains(&self.tablet.min_contact_pressure) {
            bail!("tablet.min_contact_pressure must be in [0, 0.5)");
        }
        self.gestures()?;
        let m = self.video.mode.as_str();
        if !(matches!(m, "off" | "virtual" | "test-pattern") || m.starts_with("file:")) {
            bail!("video.mode must be off, virtual, test-pattern or file:PATH, got {m:?}");
        }
        if !matches!(self.video.encoder.as_str(), "vaapi" | "x264") {
            bail!("video.encoder must be vaapi or x264");
        }
        Ok(())
    }

    pub fn mapper(&self) -> MapperConfig {
        MapperConfig {
            area: self.tablet.area,
            use_predicted: self.tablet.use_predicted,
            min_contact_pressure: self.tablet.min_contact_pressure,
        }
    }

    pub fn gestures(&self) -> Result<GestureConfig> {
        Ok(GestureConfig {
            double_tap: Action::parse(&self.pencil.double_tap).context("pencil.double_tap")?,
            squeeze: Action::parse(&self.pencil.squeeze).context("pencil.squeeze")?,
            squeeze_mode: match self.pencil.squeeze_mode.as_str() {
                "click" => SqueezeMode::Click,
                "hold" => SqueezeMode::Hold,
                m => bail!("pencil.squeeze_mode must be click or hold, got {m:?}"),
            },
        })
    }

    /// Keys the companion keyboard device must declare.
    pub fn keyboard_keys(&self) -> Vec<u16> {
        let mut keys = Vec::new();
        for a in [&self.pencil.double_tap, &self.pencil.squeeze] {
            if let Some(rest) = a.trim().strip_prefix("keys:") {
                if let Ok(k) = parse_key_combo(rest) {
                    keys.extend(k);
                }
            }
        }
        keys.sort_unstable();
        keys.dedup();
        keys
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_parses_to_defaults() {
        let c: Config = toml::from_str(EXAMPLE).unwrap();
        c.validate().unwrap();
        assert_eq!(c.connection.port, sidecar_protocol::DEFAULT_PORT);
        assert_eq!(c.tablet.area, [0.0, 0.0, 1.0, 1.0]);
        assert!(c.keyboard_keys().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn keys_and_validation() {
        let c: Config = toml::from_str("[pencil]\ndouble_tap = \"keys:ctrl+z\"\n").unwrap();
        assert_eq!(c.keyboard_keys(), vec![29, 44]);
        let bad: Config = toml::from_str("[tablet]\narea = [0.5, 0.0, 0.8, 1.0]\n").unwrap();
        assert!(bad.validate().is_err());
        assert!(toml::from_str::<Config>("[tablet]\nbogus = 1\n").is_err());
    }
}
