//! The PipeWire capture source against a real PipeWire daemon and a
//! synthetic `Video/Source` node (`gst-launch-1.0 videotestsrc !
//! pipewiresink`). Runs only with SIDECAR_PIPEWIRE_TEST=1 (CI starts
//! pipewire + wireplumber for it); no GPU needed.
#![cfg(all(feature = "native-video", target_os = "linux"))]

use sidecard::native::frame::{
    Converted, Encoded, Frame, FrameSink, Pixels, DRM_FORMAT_MOD_LINEAR, DRM_XRGB8888,
};
use sidecard::native::pw::{Capture, CaptureSpec, Event};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

struct Source(Child);

impl Drop for Source {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn source(name: &str, fps: u32) -> Source {
    let child = Command::new("gst-launch-1.0")
        .args(["-q", "videotestsrc", "is-live=true", "pattern=ball", "!"])
        .arg(format!(
            "video/x-raw,format=BGRx,width=320,height=240,framerate={fps}/1"
        ))
        .args(["!", "pipewiresink", "mode=provide"])
        .arg(format!(
            "stream-properties=props,media.class=Video/Source,node.name={name}"
        ))
        .stdout(Stdio::null())
        .spawn()
        .expect("gst-launch-1.0 with the pipewire plugin");
    std::thread::sleep(Duration::from_millis(1500));
    Source(child)
}

/// Offers DMA-BUF modifiers the test producer can't satisfy, takes SHM,
/// optionally slowly.
struct TestSink {
    delay: Duration,
    shm_frames: Arc<AtomicU64>,
    last: Option<Encoded>,
}

impl FrameSink for TestSink {
    fn dmabuf_formats(&self) -> Vec<(u32, Vec<u64>)> {
        vec![(
            DRM_XRGB8888,
            vec![0x0100_0000_0000_0002, DRM_FORMAT_MOD_LINEAR],
        )]
    }

    fn convert(&mut self, f: &Frame) -> anyhow::Result<Converted> {
        std::thread::sleep(self.delay);
        let Pixels::Shm { data, stride, .. } = f.pixels else {
            anyhow::bail!("expected shared memory from a CPU test source");
        };
        assert!(stride >= 320 * 4 && data.len() >= stride as usize * 239 + 320 * 4);
        self.shm_frames.fetch_add(1, Ordering::Relaxed);
        let mut times = f.times;
        times.converted_ns = sidecard::clock::now_ns();
        self.last = Some(Encoded {
            annexb: Vec::new(),
            width: f.width,
            height: f.height,
            times,
        });
        Ok(Converted::Done)
    }

    fn encode(&mut self, _: bool) -> anyhow::Result<Option<Encoded>> {
        Ok(self.last.take())
    }
}

/// Nodes and links, for failure messages (is the session manager linking?).
fn pw_state() -> String {
    let run = |cmd: &str, args: &[&str]| {
        Command::new(cmd)
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_else(|e| format!("{cmd}: {e}"))
    };
    format!(
        "pw-cli ls Node:\n{}\npw-link -l:\n{}",
        run("pw-cli", &["ls", "Node"]),
        run("pw-link", &["-l"])
    )
}

fn enabled() -> bool {
    if std::env::var_os("SIDECAR_PIPEWIRE_TEST").is_none() {
        eprintln!("SIDECAR_PIPEWIRE_TEST not set: skipping (needs a running PipeWire)");
        return false;
    }
    true
}

/// Runs a capture until `frames` frames arrived; returns (format line,
/// stale buffers skipped in total, frames).
fn capture(name: &str, delay: Duration, frames: usize) -> (String, u32, Vec<Encoded>) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let (tx, mut rx) = mpsc::channel(4);
        let shm = Arc::new(AtomicU64::new(0));
        let shm2 = shm.clone();
        let cap = Capture::spawn(
            CaptureSpec {
                target: name.into(),
                width: 320,
                height: 240,
                max_fps: 60,
            },
            move || {
                Ok(Box::new(TestSink {
                    delay,
                    shm_frames: shm2,
                    last: None,
                }) as Box<dyn FrameSink>)
            },
            tx,
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        let (mut format, mut stale, mut got) = (String::new(), 0, Vec::new());
        while got.len() < frames {
            let left = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, rx.recv()).await {
                Ok(Some(Event::Format(f))) => format = f,
                Ok(Some(Event::Frame(e))) => {
                    stale += e.times.stale_skipped;
                    got.push(e);
                }
                Ok(Some(Event::Ended(r))) => panic!("capture ended early: {r:?}"),
                Ok(None) => panic!("capture channel closed"),
                Err(_) => panic!(
                    "only {} frames in 20 s (format: {format:?})\n{}",
                    got.len(),
                    pw_state()
                ),
            }
        }
        drop(cap);
        assert_eq!(shm.load(Ordering::Relaxed) as usize, got.len());
        (format, stale, got)
    })
}

#[test]
fn falls_back_to_shm_and_a_slow_sink_gets_fresh_pictures() {
    if !enabled() {
        return;
    }
    // gst's pipewiresink quits when its consumer leaves: one source each.
    let name = format!("sidecar-test-{}-a", std::process::id());
    let src = source(&name, 30);

    // A sink as fast as the source: every frame.
    let (format, _, fast) = capture(&name, Duration::ZERO, 20);
    drop(src);
    assert!(format.contains("XR24 320x240"), "{format}");
    assert!(format.contains("shared memory"), "{format}");
    for w in fast.windows(2) {
        assert!(w[1].times.arrived_ns > w[0].times.arrived_ns);
    }
    assert!(fast.iter().all(|f| (f.width, f.height) == (320, 240)));

    // A sink 3x slower than the source must still get fresh pictures, not
    // a backlog: the producer's timestamp -> arrival must not grow.
    // (gst's pipewiresink recycles its buffer instead of queueing more, so
    // the "skip older queued buffers" branch isn't reached here; see
    // frame::newest_usable's unit test. KWin may queue up to 4.)
    let name = format!("sidecar-test-{}-b", std::process::id());
    let _src = source(&name, 30);
    let (_, stale, slow) = capture(&name, Duration::from_millis(100), 8);
    let age = |f: &Encoded| {
        f.times
            .tick_ns
            .map(|t| (f.times.arrived_ns - t) / 1_000_000)
    };
    let mut fast_ages: Vec<u64> = fast.iter().filter_map(age).collect();
    fast_ages.sort_unstable();
    if let Some(&median) = fast_ages.get(fast_ages.len() / 2) {
        for f in &slow[1..] {
            let a = age(f).unwrap();
            assert!(
                a <= median + 30,
                "slow sink got a picture {a} ms old (fast sink: {median} ms): a backlog"
            );
        }
    }
    let last = slow.last().unwrap();
    let took_ms = (last.times.converted_ns - last.times.arrived_ns) / 1_000_000;
    assert!(
        (100..250).contains(&took_ms),
        "convert took {took_ms} ms, expected ~100"
    );
    eprintln!(
        "slow sink: {stale} stale buffers skipped over {} frames",
        slow.len()
    );
}
