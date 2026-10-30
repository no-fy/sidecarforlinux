//! A fake iPad: implements the iPad side of the protocol (TLS server,
//! pairing, clock sync) and replays a pen recording. Used by the
//! integration tests and handy for trying the daemon without an iPad.

use crate::clock::now_ns;
use crate::conn::{MsgReader, MsgWriter};
use crate::identity::{Identity, TrustStore};
use crate::recording::RecEvent;
use crate::tls;
use anyhow::{bail, Context, Result};
use sidecar_protocol::pairing;
use sidecar_protocol::*;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::time::{sleep_until, timeout, Instant};

#[derive(Clone)]
pub struct FakeOpts {
    pub events: Vec<RecEvent>,
    /// Replay speed multiplier (1.0 = real time, 0 = as fast as possible).
    pub speed: f64,
    /// Group samples into one batch per display frame of this rate (Hz),
    /// mimicking UIKit coalescing. 0 = one batch per sample.
    pub frame_hz: f64,
    /// Offset of the fake "iPad clock" from CLOCK_MONOTONIC.
    pub clock_offset_ns: i64,
    pub auto_accept: bool,
    pub name: String,
    /// Keep the session open this long after the replay finished.
    pub linger: Duration,
    pub repeat: bool,
}

impl Default for FakeOpts {
    fn default() -> Self {
        FakeOpts {
            events: Vec::new(),
            speed: 1.0,
            frame_hz: 120.0,
            clock_offset_ns: 1_000_000_000_000,
            auto_accept: true,
            name: "Fake iPad".into(),
            linger: Duration::from_millis(300),
            repeat: false,
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeStats {
    pub paired: bool,
    pub samples_sent: u64,
    pub gestures_sent: u64,
    pub pings_answered: u64,
    pub haptics: Vec<Haptic>,
    pub last_latency: Option<LatencyReport>,
    pub mapping: Option<MappingInfo>,
    pub goodbye: Option<(GoodbyeCode, String)>,
    pub video: VideoRx,
}

/// What the fake iPad received on the video channel.
#[derive(Debug, Default, Clone)]
pub struct VideoRx {
    pub connections: u32,
    pub configs: u32,
    pub frames: u32,
    pub keyframes: u32,
    pub bytes: u64,
    pub refused: u32,
}

/// Serve one video-channel connection: must come from the same daemon and
/// carry the pen session's id.
async fn serve_video(
    tcp: tokio::net::TcpStream,
    id: Identity,
    daemon_fp: [u8; 32],
    session_id: u64,
    rx: std::sync::Arc<std::sync::Mutex<VideoRx>>,
    clock_offset_ns: i64,
) -> Result<()> {
    tcp.set_nodelay(true)?;
    let acceptor = tokio_rustls::TlsAcceptor::from(tls::server_config(&id)?);
    let stream = acceptor.accept(tcp).await?;
    let fp = tls::peer_fingerprint(stream.get_ref().1.peer_certificates())?;
    let (r, w) = tokio::io::split(stream);
    let mut rd = MsgReader::new(r);
    let mut wr = MsgWriter::new(w);
    let ok = match rd.expect().await? {
        Message::Hello(h) => {
            h.channel == Channel::Video && h.session_id == session_id && fp == daemon_fp
        }
        _ => false,
    };
    if !ok {
        rx.lock().unwrap().refused += 1;
        wr.send(&Message::Goodbye {
            code: GoodbyeCode::NotPaired,
            reason: "unknown video session".into(),
        })
        .await?;
        return Ok(());
    }
    rx.lock().unwrap().connections += 1;
    wr.send(&Message::HelloAck(HelloAck {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        trusts_peer: true,
        caps: caps::VIDEO,
        surface: SurfaceInfo {
            width_pt: 1180.0,
            height_pt: 820.0,
            scale: 2.0,
            points_per_inch: 132.0,
        },
        device_name: "Fake iPad".into(),
        model: "fake".into(),
    }))
    .await?;
    while let Some(m) = rd.next().await? {
        match m {
            Message::VideoConfig(_) => rx.lock().unwrap().configs += 1,
            Message::VideoFrame(f) => {
                {
                    let mut v = rx.lock().unwrap();
                    v.frames += 1;
                    v.keyframes += f.keyframe as u32;
                    v.bytes += f.data.len() as u64;
                }
                let t = (now_ns() as i64 + clock_offset_ns) as u64; // fake iPad clock
                wr.send(&Message::VideoFeedback(VideoFeedback {
                    frame_id: f.frame_id,
                    request_keyframe: false,
                    received_ns: t,
                    decoded_ns: t,
                    displayed_ns: t,
                }))
                .await?;
            }
            Message::Goodbye { .. } => break,
            _ => {}
        }
    }
    Ok(())
}

fn ipad_now(o: &FakeOpts) -> u64 {
    (now_ns() as i64 + o.clock_offset_ns) as u64
}

/// Serve exactly one daemon connection.
pub async fn serve_one(
    listener: &TcpListener,
    id: &Identity,
    trust: &mut TrustStore,
    o: &FakeOpts,
) -> Result<FakeStats> {
    let (tcp, peer) = listener.accept().await?;
    tcp.set_nodelay(true)?;
    tracing::info!("fake iPad: connection from {peer}");
    let acceptor = tokio_rustls::TlsAcceptor::from(tls::server_config(id)?);
    let stream = acceptor.accept(tcp).await.context("TLS accept")?;
    let daemon_fp = tls::peer_fingerprint(stream.get_ref().1.peer_certificates())?;
    let (r, w) = tokio::io::split(stream);
    let mut rd = MsgReader::new(r);
    let mut wr = MsgWriter::new(w);
    let mut st = FakeStats::default();

    let hello = match timeout(Duration::from_secs(10), rd.expect()).await?? {
        Message::Hello(h) => h,
        m => bail!("expected Hello, got {m:?}"),
    };
    let surface = o
        .events
        .iter()
        .find_map(|e| match e {
            RecEvent::Surface {
                width_pt,
                height_pt,
                scale,
                points_per_inch,
            } => Some(SurfaceInfo {
                width_pt: *width_pt,
                height_pt: *height_pt,
                scale: *scale,
                points_per_inch: *points_per_inch,
            }),
            _ => None,
        })
        .unwrap_or(SurfaceInfo {
            width_pt: 1180.0,
            height_pt: 820.0,
            scale: 2.0,
            points_per_inch: 132.0,
        });
    let we_trust = trust.is_trusted(&daemon_fp);
    wr.send(&Message::HelloAck(HelloAck {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        trusts_peer: we_trust,
        caps: caps::GESTURES | caps::HAPTICS | caps::ROLL | caps::HOVER,
        surface,
        device_name: o.name.clone(),
        model: "fake".into(),
    }))
    .await?;

    if !(we_trust && hello.trusts_peer) {
        let nonce_i: [u8; 32] = rand::random();
        let my_fp = id.fingerprint();
        wr.send(&Message::PairCommit(pairing::commitment(&my_fp, &nonce_i)))
            .await?;
        let nonce_d = match rd.expect().await? {
            Message::PairNonce(n) => n,
            Message::Goodbye { code, reason } => {
                st.goodbye = Some((code, reason));
                return Ok(st);
            }
            m => bail!("expected PairNonce, got {m:?}"),
        };
        wr.send(&Message::PairReveal(nonce_i)).await?;
        let code = pairing::sas(&daemon_fp, &my_fp, &nonce_d, &nonce_i);
        tracing::info!("fake iPad: pairing code {}", pairing::format_sas(code));
        wr.send(&Message::PairDecision(o.auto_accept)).await?;
        if !o.auto_accept {
            return Ok(st);
        }
        loop {
            match rd.expect().await? {
                Message::PairDecision(true) => break,
                Message::PairDecision(false) => return Ok(st),
                Message::Goodbye { code, reason } => {
                    st.goodbye = Some((code, reason));
                    return Ok(st);
                }
                _ => {}
            }
        }
        trust.add(&daemon_fp, &hello.name)?;
        st.paired = true;
    }

    // Replay schedule: group pen samples per display frame.
    let start = Instant::now() + Duration::from_millis(50);
    let mut idx = 0usize;
    let mut seq = 0u32;
    let mut done_at: Option<Instant> = None;
    let mut pending: Vec<PenSample> = Vec::new();
    let mut pending_deadline: Option<Instant> = None;
    let replay_start_ipad = ipad_now(o) + 50_000_000;
    let at = |t_ms: f64| -> Instant {
        if o.speed <= 0.0 {
            start
        } else {
            start + Duration::from_secs_f64(t_ms / 1000.0 / o.speed)
        }
    };
    let ipad_ts = |t_ms: f64| -> u64 {
        if o.speed <= 0.0 {
            ipad_now(o)
        } else {
            replay_start_ipad + (t_ms / o.speed * 1e6) as u64
        }
    };
    let frame = |t_ms: f64| -> Option<Instant> {
        if o.frame_hz <= 0.0 || o.speed <= 0.0 {
            return None;
        }
        // Display frames are real time: in recording time a frame lasts
        // period * speed.
        let period_ms = 1000.0 / o.frame_hz * o.speed;
        let end_ms = ((t_ms / period_ms).floor() + 1.0) * period_ms;
        Some(at(end_ms))
    };

    let video_rx = std::sync::Arc::new(std::sync::Mutex::new(VideoRx::default()));
    let session_id = hello.session_id;
    loop {
        // Next thing to do: flush a batch, schedule an event, or finish.
        let next_event_at = o.events.get(idx).map(|e| at(e.t_ms()));
        let wake = [
            pending_deadline,
            next_event_at,
            done_at.map(|d| d + o.linger),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            // A second connection during the session = the video channel.
            acc = listener.accept() => {
                if let Ok((tcp, _)) = acc {
                    let (idc, rx, off) = (id.clone(), video_rx.clone(), o.clock_offset_ns);
                    tokio::spawn(async move {
                        if let Err(e) = serve_video(tcp, idc, daemon_fp, session_id, rx, off).await {
                            tracing::debug!("fake iPad video channel: {e:#}");
                        }
                    });
                }
            }
            m = rd.next() => {
                let Some(m) = m? else { break };
                match m {
                    Message::Ping(p) => {
                        let t2 = ipad_now(o);
                        wr.send(&Message::Pong(Pong { id: p.id, t1_ns: p.t1_ns, t2_ns: t2, t3_ns: ipad_now(o) })).await?;
                        st.pings_answered += 1;
                    }
                    Message::Haptic(h) => {
                        tracing::info!("fake iPad: haptic {:?}", h.kind);
                        st.haptics.push(h);
                    }
                    Message::LatencyReport(l) => st.last_latency = Some(l),
                    Message::MappingInfo(mi) => st.mapping = Some(mi),
                    Message::Goodbye { code, reason } => {
                        st.goodbye = Some((code, reason));
                        break;
                    }
                    _ => {}
                }
            }
            _ = sleep_until(wake) => {
                let now = Instant::now();
                if pending_deadline.is_some_and(|d| d <= now) || (pending_deadline.is_none() && !pending.is_empty()) {
                    let batch = PenBatch { send_time_ns: ipad_now(o), samples: std::mem::take(&mut pending) };
                    st.samples_sent += batch.samples.len() as u64;
                    wr.send(&Message::PenBatch(batch)).await?;
                    pending_deadline = None;
                }
                while let Some(e) = o.events.get(idx) {
                    if at(e.t_ms()) > Instant::now() {
                        break;
                    }
                    idx += 1;
                    let ts = ipad_ts(e.t_ms());
                    if let Some(s) = e.to_sample(ts, seq)? {
                        seq += 1;
                        pending.push(s);
                        match frame(e.t_ms()) {
                            Some(d) => { pending_deadline.get_or_insert(d); }
                            None => {
                                let batch = PenBatch { send_time_ns: ipad_now(o), samples: std::mem::take(&mut pending) };
                                st.samples_sent += batch.samples.len() as u64;
                                wr.send(&Message::PenBatch(batch)).await?;
                            }
                        }
                    } else if let Some(g) = e.to_gesture(ts)? {
                        st.gestures_sent += 1;
                        wr.send(&Message::PencilGesture(g)).await?;
                    }
                }
                if idx >= o.events.len() && pending.is_empty() && done_at.is_none() {
                    done_at = Some(Instant::now());
                }
                if let Some(d) = done_at {
                    if Instant::now() >= d + o.linger {
                        if o.repeat {
                            // Loop forever (CLI use).
                            return Box::pin(continue_repeat(st)).await;
                        }
                        wr.send(&Message::Goodbye { code: GoodbyeCode::Normal, reason: "replay finished".into() }).await?;
                        break;
                    }
                }
            }
        }
    }
    wr.shutdown().await;
    st.video = video_rx.lock().unwrap().clone();
    Ok(st)
}

async fn continue_repeat(st: FakeStats) -> Result<FakeStats> {
    // Simplest correct behaviour for --loop: end this session; the CLI loop
    // accepts the daemon's reconnection and replays again.
    Ok(st)
}
