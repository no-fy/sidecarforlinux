//! One daemon ↔ iPad session on the input channel: hello, pairing,
//! pen/gesture injection, clock sync, latency stats, haptics.

use crate::clock::{now_ns, ClockSync, LatencyStats};
use crate::conn::{MsgReader, MsgWriter};
use crate::events::DeviceKind;
use crate::gestures::{self, GestureConfig};
use crate::identity::{Identity, TrustStore};
use crate::recording::Recorder;
use crate::sink::{DeviceOptions, EventSink};
use crate::tablet::{MapperConfig, PenMapper, RateLimiter, TabletGeometry, Tool};
use crate::touch::{TouchConfig, TouchMapper, TouchStats};
use anyhow::{anyhow, bail, Context, Result};
use sidecar_protocol::pairing::{self, Fingerprint};
use sidecar_protocol::*;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::{timeout, Instant};

/// How the daemon reacts when the iPad isn't paired yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingMode {
    /// Refuse (normal `run`): unknown iPads get Goodbye(NotPaired).
    Refuse,
    /// Ask on the terminal (`sidecard pair`).
    Terminal,
    /// Accept automatically (tests only).
    AutoAccept,
}

pub type SinkFactory =
    Box<dyn FnMut(TabletGeometry, &DeviceOptions) -> Result<Box<dyn EventSink>> + Send>;

pub struct SessionCtx {
    pub identity: Identity,
    pub trust: TrustStore,
    pub pairing: PairingMode,
    pub name: String,
    pub mapper_cfg: MapperConfig,
    pub gesture_cfg: GestureConfig,
    /// `touch`: create the touchscreen if the iPad sends touches.
    pub device_opts: DeviceOptions,
    pub touch_cfg: TouchConfig,
    pub haptic_on_tool_switch: bool,
    pub sink_factory: SinkFactory,
    /// End the session after this long (latency tool, tests).
    pub max_duration: Option<Duration>,
    /// End the session once this many pen samples were injected (tests).
    pub max_samples: Option<u64>,
    pub recorder: Option<Recorder>,
    /// Told once the session is authenticated (pairing done, tablet created):
    /// the video channel waits for this.
    pub on_active: Option<tokio::sync::watch::Sender<Option<ActiveInfo>>>,
    /// Latest iPad−daemon clock offset (ns), for the video channel's latency stats.
    pub clock_offset_tx: Option<tokio::sync::watch::Sender<Option<i64>>>,
    /// Pen input → encoded frame measurement, shared with the video channel.
    pub input_probe: Option<std::sync::Arc<crate::clock::InputProbe>>,
    /// Filled during the session.
    pub stats: SessionStats,
}

/// What other channels need to know about an active input session.
#[derive(Debug, Clone)]
pub struct ActiveInfo {
    pub surface: SurfaceInfo,
    pub peer_fp: Fingerprint,
    pub dev_nodes: Vec<std::path::PathBuf>,
}

pub struct SessionStats {
    /// Touch timestamp on the iPad → written to uinput.
    pub input_latency: LatencyStats,
    /// Batch handed to the iPad network stack → received by the daemon.
    pub transport_latency: LatencyStats,
    pub samples: u64,
    pub batches: u64,
    pub gestures: u64,
    /// Finger touches received (TouchBatch samples).
    pub touches: u64,
    pub rtt_ns: Option<i64>,
    pub clock_offset_ns: Option<i64>,
    pub peer_name: String,
    pub peer_model: String,
    pub dev_nodes: Vec<std::path::PathBuf>,
}

impl Default for SessionStats {
    fn default() -> Self {
        SessionStats {
            input_latency: LatencyStats::new(100_000),
            transport_latency: LatencyStats::new(100_000),
            samples: 0,
            batches: 0,
            gestures: 0,
            touches: 0,
            rtt_ns: None,
            clock_offset_ns: None,
            peer_name: String::new(),
            peer_model: String::new(),
            dev_nodes: Vec::new(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SessionEnd {
    PeerClosed,
    Goodbye(GoodbyeCode, String),
    TimeUp,
    SampleLimit,
}

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const PAIRING_TIMEOUT: Duration = Duration::from_secs(120);
const LIVENESS_TIMEOUT: Duration = Duration::from_secs(5);

async fn prompt_terminal(code: String, peer: String, fp: String) -> bool {
    tokio::task::spawn_blocking(move || {
        use std::io::{BufRead, Write};
        println!();
        println!("  Pairing request from \"{peer}\"");
        println!("  iPad certificate: {fp}");
        println!();
        println!("      Code:  {code}");
        println!();
        print!("  Does the iPad show the same code? Accept there too. [y/N] ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let _ = std::io::stdin().lock().read_line(&mut line);
        matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    })
    .await
    .unwrap_or(false)
}

/// Runs the pairing exchange as the daemon. Returns Ok(()) once both sides
/// accepted and the iPad is pinned.
async fn pair<R, W>(
    ctx: &mut SessionCtx,
    rd: &mut MsgReader<R>,
    wr: &mut MsgWriter<W>,
    peer_fp: &Fingerprint,
    peer_name: &str,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    if ctx.pairing == PairingMode::Refuse {
        wr.send(&Message::Goodbye {
            code: GoodbyeCode::NotPaired,
            reason: "This iPad is not paired with this PC yet. Run `sidecard pair` on the PC."
                .into(),
        })
        .await?;
        bail!(
            "iPad \"{peer_name}\" ({}) is not paired; run `sidecard pair`",
            pairing::format_fingerprint(peer_fp)
        );
    }
    let commit = match timeout(PAIRING_TIMEOUT, rd.expect()).await?? {
        Message::PairCommit(c) => c,
        Message::Goodbye { code, reason } => bail!("iPad ended pairing: {code:?} {reason}"),
        m => bail!("expected PairCommit, got {m:?}"),
    };
    let nonce_d: [u8; 32] = rand::random();
    wr.send(&Message::PairNonce(nonce_d)).await?;
    let nonce_i = match timeout(PAIRING_TIMEOUT, rd.expect()).await?? {
        Message::PairReveal(n) => n,
        m => bail!("expected PairReveal, got {m:?}"),
    };
    if pairing::commitment(peer_fp, &nonce_i) != commit {
        wr.send(&Message::Goodbye {
            code: GoodbyeCode::PairingRejected,
            reason: "commitment mismatch".into(),
        })
        .await?;
        bail!("pairing commitment mismatch: possible man-in-the-middle, aborting");
    }
    let code = pairing::sas(&ctx.identity.fingerprint(), peer_fp, &nonce_d, &nonce_i);
    let code_s = pairing::format_sas(code);
    tracing::info!("pairing code {code_s}");
    let mut their_decision: Option<bool> = None;
    let ours = match ctx.pairing {
        PairingMode::AutoAccept => true,
        PairingMode::Refuse => false,
        PairingMode::Terminal => {
            let prompt = prompt_terminal(
                code_s.clone(),
                peer_name.to_string(),
                pairing::format_fingerprint(peer_fp),
            );
            tokio::pin!(prompt);
            loop {
                tokio::select! {
                    ok = &mut prompt => break ok,
                    m = rd.expect() => match m? {
                        Message::PairDecision(false) => {
                            println!("\n  The iPad rejected the pairing.");
                            bail!("pairing rejected on the iPad");
                        }
                        Message::PairDecision(true) => { their_decision = Some(true); }
                        Message::Goodbye { code, reason } => bail!("iPad ended pairing: {code:?} {reason}"),
                        _ => {}
                    }
                }
            }
        }
    };
    wr.send(&Message::PairDecision(ours)).await?;
    if !ours {
        bail!("pairing rejected on the PC");
    }
    let theirs = match their_decision {
        Some(d) => d,
        None => loop {
            match timeout(PAIRING_TIMEOUT, rd.expect()).await?? {
                Message::PairDecision(d) => break d,
                Message::Goodbye { code, reason } => bail!("iPad ended pairing: {code:?} {reason}"),
                _ => continue,
            }
        },
    };
    if !theirs {
        bail!("pairing rejected on the iPad");
    }
    ctx.trust.add(peer_fp, peer_name)?;
    tracing::info!(
        "paired with \"{peer_name}\" ({})",
        pairing::format_fingerprint(peer_fp)
    );
    if matches!(ctx.pairing, PairingMode::Terminal) {
        println!(
            "Paired. This command keeps running as the pen session (Ctrl-C to stop).\n\
             From now on, use `sidecard run` (or the user service) instead of `sidecard pair`."
        );
    }
    Ok(())
}

/// Drive a full session over an established TLS stream.
pub async fn run_session<S>(
    tls: S,
    peer_fp: Fingerprint,
    session_id: u64,
    ctx: &mut SessionCtx,
) -> Result<SessionEnd>
where
    S: AsyncRead + AsyncWrite + Unpin + Send,
{
    let (r, w) = tokio::io::split(tls);
    let mut rd = MsgReader::new(r);
    let mut wr = MsgWriter::new(w);
    let we_trust = ctx.trust.is_trusted(&peer_fp);

    wr.send(&Message::Hello(Hello {
        major: PROTOCOL_MAJOR,
        minor: PROTOCOL_MINOR,
        channel: Channel::Input,
        trusts_peer: we_trust,
        caps: caps::GESTURES
            | caps::HAPTICS
            | if ctx.device_opts.touch {
                caps::TOUCH
            } else {
                0
            },
        session_id,
        name: ctx.name.clone(),
    }))
    .await?;

    let ack = match timeout(HANDSHAKE_TIMEOUT, rd.expect())
        .await
        .context("waiting for HelloAck")??
    {
        Message::HelloAck(a) => a,
        Message::Goodbye { code, reason } => return Ok(SessionEnd::Goodbye(code, reason)),
        m => bail!("expected HelloAck, got {m:?}"),
    };
    if ack.major != PROTOCOL_MAJOR {
        wr.send(&Message::Goodbye {
            code: GoodbyeCode::VersionMismatch,
            reason: "protocol major mismatch".into(),
        })
        .await?;
        bail!(
            "iPad speaks protocol {}.{}, we speak {PROTOCOL_MAJOR}.{PROTOCOL_MINOR}",
            ack.major,
            ack.minor
        );
    }
    ctx.stats.peer_name = ack.device_name.clone();
    ctx.stats.peer_model = ack.model.clone();
    tracing::info!(
        "connected to \"{}\" ({}), protocol {}.{}, surface {}x{} pt",
        ack.device_name,
        ack.model,
        ack.major,
        ack.minor,
        ack.surface.width_pt,
        ack.surface.height_pt
    );

    if !(we_trust && ack.trusts_peer) {
        pair(ctx, &mut rd, &mut wr, &peer_fp, &ack.device_name).await?;
    }

    let geo = TabletGeometry::from_surface(&ack.surface);
    // The touchscreen only if both sides want it.
    let mut opts = ctx.device_opts.clone();
    opts.touch = opts.touch && ack.caps & caps::TOUCH != 0;
    let mut sink = (ctx.sink_factory)(geo, &opts)?;
    ctx.stats.dev_nodes = sink.dev_nodes();
    for n in &ctx.stats.dev_nodes {
        tracing::info!("virtual device node {}", n.display());
    }
    let mapper = PenMapper::new(geo, ctx.mapper_cfg.clone());
    if let Some(rec) = ctx.recorder.as_mut() {
        rec.surface(&ack.surface)?;
    }
    wr.send(&Message::MappingInfo(MappingInfo {
        output_name: String::new(),
        area: ctx.mapper_cfg.area.map(|v| v as f32),
    }))
    .await?;

    if let Some(tx) = &ctx.on_active {
        let _ = tx.send(Some(ActiveInfo {
            surface: ack.surface,
            peer_fp,
            dev_nodes: ctx.stats.dev_nodes.clone(),
        }));
    }

    if opts.touch {
        tracing::info!(
            "finger touches: on (palms over {} pt and fingers while the pencil is near are ignored)",
            ctx.touch_cfg.palm_radius_pt
        );
    }
    let touch = opts
        .touch
        .then(|| TouchMapper::new(geo, ctx.touch_cfg.clone()));
    let mut live = Live {
        sink,
        mapper,
        touch,
        opts,
        rate: RateLimiter::new(ctx.mapper_cfg.max_rate_hz),
        geo,
        clock: ClockSync::default(),
        peer_caps: ack.caps,
    };
    let res = session_loop(ctx, &mut rd, &mut wr, &mut live).await;
    // Whatever happened, never leave the pen touching or a button pressed.
    if let Err(e) = live
        .sink
        .emit_all(DeviceKind::Pen, &live.mapper.release_all())
    {
        tracing::warn!("releasing pen state failed: {e}");
    }
    if let Some(t) = live.touch.as_mut() {
        if let Err(e) = live.sink.emit_all(DeviceKind::Touch, &t.release_all()) {
            tracing::warn!("releasing touches failed: {e}");
        }
    }
    if matches!(res, Ok(SessionEnd::TimeUp | SessionEnd::SampleLimit)) {
        let _ = wr
            .send(&Message::Goodbye {
                code: GoodbyeCode::Normal,
                reason: "bye".into(),
            })
            .await;
    }
    wr.shutdown().await;
    res
}

struct Live {
    sink: Box<dyn EventSink>,
    mapper: PenMapper,
    touch: Option<TouchMapper>,
    /// The device options this session uses (touch decided at hello).
    opts: DeviceOptions,
    rate: RateLimiter,
    geo: TabletGeometry,
    clock: ClockSync,
    peer_caps: u32,
}

async fn session_loop<R, W>(
    ctx: &mut SessionCtx,
    rd: &mut MsgReader<R>,
    wr: &mut MsgWriter<W>,
    live: &mut Live,
) -> Result<SessionEnd>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut ticks: u64 = 0;
    let mut ping_id: u32 = 0;
    let started = Instant::now();
    let mut last_rx = Instant::now();
    let mut last_log = Instant::now();
    let mut logged_touch = TouchStats::default();

    loop {
        tokio::select! {
            m = rd.next() => {
                let Some(m) = m? else { return Ok(SessionEnd::PeerClosed) };
                last_rx = Instant::now();
                match m {
                    Message::PenBatch(b) => {
                        let t_rx = now_ns();
                        ctx.stats.batches += 1;
                        if let Some(l) = live.clock.to_local(b.send_time_ns) {
                            ctx.stats.transport_latency.push_ns(t_rx as i64 - l);
                        }
                        let keep = live.rate.select(&b.samples);
                        for (s, keep) in b.samples.iter().zip(keep) {
                            if !keep {
                                continue;
                            }
                            let frames = live.mapper.sample(s);
                            live.sink.emit_all(DeviceKind::Pen, &frames)?;
                            if frames.is_empty() {
                                continue;
                            }
                            let t_inj = now_ns();
                            ctx.stats.samples += 1;
                            if let Some(p) = &ctx.input_probe {
                                p.on_input(t_inj);
                            }
                            if !s.has(sample_flags::PREDICTED) {
                                if let Some(l) = live.clock.to_local(s.timestamp_ns) {
                                    ctx.stats.input_latency.push_ns(t_inj as i64 - l);
                                }
                            }
                            if let Some(rec) = ctx.recorder.as_mut() {
                                rec.pen(s)?;
                            }
                        }
                        // Pencil near: fingers stop (palm rejection).
                        if let Some(t) = live.touch.as_mut() {
                            let frames = t.set_pen_near(live.mapper.in_proximity());
                            live.sink.emit_all(DeviceKind::Touch, &frames)?;
                        }
                        if ctx.max_samples.is_some_and(|m| ctx.stats.samples >= m) {
                            return Ok(SessionEnd::SampleLimit);
                        }
                    }
                    Message::PencilGesture(g) => {
                        ctx.stats.gestures += 1;
                        let out = gestures::handle(&ctx.gesture_cfg, &mut live.mapper, &g);
                        // Logged at info: "did the gesture reach the PC?" is the
                        // first question when one seems to do nothing.
                        let action = match g.kind {
                            GestureKind::DoubleTap => &ctx.gesture_cfg.double_tap,
                            GestureKind::Squeeze => &ctx.gesture_cfg.squeeze,
                        };
                        let msg = format!(
                            "pencil {:?} {:?} (iPadOS preferred action {}) -> {}, {} pen / {} key events",
                            g.kind,
                            g.phase,
                            g.preferred_action,
                            action.describe(),
                            out.pen_frames.len(),
                            out.key_frames.len()
                        );
                        // A squeeze sends ~15 Changed updates; keep those out of the info log.
                        if g.phase == GesturePhase::Changed {
                            tracing::debug!("{msg}");
                        } else {
                            tracing::info!("{msg}");
                        }
                        live.sink.emit_all(DeviceKind::Pen, &out.pen_frames)?;
                        live.sink.emit_all(DeviceKind::Keys, &out.key_frames)?;
                        if let Some(tool) = out.tool_changed {
                            tracing::info!("tool -> {}", if tool == Tool::Eraser { "eraser" } else { "pen" });
                            if ctx.haptic_on_tool_switch && live.peer_caps & caps::HAPTICS != 0 {
                                wr.send(&Message::Haptic(Haptic { kind: HapticKind::Alignment, x: f32::NAN, y: f32::NAN })).await?;
                            }
                        }
                        if let Some(rec) = ctx.recorder.as_mut() {
                            rec.gesture(&g)?;
                        }
                    }
                    Message::TouchBatch(b) => {
                        ctx.stats.touches += b.touches.len() as u64;
                        if let Some(t) = live.touch.as_mut() {
                            let frames = t.batch(&b.touches);
                            live.sink.emit_all(DeviceKind::Touch, &frames)?;
                        }
                    }
                    Message::SurfaceInfo(s) => {
                        let ng = TabletGeometry::from_surface(&s);
                        if ng != live.geo {
                            tracing::info!("surface changed to {}x{} pt, recreating tablet device", s.width_pt, s.height_pt);
                            live.sink.emit_all(DeviceKind::Pen, &live.mapper.release_all())?;
                            if let Some(t) = live.touch.as_mut() {
                                live.sink.emit_all(DeviceKind::Touch, &t.release_all())?;
                            }
                            live.geo = ng;
                            live.sink = (ctx.sink_factory)(ng, &live.opts)?;
                            ctx.stats.dev_nodes = live.sink.dev_nodes();
                            live.mapper = PenMapper::new(ng, ctx.mapper_cfg.clone());
                            if live.touch.is_some() {
                                live.touch = Some(TouchMapper::new(ng, ctx.touch_cfg.clone()));
                            }
                        }
                        if let Some(rec) = ctx.recorder.as_mut() {
                            rec.surface(&s)?;
                        }
                    }
                    Message::Pong(p) => {
                        live.clock.on_pong(&p, now_ns());
                        ctx.stats.rtt_ns = live.clock.rtt_ns();
                        ctx.stats.clock_offset_ns = live.clock.offset_ns();
                        if let Some(tx) = &ctx.clock_offset_tx {
                            let _ = tx.send(live.clock.offset_ns());
                        }
                    }
                    Message::Goodbye { code, reason } => return Ok(SessionEnd::Goodbye(code, reason)),
                    Message::Unknown { ty, .. } => tracing::debug!("ignoring unknown message 0x{ty:02x}"),
                    other => tracing::debug!("ignoring {other:?}"),
                }
            }
            _ = tick.tick() => {
                ticks += 1;
                // Burst of pings at start for a quick offset, then 1 Hz.
                if ticks <= 10 || ticks % 10 == 0 {
                    ping_id = ping_id.wrapping_add(1);
                    wr.send(&Message::Ping(Ping { id: ping_id, t1_ns: now_ns() })).await?;
                }
                if ticks % 10 == 0 {
                    let rep = ctx.stats.input_latency.report(live.clock.rtt_ns());
                    wr.send(&Message::LatencyReport(rep)).await?;
                    if last_log.elapsed() >= Duration::from_secs(10) && rep.samples > 0 {
                        last_log = Instant::now();
                        tracing::info!(
                            "latency (touch->uinput) p50 {:.1} ms p95 {:.1} ms p99 {:.1} ms, rtt {:.2} ms, {} samples",
                            rep.p50_us as f64 / 1000.0, rep.p95_us as f64 / 1000.0, rep.p99_us as f64 / 1000.0,
                            rep.rtt_us as f64 / 1000.0, ctx.stats.samples
                        );
                    }
                }
                if ticks % 100 == 0 {
                    if let Some(t) = &live.touch {
                        if t.stats != logged_touch {
                            logged_touch = t.stats;
                            tracing::info!(
                                "finger touches so far: {} fingers, {} palms ignored, {} ignored while the pencil was near, {} cancelled",
                                t.stats.fingers, t.stats.palms, t.stats.pen_near, t.stats.cancelled
                            );
                        }
                    }
                }
                if last_rx.elapsed() > LIVENESS_TIMEOUT {
                    return Err(anyhow!("iPad stopped responding (no data for {LIVENESS_TIMEOUT:?})"));
                }
                if ctx.max_duration.is_some_and(|d| started.elapsed() >= d) {
                    return Ok(SessionEnd::TimeUp);
                }
            }
        }
    }
}
