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
# Cap pen samples per second (0 = all, ~240 Hz). Try 120 or 60 if an app lags
# or loses parts of strokes with the pencil but not with a mouse (AFFiNE).
max_rate_hz = 0

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
# iPad) | test-pattern | file:/path/to/clip.h264 | pipewire:NODE (capture an
# existing PipeWire video node by id or name; for testing)
mode = "off"
# Who captures and encodes. Default: native when this sidecard was built
# with it (scripts/install-daemon.sh says if not) and the encoder isn't x264,
# else gstreamer.
#   native     inside sidecard: PipeWire DMA-BUF -> VA-API, zero copy, as
#              fast as the virtual monitor refreshes, per-stage timing log,
#              keyframes on request (tested on an Intel UHD 630)
#   gstreamer  a gst-launch-1.0 subprocess (tested; no build dependencies)
#   hybrid     native PipeWire capture, frames piped into the gst-launch
#              encoder (slower; for testing the capture on its own)
# backend = "native"
# vaapi (GPU) | vaapi-lp (GPU fixed-function "low power" encoder, Intel) |
# x264 (software; gstreamer and hybrid backends only)
encoder = "vaapi"
# native backend: DRM render node for VA-API. Empty = the first one that can
# encode H.264 (the one gst's vah264enc uses), e.g. "/dev/dri/renderD129".
# Use the GPU KWin renders on: another GPU can't take KWin's buffers directly
# and falls back to a slow CPU copy (seen with Intel + AMD in one laptop).
va_device = ""
# VA-API speed/quality trade-off: 1 = best quality ... 7 = fastest (lowest
# latency). The native backend maps it onto the driver's own range (Intel
# has 7 levels, AMD 32; 7 is always the fastest).
target_usage = 7
# native backend: constant QP (1-51, lower = sharper and more bits) instead
# of CBR at bitrate_kbps. 0 = CBR when the encoder has it, else QP 24 (Intel's
# low-power encoder, vaapi-lp, has no CBR without the HuC firmware).
qp = 0
# Pixel density of the virtual monitor. 0 = the iPad's own (2 = sharp). Lower
# (e.g. 1.5) means fewer pixels to render, encode and decode: less latency,
# softer picture. Clashes with a custom display mode saved for Virtual-ipad
# at the full size (the picture lands in a corner; README troubleshooting).
scale = 0.0
bitrate_kbps = 20000
# gstreamer/hybrid backends: the encoder's frame rate (keyframe interval,
# test pattern). native: the rate-control hint until the compositor says how
# fast it will send.
fps = 60
# native/hybrid: cap on how often KWin captures (its "max framerate").
# 0 = no cap: as fast as the virtual monitor's refresh rate (e.g. 120 with a
# 120 Hz mode). If the encoder can't keep up, older frames are skipped.
max_fps = 0
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
    /// Empty = automatic, see [`VideoCfg::backend`].
    pub backend: String,
    pub encoder: String,
    pub va_device: String,
    pub target_usage: u8,
    pub qp: u8,
    pub scale: f64,
    pub bitrate_kbps: u32,
    pub fps: u32,
    pub max_fps: u32,
}

/// The automatic `[video] backend`: "native" when this build has it (Linux,
/// cargo feature native-video; faster on real hardware, DECISIONS D22),
/// else "gstreamer".
pub fn default_backend() -> &'static str {
    if cfg!(all(feature = "native-video", target_os = "linux")) {
        "native"
    } else {
        "gstreamer"
    }
}

impl VideoCfg {
    /// The backend to use: the configured one, or automatic: x264 needs
    /// gst-launch (native encodes with VA-API only), else [`default_backend`].
    pub fn backend(&self) -> &str {
        if !self.backend.is_empty() {
            &self.backend
        } else if self.encoder == "x264" {
            "gstreamer"
        } else {
            default_backend()
        }
    }
}

impl Default for VideoCfg {
    fn default() -> Self {
        VideoCfg {
            mode: "off".into(),
            backend: String::new(),
            encoder: "vaapi".into(),
            va_device: String::new(),
            target_usage: 7,
            qp: 0,
            scale: 0.0,
            bitrate_kbps: 20000,
            fps: 60,
            max_fps: 0,
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
    pub max_rate_hz: u32,
}

impl Default for TabletCfg {
    fn default() -> Self {
        TabletCfg {
            area: [0.0, 0.0, 1.0, 1.0],
            direct: true,
            use_predicted: false,
            min_contact_pressure: DEFAULT_MIN_CONTACT_PRESSURE,
            max_rate_hz: 0,
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
        if self.tablet.max_rate_hz != 0 && !(20..=240).contains(&self.tablet.max_rate_hz) {
            bail!("tablet.max_rate_hz must be 0 (no cap) or between 20 and 240");
        }
        self.gestures()?;
        let m = self.video.mode.as_str();
        if !(matches!(m, "off" | "virtual" | "test-pattern")
            || m.starts_with("file:")
            || m.strip_prefix("pipewire:").is_some_and(|t| !t.is_empty()))
        {
            bail!(
                "video.mode must be off, virtual, test-pattern, file:PATH or pipewire:NODE, got {m:?}"
            );
        }
        if !matches!(
            self.video.backend.as_str(),
            "" | "gstreamer" | "native" | "hybrid"
        ) {
            bail!("video.backend must be gstreamer, native or hybrid");
        }
        if self.video.backend == "native" && self.video.encoder == "x264" {
            bail!("video.backend = \"native\" encodes with VA-API: use encoder = \"vaapi\" or \"vaapi-lp\" (or backend = \"hybrid\" for x264)");
        }
        if self.video.qp > 51 {
            bail!("video.qp must be 0 (CBR) or 1-51");
        }
        if !(1..=7).contains(&self.video.target_usage) {
            bail!("video.target_usage must be 1 (best quality) to 7 (fastest)");
        }
        if self.video.scale != 0.0 && !(1.0..=3.0).contains(&self.video.scale) {
            bail!("video.scale must be 0 (the iPad's own) or between 1.0 and 3.0");
        }
        if !matches!(self.video.encoder.as_str(), "vaapi" | "vaapi-lp" | "x264") {
            bail!("video.encoder must be vaapi, vaapi-lp or x264");
        }
        Ok(())
    }

    pub fn mapper(&self) -> MapperConfig {
        MapperConfig {
            area: self.tablet.area,
            use_predicted: self.tablet.use_predicted,
            min_contact_pressure: self.tablet.min_contact_pressure,
            max_rate_hz: self.tablet.max_rate_hz,
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
        assert_eq!(c.video.backend(), default_backend());
        // x264 without a backend picks the one that can run it.
        let x: Config = toml::from_str("[video]\nencoder = \"x264\"\n").unwrap();
        x.validate().unwrap();
        assert_eq!(x.video.backend(), "gstreamer");
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

    #[test]
    fn video_backend_validation() {
        let ok = |t: &str| toml::from_str::<Config>(t).unwrap().validate().is_ok();
        assert!(ok("[video]\nbackend = \"native\"\nmode = \"virtual\"\n"));
        assert!(ok(
            "[video]\nbackend = \"hybrid\"\nencoder = \"x264\"\nmode = \"pipewire:42\"\n"
        ));
        assert!(!ok("[video]\nbackend = \"native\"\nencoder = \"x264\"\n"));
        assert!(!ok("[video]\nbackend = \"ffmpeg\"\n"));
        assert!(!ok("[video]\nmode = \"pipewire:\"\n"));
    }
}
