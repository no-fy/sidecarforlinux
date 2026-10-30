//! Where mapped events go: real uinput devices, or a recorder for tests.

use crate::events::*;
use crate::tablet::{
    Frame, TabletGeometry, DISTANCE_MAX, PRESSURE_MAX, ROLL_MAX, ROLL_MIN, TILT_MAX_DEG, TILT_RESOLUTION,
};
use anyhow::{Context, Result};
use std::io;
use std::path::PathBuf;

pub trait EventSink: Send {
    /// Write one frame; the sink appends SYN_REPORT.
    fn emit(&mut self, dev: DeviceKind, frame: &[RawEvent]) -> io::Result<()>;

    fn emit_all(&mut self, dev: DeviceKind, frames: &[Frame]) -> io::Result<()> {
        for f in frames {
            self.emit(dev, f)?;
        }
        Ok(())
    }

    /// /dev/input/eventN nodes (for logs and KWin integration).
    fn dev_nodes(&mut self) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// Records frames in memory; used by tests and `--dry-run`.
#[derive(Default)]
pub struct RecordingSink {
    pub frames: Vec<(DeviceKind, Frame)>,
    pub print: bool,
}

impl EventSink for RecordingSink {
    fn emit(&mut self, dev: DeviceKind, frame: &[RawEvent]) -> io::Result<()> {
        if self.print {
            let s: Vec<String> = frame.iter().map(|e| e.to_string()).collect();
            println!("{dev:?}: {} | SYN_REPORT", s.join(", "));
        }
        self.frames.push((dev, frame.to_vec()));
        Ok(())
    }
}

pub const PEN_DEVICE_NAME: &str = "Sidecar Apple Pencil";
pub const KEYS_DEVICE_NAME: &str = "Sidecar Apple Pencil Keys";
/// pid.codes test VID/PID (1209:0001), BUS_VIRTUAL. See docs/DECISIONS.md.
pub const VENDOR_ID: u16 = 0x1209;
pub const PRODUCT_ID: u16 = 0x0001;

#[derive(Debug, Clone)]
pub struct DeviceOptions {
    /// Advertise INPUT_PROP_DIRECT (a screen tablet, like a Cintiq) rather
    /// than INPUT_PROP_POINTER (an opaque tablet, like an Intuos).
    pub direct: bool,
    /// Extra keys for the companion keyboard device.
    pub keyboard_keys: Vec<u16>,
}

pub struct UinputSink {
    pen: evdev::uinput::VirtualDevice,
    keys: Option<evdev::uinput::VirtualDevice>,
}

fn abs(code: u16, min: i32, max: i32, res: i32) -> evdev::UinputAbsSetup {
    evdev::UinputAbsSetup::new(evdev::AbsoluteAxisCode(code), evdev::AbsInfo::new(0, min, max, 0, 0, res))
}

impl UinputSink {
    pub fn create(geo: TabletGeometry, opts: &DeviceOptions) -> Result<Self> {
        use evdev::{AttributeSet, BusType, InputId, KeyCode, PropType};
        let mut keys = AttributeSet::<KeyCode>::new();
        for k in [BTN_TOOL_PEN, BTN_TOOL_RUBBER, BTN_TOUCH, BTN_STYLUS, BTN_STYLUS2, BTN_STYLUS3] {
            keys.insert(KeyCode(k));
        }
        let mut props = AttributeSet::<PropType>::new();
        props.insert(if opts.direct { PropType::DIRECT } else { PropType::POINTER });
        let pen = evdev::uinput::VirtualDevice::builder()
            .context("opening /dev/uinput (see docs/MORNING.md: udev rule + uinput group)")?
            .name(PEN_DEVICE_NAME)
            .input_id(InputId::new(BusType::BUS_VIRTUAL, VENDOR_ID, PRODUCT_ID, 1))
            .with_properties(&props)?
            .with_keys(&keys)?
            .with_absolute_axis(&abs(ABS_X, 0, geo.x_max, geo.resolution))?
            .with_absolute_axis(&abs(ABS_Y, 0, geo.y_max, geo.resolution))?
            .with_absolute_axis(&abs(ABS_PRESSURE, 0, PRESSURE_MAX, 0))?
            .with_absolute_axis(&abs(ABS_DISTANCE, 0, DISTANCE_MAX, 0))?
            .with_absolute_axis(&abs(ABS_TILT_X, -TILT_MAX_DEG, TILT_MAX_DEG, TILT_RESOLUTION))?
            .with_absolute_axis(&abs(ABS_TILT_Y, -TILT_MAX_DEG, TILT_MAX_DEG, TILT_RESOLUTION))?
            .with_absolute_axis(&abs(ABS_Z, ROLL_MIN, ROLL_MAX, 0))?
            .build()
            .context("creating uinput pen device")?;

        let keys_dev = if opts.keyboard_keys.is_empty() {
            None
        } else {
            let mut ks = AttributeSet::<KeyCode>::new();
            for k in &opts.keyboard_keys {
                ks.insert(KeyCode(*k));
            }
            Some(
                evdev::uinput::VirtualDevice::builder()?
                    .name(KEYS_DEVICE_NAME)
                    .input_id(InputId::new(BusType::BUS_VIRTUAL, VENDOR_ID, PRODUCT_ID + 1, 1))
                    .with_keys(&ks)?
                    .build()
                    .context("creating uinput keyboard device")?,
            )
        };
        Ok(UinputSink { pen, keys: keys_dev })
    }
}

impl EventSink for UinputSink {
    fn emit(&mut self, dev: DeviceKind, frame: &[RawEvent]) -> io::Result<()> {
        let d = match dev {
            DeviceKind::Pen => &mut self.pen,
            DeviceKind::Keys => match self.keys.as_mut() {
                Some(k) => k,
                None => return Ok(()),
            },
        };
        let evs: Vec<evdev::InputEvent> =
            frame.iter().map(|e| evdev::InputEvent::new(e.ty, e.code, e.value)).collect();
        d.emit(&evs)
    }

    fn dev_nodes(&mut self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for d in std::iter::once(&mut self.pen).chain(self.keys.as_mut()) {
            if let Ok(nodes) = d.enumerate_dev_nodes_blocking() {
                out.extend(nodes.filter_map(|n| n.ok()));
            }
        }
        out
    }
}
