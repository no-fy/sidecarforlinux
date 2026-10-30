//! Real /dev/uinput test: create the tablet, replay the synthetic recording
//! through the mapper, read the events back from /dev/input/eventN and check
//! the device capabilities the kernel reports.
//!
//! Needs write access to /dev/uinput, so it only runs when
//! SIDECAR_UINPUT_TEST=1 (CI runs it as root on an ubuntu runner).

use sidecard::events::*;
use sidecard::gestures::{self, GestureConfig};
use sidecard::recording;
use sidecard::sink::{DeviceOptions, EventSink, UinputSink, PEN_DEVICE_NAME};
use sidecard::tablet::*;
use std::time::{Duration, Instant};

fn enabled() -> bool {
    std::env::var_os("SIDECAR_UINPUT_TEST").is_some()
}

#[test]
fn uinput_tablet_capabilities_and_events_roundtrip() {
    if !enabled() {
        eprintln!("skipped: set SIDECAR_UINPUT_TEST=1 (needs /dev/uinput)");
        return;
    }
    let geo = TabletGeometry {
        x_max: 22706,
        y_max: 15779,
        resolution: 100,
    };
    let mut sink = UinputSink::create(
        geo,
        &DeviceOptions {
            direct: true,
            keyboard_keys: vec![],
        },
    )
    .unwrap();
    let nodes = sink.dev_nodes();
    let node = nodes
        .iter()
        .find(|n| n.to_string_lossy().contains("event"))
        .expect("event node")
        .clone();
    // udev may need a moment to create the node.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut dev = loop {
        match evdev::raw_stream::RawDevice::open(&node) {
            Ok(d) => break d,
            Err(e) if Instant::now() < deadline => {
                eprintln!("waiting for {}: {e}", node.display());
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("open {}: {e}", node.display()),
        }
    };
    assert_eq!(dev.name(), Some(PEN_DEVICE_NAME));
    assert!(dev.properties().contains(evdev::PropType::DIRECT));
    let keys = dev.supported_keys().unwrap();
    for k in [
        BTN_TOOL_PEN,
        BTN_TOOL_RUBBER,
        BTN_TOUCH,
        BTN_STYLUS,
        BTN_STYLUS2,
    ] {
        assert!(keys.contains(evdev::KeyCode(k)), "missing key {k:#x}");
    }
    let absinfo: Vec<(u16, evdev::AbsInfo)> =
        dev.get_absinfo().unwrap().map(|(c, i)| (c.0, i)).collect();
    let info = |code: u16| {
        absinfo
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, i)| *i)
            .unwrap()
    };
    assert_eq!(
        (info(ABS_X).maximum(), info(ABS_X).resolution()),
        (22706, 100)
    );
    assert_eq!(
        (info(ABS_Y).maximum(), info(ABS_Y).resolution()),
        (15779, 100)
    );
    assert_eq!(info(ABS_PRESSURE).maximum(), PRESSURE_MAX);
    const { assert!(PRESSURE_MAX + 1 >= 4096) };
    assert_eq!(
        (info(ABS_TILT_X).minimum(), info(ABS_TILT_X).maximum()),
        (-90, 90)
    );
    assert_eq!(info(ABS_TILT_X).resolution(), TILT_RESOLUTION);
    assert_eq!(
        (info(ABS_Z).minimum(), info(ABS_Z).maximum()),
        (ROLL_MIN, ROLL_MAX)
    );
    assert_eq!(info(ABS_DISTANCE).maximum(), DISTANCE_MAX);

    // Replay the demo as fast as possible and compute the expected stream.
    let mut mapper = PenMapper::new(geo, MapperConfig::default());
    let gcfg = GestureConfig::default();
    let mut expected: Vec<RawEvent> = Vec::new();
    for (i, e) in recording::demo().iter().enumerate() {
        let frames = if let Some(s) = e.to_sample(i as u64, i as u32).unwrap() {
            mapper.sample(&s)
        } else if let Some(g) = e.to_gesture(i as u64).unwrap() {
            gestures::handle(&gcfg, &mut mapper, &g).pen_frames
        } else {
            vec![]
        };
        for f in &frames {
            sink.emit(DeviceKind::Pen, f).unwrap();
            expected.extend(f.iter().copied());
            expected.push(RawEvent {
                ty: EV_SYN,
                code: SYN_REPORT,
                value: 0,
            });
        }
    }

    // Read back everything the kernel delivered.
    {
        use std::os::fd::AsRawFd;
        let fd = dev.as_raw_fd();
        // SAFETY: fcntl on a valid, owned fd.
        unsafe {
            libc::fcntl(
                fd,
                libc::F_SETFL,
                libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK,
            )
        };
    }
    let mut got: Vec<RawEvent> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last_data = Instant::now();
    while Instant::now() < deadline {
        match read_some(&mut dev, &mut got) {
            Ok(_) => last_data = Instant::now(),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                if !got.is_empty() && last_data.elapsed() > Duration::from_millis(300) {
                    break; // drained
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => panic!("read: {e}"),
        }
    }
    // The input core drops ABS events equal to the current value (all start
    // at 0) and key events that don't change state, and SYN_REPORTs of
    // frames that became empty. Apply the same filter to the expectation.
    let mut abs_state = std::collections::HashMap::new();
    let mut key_state = std::collections::HashMap::new();
    let filtered: Vec<RawEvent> = expected
        .iter()
        .filter(|e| match e.ty {
            EV_ABS => abs_state.insert(e.code, e.value).unwrap_or(0) != e.value,
            EV_KEY => key_state.insert(e.code, e.value).unwrap_or(0) != e.value,
            _ => false,
        })
        .copied()
        .collect();
    let strip = |v: &[RawEvent]| {
        v.iter()
            .filter(|e| e.ty != EV_SYN)
            .copied()
            .collect::<Vec<_>>()
    };
    assert_eq!(
        strip(&got),
        filtered,
        "kernel delivered a different event stream"
    );
    let pressure_max = got
        .iter()
        .filter(|e| e.ty == EV_ABS && e.code == ABS_PRESSURE)
        .map(|e| e.value)
        .max();
    assert!(pressure_max.unwrap() > 8000);
}

fn read_some(
    dev: &mut evdev::raw_stream::RawDevice,
    out: &mut Vec<RawEvent>,
) -> std::io::Result<usize> {
    let evs: Vec<RawEvent> = dev
        .fetch_events()?
        .map(|e| RawEvent {
            ty: e.event_type().0,
            code: e.code(),
            value: e.value(),
        })
        .collect();
    out.extend(&evs);
    Ok(evs.len())
}
