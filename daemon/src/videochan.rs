//! Phase 2: the video channel. A second TLS connection to the iPad
//! (`Hello.channel = Video`, same `session_id` as the pen session) that
//! carries `VideoConfig` + `VideoFrame` messages from an encoder.
//!
//! Encoders: a `gst-launch-1.0` subprocess writing H.264 Annex-B to stdout
//! (`[video] backend = "gstreamer"`, DECISIONS D18), or capture and encode
//! inside the daemon (`"native"` / `"hybrid"`, D22, `native/`). See
//! docs/phase2-virtual-display.md.

use crate::clock::now_ns;
use crate::conn::{MsgReader, MsgWriter};
use crate::identity::Identity;
use crate::native::h264::KeyframePolicy;
use crate::native::stats::{StageStats, StageTimes};
use crate::transport::{self, Connected};
use crate::video::{complete_au, AnnexBSplitter, AuAssembler, GdpReader, Output};
use anyhow::{anyhow, bail, Context, Result};
use sidecar_protocol::pairing::Fingerprint;
use sidecar_protocol::*;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoder {
    /// Mesa VA-API through GStreamer's `va` plugin (AMD/Intel GPUs).
    Vaapi,
    /// The same, on Intel's fixed-function "low power" encoder
    /// (`vah264lpenc`, VAEntrypointEncSliceLP): usually faster.
    VaapiLowPower,
    /// Software x264 (any machine).
    X264,
}

#[derive(Debug, Clone)]
pub enum VideoSource {
    /// Replay an H.264 Annex-B file at `fps` (tests, demos).
    File {
        path: PathBuf,
        fps: u32,
        width: u32,
        height: u32,
        repeat: bool,
    },
    /// Any command that writes H.264 Annex-B to stdout: raw, or one access
    /// unit per GDP buffer (`gdp`, what our gst-launch pipelines produce).
    Command {
        argv: Vec<String>,
        width: u32,
        height: u32,
        fps: u32,
        gdp: bool,
    },
    /// Capture a PipeWire node inside the daemon (`backend = "native"` or
    /// `"hybrid"`).
    Native(NativeSpec),
}

/// The in-daemon backends (`native/`).
#[derive(Debug, Clone)]
pub struct NativeSpec {
    /// PipeWire node id (KWin's virtual output), name or serial.
    pub target: String,
    pub width: u32,
    pub height: u32,
    /// Rate-control hint (and gst's frame rate for hybrid).
    pub fps: u32,
    /// Cap on the compositor's capture rate; 0 = none.
    pub max_fps: u32,
    pub kbps: u32,
    pub target_usage: u8,
    /// Constant QP for the native encoder; 0 = CBR when available.
    pub qp: u8,
    pub encoder: Encoder,
    pub kind: NativeKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeKind {
    /// DMA-BUF → VA-API colour conversion → VA-API H.264, all in-process.
    /// `device`: a DRM render node, None = the first one that encodes H.264.
    Va { device: Option<PathBuf> },
    /// Shared-memory frames piped into the gst-launch encoder.
    Hybrid,
}

impl VideoSource {
    /// Labels of the middle stages, for sources that report stage times.
    fn stage_labels(&self) -> Option<(&'static str, &'static str)> {
        match self {
            VideoSource::Native(n) if n.kind == NativeKind::Hybrid => {
                Some(("copy out", "gst encode"))
            }
            VideoSource::Native(_) => Some(("convert", "encode")),
            _ => None,
        }
    }
}

const X264: &str =
    "x264enc tune=zerolatency speed-preset=ultrafast bframes=0 aud=true byte-stream=true";

pub(crate) fn tail(fps: u32) -> Vec<String> {
    // Keyframe every 2 s so a decoder that joins late recovers quickly.
    vec![
        format!("key-int-max={}", fps * 2),
        "!".into(),
        "h264parse".into(),
        "config-interval=-1".into(),
        "!".into(),
        "video/x-h264,stream-format=byte-stream,alignment=au".into(),
        "!".into(),
        // One access unit per GDP packet, so frames go out without waiting
        // for the next one (see video::GdpReader).
        "gdppay".into(),
        "!".into(),
        "fdsink".into(),
        "fd=1".into(),
        "sync=false".into(),
    ]
}

fn split(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// Moving test pattern, software encoded. Needs gstreamer + gst-plugins-ugly.
pub fn test_pattern(width: u32, height: u32, fps: u32) -> VideoSource {
    let mut argv = split("gst-launch-1.0 -q videotestsrc is-live=true pattern=ball !");
    argv.push(format!(
        "video/x-raw,width={width},height={height},framerate={fps}/1,format=I420"
    ));
    argv.push("!".into());
    argv.extend(split(X264));
    argv.extend(tail(fps));
    VideoSource::Command {
        argv,
        width,
        height,
        fps,
        gdp: true,
    }
}

/// The encoder part of a gst-launch pipeline: raw video in, H.264 out (up
/// to `tail`). Shared by the GStreamer backend and the hybrid one.
pub(crate) fn encoder_args(encoder: Encoder, kbps: u32, target_usage: u8) -> Vec<String> {
    let mut argv = Vec::new();
    match encoder {
        Encoder::Vaapi | Encoder::VaapiLowPower => {
            argv.extend(split("vapostproc !"));
            argv.push("video/x-raw(memory:VAMemory),format=NV12".into());
            argv.push("!".into());
            argv.push(
                if encoder == Encoder::Vaapi {
                    "vah264enc"
                } else {
                    "vah264lpenc"
                }
                .into(),
            );
            argv.extend(split("b-frames=0 rate-control=cbr"));
            argv.push(format!("target-usage={target_usage}"));
            argv.push(format!("bitrate={kbps}"));
        }
        Encoder::X264 => {
            argv.extend(split("videoconvert ! video/x-raw,format=I420 !"));
            argv.extend(split(X264));
            argv.push(format!("bitrate={kbps}"));
        }
    }
    argv
}

/// Capture a PipeWire node (e.g. KWin's virtual output stream) and encode it.
/// `target`: a node id (`path=`, as always used for KWin's node) or a node
/// name (`target-object=`, plus the stream properties WirePlumber needs to
/// link it, found with a synthetic node in the VM).
pub fn pipewire(
    target: &str,
    width: u32,
    height: u32,
    fps: u32,
    encoder: Encoder,
    kbps: u32,
    target_usage: u8,
) -> VideoSource {
    let mut argv = split("gst-launch-1.0 -q");
    argv.push("pipewiresrc".into());
    if target.parse::<u32>().is_ok() {
        argv.push(format!("path={target}"));
    } else {
        argv.push(format!("target-object={target}"));
        argv.push("stream-properties=props,media.type=Video,media.category=Capture".into());
    }
    argv.extend([
        "do-timestamp=true".into(),
        "keepalive-time=1000".into(),
        "!".into(),
    ]);
    argv.extend(split("queue max-size-buffers=1 leaky=downstream !"));
    argv.extend(encoder_args(encoder, kbps, target_usage));
    argv.extend(tail(fps));
    VideoSource::Command {
        argv,
        width,
        height,
        fps,
        gdp: true,
    }
}

#[derive(Debug, Default, Clone)]
pub struct VideoStats {
    pub frames_sent: u64,
    pub keyframes_sent: u64,
    pub bytes_sent: u64,
    pub feedback: u64,
    pub keyframe_requests: u64,
    /// Encoder output → iPad received, microseconds (needs clock sync).
    pub transport_p50_us: u32,
    pub transport_p95_us: u32,
    pub latency_samples: u32,
}

struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A message for the iPad and, for native sources, when each stage ran.
type Produced = (Message, Option<StageTimes>);

/// Messages produced by a source, re-stamped with the daemon clock.
/// `keyframes`: the iPad needs an IDR (only native sources can make one).
async fn produce(
    source: VideoSource,
    tx: mpsc::Sender<Produced>,
    keyframes: mpsc::UnboundedReceiver<()>,
) -> Result<()> {
    match source {
        VideoSource::Native(spec) => produce_native(spec, tx, keyframes).await,
        VideoSource::File {
            path,
            fps,
            width,
            height,
            repeat,
        } => {
            let data = tokio::fs::read(&path)
                .await
                .with_context(|| format!("reading {}", path.display()))?;
            let msgs = crate::video::annexb_to_messages(
                VideoCodec::H264,
                width,
                height,
                fps * 1000,
                &data,
            );
            if msgs.is_empty() {
                bail!("{} contains no H.264 access units", path.display());
            }
            let period = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
            let mut tick = tokio::time::interval(period);
            let mut id = 0u32;
            loop {
                for m in &msgs {
                    let m = match m.clone() {
                        Message::VideoFrame(mut f) => {
                            tick.tick().await;
                            id = id.wrapping_add(1);
                            f.frame_id = id;
                            f.capture_ns = now_ns();
                            f.encoded_ns = f.capture_ns;
                            Message::VideoFrame(f)
                        }
                        other => other,
                    };
                    if tx.send((m, None)).await.is_err() {
                        return Ok(());
                    }
                }
                if !repeat {
                    return Ok(());
                }
            }
        }
        VideoSource::Command {
            argv,
            width,
            height,
            fps,
            gdp,
        } => {
            let (prog, args) = argv
                .split_first()
                .ok_or_else(|| anyhow!("empty encoder command"))?;
            tracing::info!("video encoder: {}", argv.join(" "));
            let mut child = tokio::process::Command::new(prog)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
                .spawn()
                .with_context(|| {
                    format!("starting {prog} (is GStreamer installed? see docs/dependencies.md)")
                })?;
            let mut out = child.stdout.take().unwrap();
            let mut split = AnnexBSplitter::new();
            let mut asm = AuAssembler::new(VideoCodec::H264);
            let mut gdp_reader = gdp.then(GdpReader::new);
            let mut buf = vec![0u8; 256 * 1024];
            let mut id = 0u32;
            loop {
                let n = out.read(&mut buf).await?;
                if n == 0 {
                    let status = child.wait().await?;
                    bail!("video encoder exited ({status})");
                }
                let outputs: Vec<Output> = match gdp_reader.as_mut() {
                    Some(r) => r
                        .push(&buf[..n])?
                        .iter()
                        .flat_map(|au| complete_au(&mut split, &mut asm, au))
                        .collect(),
                    None => split
                        .push(&buf[..n])
                        .into_iter()
                        .flat_map(|nal| asm.push(nal))
                        .collect(),
                };
                for o in outputs {
                    let m = match o {
                        Output::Config(sets) => Message::VideoConfig(VideoConfig {
                            codec: VideoCodec::H264,
                            width,
                            height,
                            fps_milli: fps * 1000,
                            parameter_sets: sets,
                        }),
                        Output::Frame(au) => {
                            id = id.wrapping_add(1);
                            let t = now_ns();
                            Message::VideoFrame(VideoFrame {
                                frame_id: id,
                                keyframe: au.keyframe,
                                capture_ns: t,
                                encoded_ns: t,
                                data: au.sample,
                            })
                        }
                    };
                    if tx.send((m, None)).await.is_err() {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(not(all(feature = "native-video", target_os = "linux")))]
async fn produce_native(
    _: NativeSpec,
    _: mpsc::Sender<Produced>,
    _: mpsc::UnboundedReceiver<()>,
) -> Result<()> {
    Err(anyhow::Error::new(crate::native::Unsupported(
        "this sidecard was built without native video (Linux, cargo feature native-video); \
         use [video] backend = \"gstreamer\""
            .into(),
    )))
}

#[cfg(all(feature = "native-video", target_os = "linux"))]
async fn produce_native(
    spec: NativeSpec,
    tx: mpsc::Sender<Produced>,
    mut keyframes: mpsc::UnboundedReceiver<()>,
) -> Result<()> {
    use crate::native::pw::{Capture, CaptureSpec, Event};
    let (ev_tx, mut events) = mpsc::channel::<Event>(4);
    let max_fps = crate::native::h264::capture_cap(spec.max_fps, spec.width, spec.height);
    if max_fps != spec.max_fps {
        tracing::info!(
            "native capture: at most {max_fps} frames/s at {}x{} (faster needs H.264 level 6, \
             which Intel's encoder rejects)",
            spec.width,
            spec.height
        );
    }
    let cap = CaptureSpec {
        target: spec.target.clone(),
        width: spec.width,
        height: spec.height,
        max_fps,
    };
    let capture = match &spec.kind {
        NativeKind::Hybrid => {
            let mut args = encoder_args(spec.encoder, spec.kbps, spec.target_usage);
            args.extend(tail(spec.fps));
            let fps = spec.fps;
            Capture::spawn(
                cap,
                move |out| {
                    Ok(
                        Box::new(crate::native::hybrid::GstPipeSink::new(args, fps, out))
                            as Box<dyn crate::native::frame::FrameSink>,
                    )
                },
                ev_tx,
            )?
        }
        NativeKind::Va { device } => {
            let opts = crate::native::encoder::VaOptions {
                device: device.clone(),
                low_power: spec.encoder == Encoder::VaapiLowPower,
                kbps: spec.kbps,
                fps: spec.fps.max(1),
                target_usage: spec.target_usage,
                qp: spec.qp,
            };
            Capture::spawn(
                cap,
                move |out| {
                    Ok(
                        Box::new(crate::native::encoder::VaEncoder::open(opts, out)?)
                            as Box<dyn crate::native::frame::FrameSink>,
                    )
                },
                ev_tx,
            )?
        }
    };
    tracing::info!(
        "video: native capture of PipeWire node {} ({})",
        spec.target,
        match spec.kind {
            NativeKind::Va { .. } => "VA-API in the daemon",
            NativeKind::Hybrid => "hybrid: raw frames to gst-launch",
        }
    );
    let mut split = AnnexBSplitter::new();
    let mut asm = AuAssembler::new(VideoCodec::H264);
    let mut id = 0u32;
    let mut last_capture = 0u64;
    loop {
        tokio::select! {
            ev = events.recv() => match ev {
                Some(Event::Format(d)) => tracing::info!("{d}"),
                Some(Event::Frame(e)) => {
                    for o in complete_au(&mut split, &mut asm, &e.annexb) {
                        let m = match o {
                            Output::Config(sets) => Message::VideoConfig(VideoConfig {
                                codec: VideoCodec::H264,
                                width: e.width,
                                height: e.height,
                                fps_milli: spec.fps * 1000,
                                parameter_sets: sets,
                            }),
                            Output::Frame(au) => {
                                id = id.wrapping_add(1);
                                // The compositor's capture tick when known. The iPad
                                // uses it as the presentation time: keep it increasing.
                                last_capture = e.times.start_ns().max(last_capture + 1);
                                Message::VideoFrame(VideoFrame {
                                    frame_id: id,
                                    keyframe: au.keyframe,
                                    capture_ns: last_capture,
                                    encoded_ns: e.times.encoded_ns,
                                    data: au.sample,
                                })
                            }
                        };
                        if tx.send((m, Some(e.times))).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                Some(Event::Ended(Ok(()))) | None => return Ok(()),
                Some(Event::Ended(Err(e))) if e.unsupported => {
                    return Err(anyhow::Error::new(crate::native::Unsupported(e.msg))
                        .context("native video"))
                }
                Some(Event::Ended(Err(e))) => bail!("native video: {}", e.msg),
            },
            Some(()) = keyframes.recv() => capture.request_keyframe(),
        }
    }
}

/// Open the video channel on an already-connected transport and stream
/// `source` until the iPad closes it or the source ends.
#[allow(clippy::too_many_arguments)]
pub async fn run_video(
    conn: Connected,
    identity: &Identity,
    expect_fp: Fingerprint,
    session_id: u64,
    name: &str,
    source: VideoSource,
    clock_offset: Option<tokio::sync::watch::Receiver<Option<i64>>>,
    input_probe: Option<std::sync::Arc<crate::clock::InputProbe>>,
) -> Result<VideoStats> {
    let (tls, fp) = transport::tls_connect(conn, identity).await?;
    if fp != expect_fp {
        bail!("video channel reached a different iPad than the pen session");
    }
    let (r, w) = tokio::io::split(tls);
    let mut rd = MsgReader::new(r);
    let mut wr = MsgWriter::new(w);
    wr.send(&Message::Hello(Hello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        channel: Channel::Video,
        trusts_peer: true,
        caps: caps::VIDEO,
        session_id,
        name: name.to_string(),
    }))
    .await?;
    match tokio::time::timeout(Duration::from_secs(10), rd.expect()).await?? {
        Message::HelloAck(a) if a.caps & caps::VIDEO != 0 => {}
        Message::HelloAck(_) => bail!("the iPad app does not support video"),
        Message::Goodbye { code, reason } => {
            bail!("iPad refused the video channel: {code:?} {reason}")
        }
        m => bail!("expected HelloAck on the video channel, got {m:?}"),
    }
    tracing::info!("video channel open");

    let (tx, mut rx) = mpsc::channel::<Produced>(8);
    let (keyframe_tx, keyframe_rx) = mpsc::unbounded_channel();
    let mut stages = source.stage_labels().map(StageStats::new);
    // Only native sources can make a keyframe on request.
    let can_keyframe = matches!(
        source,
        VideoSource::Native(NativeSpec {
            kind: NativeKind::Va { .. },
            ..
        })
    );
    let mut keyframe_policy = KeyframePolicy::default();
    // Aborted on drop too: if this future is cancelled (pen session ended),
    // the encoder task and its gst-launch child (kill_on_drop) must go with it.
    let mut producer = AbortOnDrop(tokio::spawn(produce(source, tx, keyframe_rx)));
    let mut stats = VideoStats::default();
    // frame_id -> daemon time the frame left the encoder
    let mut sent_at: std::collections::VecDeque<(u32, u64)> = std::collections::VecDeque::new();
    let mut lat = crate::clock::LatencyStats::new(10_000);
    let mut input_lat = crate::clock::LatencyStats::new(1_000);
    // Pen input -> the compositor's capture tick (native sources only).
    let mut input_tick_lat = crate::clock::LatencyStats::new(1_000);
    let mut ipad_lat = crate::videostats::IpadVideoLatency::new();
    let mut last_log = tokio::time::Instant::now();
    let result = loop {
        tokio::select! {
            m = rx.recv() => {
                let Some((m, times)) = m else {
                    // The source ended: report why.
                    break match (&mut producer.0).await {
                        Ok(Err(e)) => Err(e),
                        _ => Ok(()),
                    };
                };
                if let Message::VideoFrame(f) = &m {
                    stats.frames_sent += 1;
                    stats.keyframes_sent += f.keyframe as u64;
                    stats.bytes_sent += f.data.len() as u64;
                    if f.keyframe {
                        keyframe_policy.on_keyframe_sent(f.frame_id);
                    }
                    sent_at.push_back((f.frame_id, f.encoded_ns));
                    let tick = times.and_then(|t| t.tick_ns);
                    // A re-sent picture (keep-alive, idle keyframe) answers no input.
                    let repeat = times.is_some_and(|t| t.repeat);
                    if let Some((to_tick, d)) = input_probe
                        .as_ref()
                        .filter(|_| !repeat)
                        .and_then(|p| p.on_frame_split(tick, f.encoded_ns))
                    {
                        input_lat.push_ns(d as i64);
                        if let Some(t) = to_tick {
                            input_tick_lat.push_ns(t as i64);
                        }
                    }
                    if sent_at.len() > 600 {
                        sent_at.pop_front();
                    }
                }
                if let Err(e) = wr.send(&m).await {
                    break Err(e);
                }
                if let (Some(st), Some(t)) = (stages.as_mut(), times) {
                    if matches!(m, Message::VideoFrame(_)) {
                        st.on_sent(&t, now_ns());
                    }
                }
            }
            m = rd.next() => match m {
                Ok(Some(Message::VideoFeedback(fb))) => {
                    stats.feedback += 1;
                    let offset = clock_offset.as_ref().and_then(|r| *r.borrow());
                    let t_out = sent_at.iter().find(|(id, _)| *id == fb.frame_id).map(|&(_, t)| t);
                    ipad_lat.push(&fb, t_out, offset);
                    if let (Some(off), Some(t_out)) = (offset, t_out) {
                        lat.push_ns(fb.received_ns as i64 - off - t_out as i64);
                        if last_log.elapsed() >= Duration::from_secs(10) {
                            last_log = tokio::time::Instant::now();
                            let r = lat.report(None);
                            tracing::info!(
                                "video latency (encoder out -> iPad received) p50 {:.1} ms p95 {:.1} ms over {} frames",
                                r.p50_us as f64 / 1000.0, r.p95_us as f64 / 1000.0, r.samples
                            );
                            if let Some(s) = stages.as_mut().and_then(|s| s.report()) {
                                tracing::info!("{s}");
                            }
                            if let Some(s) = ipad_lat.summary() {
                                tracing::info!("{s}");
                            }
                            let to_screen = ipad_lat.encoder_to_screen_p50_us();
                            ipad_lat.clear();
                            let r = input_lat.report(None);
                            if r.samples > 0 {
                                let rt = input_tick_lat.report(None);
                                if rt.samples > 0 {
                                    tracing::info!(
                                        "video latency (pen input -> capture tick, after a pause) p50 {:.1} ms p95 {:.1} ms over {} pauses",
                                        rt.p50_us as f64 / 1000.0, rt.p95_us as f64 / 1000.0, rt.samples
                                    );
                                }
                                tracing::info!(
                                    "video latency (pen input -> frame encoded, after a pause) p50 {:.1} ms p95 {:.1} ms over {} pauses",
                                    r.p50_us as f64 / 1000.0, r.p95_us as f64 / 1000.0, r.samples
                                );
                                if let Some(ts) = to_screen {
                                    tracing::info!(
                                        "video latency (pen input -> on the iPad screen, sum of medians) about {:.0} ms: input -> encoded {:.1} + encoder out -> on screen {:.1}",
                                        (r.p50_us + ts) as f64 / 1000.0, r.p50_us as f64 / 1000.0, ts as f64 / 1000.0
                                    );
                                }
                            }
                        }
                    }
                    if fb.request_keyframe {
                        stats.keyframe_requests += 1;
                        if can_keyframe && keyframe_policy.on_request(fb.frame_id) {
                            tracing::info!("iPad could not show frame {}: asking the encoder for a keyframe", fb.frame_id);
                            let _ = keyframe_tx.send(());
                        } else {
                            tracing::debug!("iPad requested a keyframe (frame {})", fb.frame_id);
                        }
                    }
                }
                Ok(Some(Message::Goodbye { .. })) | Ok(None) => break Ok(()),
                Ok(Some(_)) => {}
                Err(e) => break Err(e),
            }
        }
    };
    producer.0.abort();
    wr.shutdown().await;
    if let Some(s) = stages.as_mut().and_then(|s| s.report()) {
        tracing::info!("{s}");
    }
    if let Some(s) = ipad_lat.summary() {
        tracing::info!("{s}");
    }
    let r = lat.report(None);
    stats.transport_p50_us = r.p50_us;
    stats.transport_p95_us = r.p95_us;
    stats.latency_samples = r.samples;
    tracing::info!(
        "video channel closed: {} frames ({} keyframes), {:.1} MB",
        stats.frames_sent,
        stats.keyframes_sent,
        stats.bytes_sent as f64 / 1e6
    );
    result.map(|_| stats)
}
