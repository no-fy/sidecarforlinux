//! `[video] backend = "hybrid"`: native PipeWire capture (shared memory),
//! encoded by the same `gst-launch-1.0` encoder as the GStreamer backend,
//! fed raw frames on stdin (`fdsrc ! rawvideoparse ! … ! gdppay`).
//!
//! Slower than either other backend (a CPU copy and a pipe of ~15 MB per
//! frame at 2360x1640). It exists to test the capture half on its own, and
//! it is the one native path that runs end to end without a GPU (CI, VM).
//!
//! Threads: the capture thread copies the frame into a one-slot
//! newest-wins hand-over (`latest.rs`), a writer thread pipes it to gst, a
//! reader thread turns gst's GDP packets back into `Event::Frame`s. gst
//! encodes every frame it is given, in order, so stage times are matched
//! to output access units first-in first-out.

use super::frame::{
    fourcc_name, Converted, Encoded, Frame, FrameSink, Pixels, DRM_ABGR8888, DRM_ARGB8888,
    DRM_XBGR8888, DRM_XRGB8888,
};
use super::latest::{Latest, Taken};
use super::pw::Event;
use super::stats::StageTimes;
use crate::clock::now_ns;
use crate::video::GdpReader;
use anyhow::{anyhow, Context, Result};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;

struct RawFrame {
    data: Vec<u8>,
    times: StageTimes,
}

struct Running {
    key: (u32, u32, u32),
    child: Child,
    slot: Arc<Latest<RawFrame>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.slot.close();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub struct GstPipeSink {
    /// `encoder_args` + `tail` of the GStreamer backend.
    encoder: Vec<String>,
    fps: u32,
    out: mpsc::Sender<Event>,
    running: Option<Running>,
    pool: Arc<Mutex<Vec<Vec<u8>>>>,
    /// Frames replaced in the slot before the writer took them.
    replaced: u32,
}

fn raw_format(fourcc: u32) -> Option<&'static str> {
    match fourcc {
        DRM_XRGB8888 => Some("bgrx"),
        DRM_ARGB8888 => Some("bgra"),
        DRM_XBGR8888 => Some("rgbx"),
        DRM_ABGR8888 => Some("rgba"),
        _ => None,
    }
}

impl GstPipeSink {
    pub fn new(encoder: Vec<String>, fps: u32, out: mpsc::Sender<Event>) -> Self {
        GstPipeSink {
            encoder,
            fps: fps.max(1),
            out,
            running: None,
            pool: Arc::default(),
            replaced: 0,
        }
    }

    fn start(&mut self, width: u32, height: u32, fourcc: u32) -> Result<()> {
        self.running = None;
        let format = raw_format(fourcc)
            .ok_or_else(|| anyhow!("no raw format for {}", fourcc_name(fourcc)))?;
        let frame_bytes = width as usize * height as usize * 4;
        let mut argv: Vec<String> = vec![
            "-q".into(),
            "fdsrc".into(),
            "fd=0".into(),
            format!("blocksize={frame_bytes}"),
            "!".into(),
            "rawvideoparse".into(),
            format!("width={width}"),
            format!("height={height}"),
            format!("format={format}"),
            format!("framerate={}/1", self.fps),
            "!".into(),
        ];
        argv.extend(self.encoder.iter().cloned());
        tracing::info!("hybrid video encoder: gst-launch-1.0 {}", argv.join(" "));
        let mut child = Command::new("gst-launch-1.0")
            .args(&argv)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .context(
                "starting gst-launch-1.0 (is GStreamer installed? see docs/dependencies.md)",
            )?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        {
            use std::os::fd::AsRawFd;
            // Fewer wake-ups per frame; best effort (the default is 64 KiB).
            // SAFETY: plain fcntl on a pipe fd we own.
            unsafe { libc::fcntl(stdin.as_raw_fd(), libc::F_SETPIPE_SZ, 1 << 20) };
        }
        let slot: Arc<Latest<RawFrame>> = Arc::default();
        let fifo: Arc<Mutex<VecDeque<StageTimes>>> = Arc::default();

        let (slot2, fifo2, pool) = (slot.clone(), fifo.clone(), self.pool.clone());
        std::thread::Builder::new()
            .name("sidecard-gst-in".into())
            .spawn(move || loop {
                match slot2.take(Duration::from_secs(1)) {
                    Taken::Value(f) => {
                        fifo2.lock().unwrap().push_back(f.times);
                        if stdin.write_all(&f.data).is_err() {
                            return; // gst went away; the reader reports it
                        }
                        pool.lock().unwrap().push(f.data);
                    }
                    Taken::TimedOut => {}
                    Taken::Closed => return,
                }
            })?;

        let out = self.out.clone();
        std::thread::Builder::new()
            .name("sidecard-gst-out".into())
            .spawn(move || {
                let mut gdp = GdpReader::new();
                let mut buf = vec![0u8; 256 * 1024];
                let why = loop {
                    let n = match stdout.read(&mut buf) {
                        Ok(0) => break "the gst-launch encoder exited".to_string(),
                        Ok(n) => n,
                        Err(e) => break format!("reading the gst-launch encoder: {e}"),
                    };
                    let aus = match gdp.push(&buf[..n]) {
                        Ok(a) => a,
                        Err(e) => break format!("{e:#}"),
                    };
                    for au in aus {
                        let encoded = now_ns();
                        let mut times = fifo.lock().unwrap().pop_front().unwrap_or(StageTimes {
                            arrived_ns: encoded,
                            converted_ns: encoded,
                            ..Default::default()
                        });
                        times.encoded_ns = encoded;
                        let e = Encoded {
                            annexb: au,
                            width,
                            height,
                            times,
                        };
                        if out.blocking_send(Event::Frame(e)).is_err() {
                            return;
                        }
                    }
                };
                let _ = out.blocking_send(Event::Ended(Err(why)));
            })?;
        self.running = Some(Running {
            key: (width, height, fourcc),
            child,
            slot,
        });
        Ok(())
    }
}

impl FrameSink for GstPipeSink {
    fn dmabuf_formats(&self) -> Vec<(u32, Vec<u64>)> {
        Vec::new() // shared memory only: the frame goes through the CPU anyway
    }

    fn convert(&mut self, frame: &Frame) -> Result<Converted> {
        let Pixels::Shm {
            fourcc,
            data,
            stride,
        } = frame.pixels
        else {
            return Ok(Converted::Skipped(
                "the hybrid backend takes shared-memory frames only".into(),
            ));
        };
        if raw_format(fourcc).is_none() {
            return Ok(Converted::Skipped(format!(
                "pixel format {} not supported",
                fourcc_name(fourcc)
            )));
        }
        let key = (frame.width, frame.height, fourcc);
        if self.running.as_ref().map(|r| r.key) != Some(key) {
            self.start(frame.width, frame.height, fourcc)?;
        }
        // Tightly packed rows, as rawvideoparse expects.
        let row = frame.width as usize * 4;
        let mut out = self.pool.lock().unwrap().pop().unwrap_or_default();
        out.clear();
        out.reserve(row * frame.height as usize);
        for y in 0..frame.height as usize {
            let start = y * stride as usize;
            let line = data
                .get(start..start + row)
                .ok_or_else(|| anyhow!("frame shorter than its stride says"))?;
            out.extend_from_slice(line);
        }
        let mut times = frame.times;
        times.stale_skipped += std::mem::take(&mut self.replaced);
        times.converted_ns = now_ns();
        let slot = &self.running.as_ref().unwrap().slot;
        if let Some(old) = slot.put(RawFrame { data: out, times }) {
            self.replaced += 1;
            self.pool.lock().unwrap().push(old.data);
        }
        Ok(Converted::Done)
    }

    fn encode(&mut self, _force_idr: bool) -> Result<Option<Encoded>> {
        // gst encodes on its own threads; frames arrive through the reader.
        // It can't make a keyframe on request (key-int-max = 2 s instead).
        Ok(None)
    }
}
