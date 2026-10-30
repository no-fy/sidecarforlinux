//! Minimal evdev event representation, independent of the uinput backend so
//! the mapping logic can be unit-tested without /dev/uinput.

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const EV_MSC: u16 = 0x04;

pub const SYN_REPORT: u16 = 0;

pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_Z: u16 = 0x02;
pub const ABS_PRESSURE: u16 = 0x18;
pub const ABS_DISTANCE: u16 = 0x19;
pub const ABS_TILT_X: u16 = 0x1a;
pub const ABS_TILT_Y: u16 = 0x1b;

pub const BTN_TOOL_PEN: u16 = 0x140;
pub const BTN_TOOL_RUBBER: u16 = 0x141;
pub const BTN_TOUCH: u16 = 0x14a;
pub const BTN_STYLUS: u16 = 0x14b;
pub const BTN_STYLUS2: u16 = 0x14c;
pub const BTN_STYLUS3: u16 = 0x149;

pub const INPUT_PROP_POINTER: u16 = 0x00;
pub const INPUT_PROP_DIRECT: u16 = 0x01;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawEvent {
    pub ty: u16,
    pub code: u16,
    pub value: i32,
}

impl RawEvent {
    pub const fn key(code: u16, down: bool) -> Self {
        RawEvent { ty: EV_KEY, code, value: down as i32 }
    }
    pub const fn abs(code: u16, value: i32) -> Self {
        RawEvent { ty: EV_ABS, code, value }
    }
}

/// Which virtual device an event batch belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Pen,
    Keys,
}

/// Human-readable names (mirrors evtest output) for logs and tests.
pub fn code_name(ty: u16, code: u16) -> String {
    let n = match (ty, code) {
        (EV_ABS, ABS_X) => "ABS_X",
        (EV_ABS, ABS_Y) => "ABS_Y",
        (EV_ABS, ABS_Z) => "ABS_Z",
        (EV_ABS, ABS_PRESSURE) => "ABS_PRESSURE",
        (EV_ABS, ABS_DISTANCE) => "ABS_DISTANCE",
        (EV_ABS, ABS_TILT_X) => "ABS_TILT_X",
        (EV_ABS, ABS_TILT_Y) => "ABS_TILT_Y",
        (EV_KEY, BTN_TOOL_PEN) => "BTN_TOOL_PEN",
        (EV_KEY, BTN_TOOL_RUBBER) => "BTN_TOOL_RUBBER",
        (EV_KEY, BTN_TOUCH) => "BTN_TOUCH",
        (EV_KEY, BTN_STYLUS) => "BTN_STYLUS",
        (EV_KEY, BTN_STYLUS2) => "BTN_STYLUS2",
        (EV_KEY, BTN_STYLUS3) => "BTN_STYLUS3",
        (EV_SYN, SYN_REPORT) => "SYN_REPORT",
        _ => return format!("{ty}:{code}"),
    };
    n.to_string()
}

impl std::fmt::Display for RawEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", code_name(self.ty, self.code), self.value)
    }
}
