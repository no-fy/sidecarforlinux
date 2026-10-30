//! Phase 2: the video channel. A second TLS connection to the iPad
//! (`Hello.channel = Video`, same `session_id` as the pen session) that
//! carries `VideoConfig` + `VideoFrame` messages from an encoder.
//!
//! Encoders run as a subprocess writing H.264 Annex-B to stdout
//! (GStreamer `gst-launch-1.0`), which keeps the daemon free of GStreamer
//! bindings and makes the encoder easy to swap (VA-API / x264 / test
//! pattern). The pipelines for real hardware are built but untested; see
//! docs/phase2-virtual-display.md.

use crate::clock::now_ns;
use crate::conn::{MsgReader, MsgWriter};
use crate::identity::Identity;
use crate::transport::{self, Connected};
use crate::video::{AnnexBSplitter, AuAssembler, Output};
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
    /// Any command that writes H.264 Annex-B to stdout.
    Command {
        argv: Vec<String>,
        width: u32,
        height: u32,
        fps: u32,
    },
}

const X264: &str =
    "x264enc tune=zerolatency speed-preset=ultrafast bframes=0 aud=true byte-stream=true";

fn tail(fps: u32) -> Vec<String> {
    // Keyframe every 2 s so a decoder that joins late recovers quickly.
    vec![
        format!("key-int-max={}", fps * 2),
        "!".into(),
        "h264parse".into(),
        "config-interval=-1".into(),
        "!".into(),
        "video/x-h264,stream-format=byte-stream,alignment=au".into(),
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
    }
}

/// Capture a PipeWire node (e.g. KWin's virtual output stream) and encode it.
pub fn pipewire(
    node: u32,
    width: u32,
    height: u32,
    fps: u32,
    encoder: Encoder,
    kbps: u32,
) -> VideoSource {
    let mut argv = split("gst-launch-1.0 -q");
    argv.extend([
        "pipewiresrc".into(),
        format!("path={node}"),
        "do-timestamp=true".into(),
        "keepalive-time=1000".into(),
        "!".into(),
    ]);
    argv.extend(split("queue max-size-buffers=1 leaky=downstream !"));
    match encoder {
        Encoder::Vaapi => {
            argv.extend(split("vapostproc !"));
            argv.push("video/x-raw(memory:VAMemory),format=NV12".into());
            argv.push("!".into());
            argv.extend(split(
                "vah264enc b-frames=0 rate-control=cbr target-usage=3",
            ));
            argv.push(format!("bitrate={kbps}"));
        }
        Encoder::X264 => {
            argv.extend(split("videoconvert ! video/x-raw,format=I420 !"));
            argv.extend(split(X264));
            argv.push(format!("bitrate={kbps}"));
        }
    }
    argv.extend(tail(fps));
    VideoSource::Command {
        argv,
        width,
        height,
        fps,
    }
}

#[derive(Debug, Default, Clone)]
pub struct VideoStats {
    pub frames_sent: u64,
    pub keyframes_sent: u64,
    pub bytes_sent: u64,
    pub feedback: u64,
    pub keyframe_requests: u64,
}

/// Messages produced by a source, re-stamped with the daemon clock.
async fn produce(source: VideoSource, tx: mpsc::Sender<Message>) -> Result<()> {
    match source {
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
                    if tx.send(m).await.is_err() {
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
            let mut buf = vec![0u8; 256 * 1024];
            let mut id = 0u32;
            loop {
                let n = out.read(&mut buf).await?;
                if n == 0 {
                    let status = child.wait().await?;
                    bail!("video encoder exited ({status})");
                }
                for nal in split.push(&buf[..n]) {
                    for o in asm.push(nal) {
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
                        if tx.send(m).await.is_err() {
                            return Ok(());
                        }
                    }
                }
            }
        }
    }
}

/// Open the video channel on an already-connected transport and stream
/// `source` until the iPad closes it or the source ends.
pub async fn run_video(
    conn: Connected,
    identity: &Identity,
    expect_fp: Fingerprint,
    session_id: u64,
    name: &str,
    source: VideoSource,
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

    let (tx, mut rx) = mpsc::channel::<Message>(8);
    let mut producer = tokio::spawn(produce(source, tx));
    let mut stats = VideoStats::default();
    let result = loop {
        tokio::select! {
            m = rx.recv() => {
                let Some(m) = m else {
                    // The source ended: report why.
                    break match (&mut producer).await {
                        Ok(Err(e)) => Err(e),
                        _ => Ok(()),
                    };
                };
                if let Message::VideoFrame(f) = &m {
                    stats.frames_sent += 1;
                    stats.keyframes_sent += f.keyframe as u64;
                    stats.bytes_sent += f.data.len() as u64;
                }
                if let Err(e) = wr.send(&m).await {
                    break Err(e);
                }
            }
            m = rd.next() => match m {
                Ok(Some(Message::VideoFeedback(fb))) => {
                    stats.feedback += 1;
                    if fb.request_keyframe {
                        stats.keyframe_requests += 1;
                        tracing::debug!("iPad requested a keyframe (frame {})", fb.frame_id);
                    }
                }
                Ok(Some(Message::Goodbye { .. })) | Ok(None) => break Ok(()),
                Ok(Some(_)) => {}
                Err(e) => break Err(e),
            }
        }
    };
    producer.abort();
    wr.shutdown().await;
    tracing::info!(
        "video channel closed: {} frames ({} keyframes), {:.1} MB",
        stats.frames_sent,
        stats.keyframes_sent,
        stats.bytes_sent as f64 / 1e6
    );
    result.map(|_| stats)
}
