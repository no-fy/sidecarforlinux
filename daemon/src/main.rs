use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use sidecar_protocol::pairing;
use sidecard::config::{self, Config};
use sidecard::events::DeviceKind;
use sidecard::fake_ipad::{self, FakeOpts};
use sidecard::identity::{Identity, TrustStore};
use sidecard::recording::{self, RecEvent, Recorder};
use sidecard::session::{self, PairingMode, SessionCtx, SessionEnd, SessionStats, SinkFactory};
#[cfg(target_os = "linux")]
use sidecard::sink::UinputSink;
use sidecard::sink::{DeviceOptions, EventSink, RecordingSink};
use sidecard::tablet::{PenMapper, TabletGeometry};
use sidecard::transport::{self, Transport};
use sidecard::{gestures, kde, usbmux, videochan};
use std::path::PathBuf;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "sidecard",
    version,
    about = "iPad + Apple Pencil Pro as a pen tablet for Linux"
)]
struct Cli {
    /// Config file (default: ~/.config/sidecarforlinux/config.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// More logging (-v debug, -vv trace).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Args, Clone)]
struct ConnArgs {
    /// auto | usb | wifi | tcp
    #[arg(long)]
    transport: Option<String>,
    /// iPad UDID (USB) when several are attached.
    #[arg(long)]
    udid: Option<String>,
    /// HOST:PORT for --transport tcp (e.g. the fake iPad).
    #[arg(long)]
    connect: Option<String>,
    /// Print events instead of creating uinput devices.
    #[arg(long)]
    dry_run: bool,
    /// Phase 2 (experimental) video: off | virtual | test-pattern | file:PATH
    /// (default: [video] mode in config.toml).
    #[arg(long)]
    video: Option<String>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Connect to the iPad and run the pen tablet (reconnects forever).
    Run {
        #[command(flatten)]
        conn: ConnArgs,
        /// Allow pairing a new iPad (asks on this terminal).
        #[arg(long)]
        pair: bool,
        /// Record every received pen event to this JSONL file.
        #[arg(long)]
        record: Option<PathBuf>,
        /// Exit after one session instead of reconnecting.
        #[arg(long)]
        once: bool,
    },
    /// Pair with an iPad (shows a code to compare), then keep running.
    Pair {
        #[command(flatten)]
        conn: ConnArgs,
    },
    /// Measure pen latency (touch on the iPad → event written to uinput).
    Latency {
        #[command(flatten)]
        conn: ConnArgs,
        /// Seconds to measure; draw on the iPad meanwhile.
        #[arg(long, default_value_t = 30)]
        duration: u64,
        /// Write the raw report as JSON here.
        #[arg(long)]
        json: Option<PathBuf>,
    },
    /// List iPads reachable over USB (usbmuxd) and Wi-Fi (mDNS).
    Devices,
    /// Inject a recording straight into a virtual tablet (no iPad needed).
    Replay {
        file: PathBuf,
        /// Replay speed multiplier; 0 = as fast as possible.
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        #[arg(long)]
        dry_run: bool,
        /// Repeat forever.
        #[arg(long = "loop")]
        repeat: bool,
        /// Wait this many seconds after creating the device (time to start
        /// evtest / libinput debug-events / Krita).
        #[arg(long, default_value_t = 1.0)]
        settle: f64,
    },
    /// Pretend to be the iPad app: TLS server that replays a recording.
    FakeIpad {
        #[arg(long, default_value = "127.0.0.1:47811")]
        listen: String,
        /// Recording to replay (default: built-in demo).
        #[arg(long)]
        replay: Option<PathBuf>,
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        /// Serve connections forever.
        #[arg(long = "loop")]
        repeat: bool,
        /// Separate identity/trust directory for the fake iPad.
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Reject pairing instead of accepting it.
        #[arg(long)]
        reject_pairing: bool,
    },
    /// Write the synthetic demo recording (every pencil signal) to stdout.
    GenDemo,
    /// Show identity fingerprint, config and paired iPads.
    Info,
    /// Forget a paired iPad (by name or fingerprint prefix).
    Unpair { who: String },
    /// Print an example config file.
    ExampleConfig,
    /// Phase 2 probe: ask KWin for a virtual monitor sized like the iPad,
    /// print its PipeWire node, keep it for a while, optionally pin the pen.
    #[cfg(feature = "display")]
    DisplayProbe {
        #[arg(long, default_value_t = 2360)]
        width: i32,
        #[arg(long, default_value_t = 1640)]
        height: i32,
        #[arg(long, default_value_t = 2.0)]
        scale: f64,
        #[arg(long, default_value = "ipad")]
        name: String,
        /// Keep the output this many seconds (0 = until Ctrl-C).
        #[arg(long, default_value_t = 30)]
        seconds: u64,
        /// Instead of creating a virtual monitor, stream this existing
        /// output (testing; e.g. "Virtual-0" in a headless KWin).
        #[arg(long)]
        existing: Option<String>,
    },
}

fn init_logging(v: u8) {
    let default = match v {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    let filter = tracing_subscriber::EnvFilter::try_from_env("SIDECAR_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(format!("sidecard={default},warn")));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "linux".into())
}

struct Env {
    cfg: Config,
    dir: PathBuf,
}

impl Env {
    fn load(cli: &Cli) -> Result<Env> {
        let dir = config::config_dir();
        let path = cli
            .config
            .clone()
            .unwrap_or_else(|| dir.join("config.toml"));
        Ok(Env {
            cfg: Config::load(&path)?,
            dir,
        })
    }
    fn identity(&self) -> Result<Identity> {
        Identity::load_or_create(&self.dir, &format!("sidecard on {}", hostname()))
    }
    fn trust(&self) -> Result<TrustStore> {
        TrustStore::load(&self.dir.join("trusted.toml"))
    }
    fn transport(&self, a: &ConnArgs) -> Result<Transport> {
        let kind = a.transport.clone().unwrap_or_else(|| {
            if a.connect.is_some() {
                "tcp".into()
            } else {
                self.cfg.connection.transport.clone()
            }
        });
        let udid = a
            .udid
            .clone()
            .or_else(|| Some(self.cfg.connection.udid.clone()).filter(|u| !u.is_empty()));
        let addr = a
            .connect
            .clone()
            .or_else(|| Some(self.cfg.connection.address.clone()).filter(|u| !u.is_empty()));
        Transport::parse(&kind, udid, addr)
    }
}

fn sink_factory(dry_run: bool, kde_output: String) -> SinkFactory {
    Box::new(
        move |geo: TabletGeometry, opts: &DeviceOptions| -> Result<Box<dyn EventSink>> {
            if dry_run {
                tracing::info!(
                    "dry run: printing events, tablet {}x{} @{}/mm",
                    geo.x_max,
                    geo.y_max,
                    geo.resolution
                );
                return Ok(Box::new(RecordingSink {
                    frames: Vec::new(),
                    print: true,
                }));
            }
            #[cfg(target_os = "linux")]
            {
                let mut s = UinputSink::create(geo, opts)?;
                if !kde_output.is_empty() {
                    let nodes = s.dev_nodes();
                    if let Err(e) = kde::map_to_output(&nodes, &kde_output) {
                        tracing::warn!(
                            "could not map the pen to output {kde_output:?} via KWin: {e:#}"
                        );
                    }
                }
                Ok(Box::new(s))
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = (opts, &kde_output);
                bail!("uinput is Linux-only; use --dry-run on this system")
            }
        },
    )
}

/// One daemon session per user: the iPad lets the newest connection win, so
/// two `sidecard` processes would keep kicking each other off.
fn single_instance_lock() -> Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let path = dir.join("sidecard.lock");
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    // SAFETY: flock on a valid fd we own; released when the file is dropped.
    let r = unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if r != 0 {
        let pid = std::fs::read_to_string(&path).unwrap_or_default();
        bail!(
            "another sidecard is already talking to the iPad (pid {}). Stop it first, e.g. `systemctl --user stop sidecard`",
            pid.trim()
        );
    }
    use std::io::Write;
    let mut w = &f;
    let _ = f.set_len(0);
    let _ = write!(w, "{}", std::process::id());
    Ok(f)
}

fn session_ctx(env: &Env, pairing: PairingMode, dry_run: bool) -> Result<SessionCtx> {
    Ok(SessionCtx {
        identity: env.identity()?,
        trust: env.trust()?,
        pairing,
        name: hostname(),
        mapper_cfg: env.cfg.mapper(),
        gesture_cfg: env.cfg.gestures()?,
        device_opts: DeviceOptions {
            direct: env.cfg.tablet.direct,
            keyboard_keys: env.cfg.keyboard_keys(),
        },
        haptic_on_tool_switch: env.cfg.pencil.haptic_on_tool_switch,
        sink_factory: sink_factory(dry_run, env.cfg.kde.output.clone()),
        max_duration: None,
        max_samples: None,
        recorder: None,
        on_active: None,
        clock_offset_tx: None,
        stats: SessionStats::default(),
    })
}

#[derive(Clone)]
struct VideoPlan {
    mode: String,
    encoder: videochan::Encoder,
    kbps: u32,
    fps: u32,
}

impl VideoPlan {
    fn from(env: &Env, a: &ConnArgs) -> VideoPlan {
        VideoPlan {
            mode: a
                .video
                .clone()
                .unwrap_or_else(|| env.cfg.video.mode.clone()),
            encoder: if env.cfg.video.encoder == "x264" {
                videochan::Encoder::X264
            } else {
                videochan::Encoder::Vaapi
            },
            kbps: env.cfg.video.bitrate_kbps,
            fps: env.cfg.video.fps.max(1),
        }
    }
}

/// Opens the video channel once the pen session is up.
#[allow(clippy::too_many_arguments)]
async fn run_video_plan(
    t: Transport,
    port: u16,
    identity: Identity,
    info: session::ActiveInfo,
    session_id: u64,
    name: String,
    plan: VideoPlan,
    clock_rx: tokio::sync::watch::Receiver<Option<i64>>,
) -> Result<videochan::VideoStats> {
    use videochan::VideoSource;
    let w = (info.surface.width_pt * info.surface.scale)
        .round()
        .max(16.0) as u32;
    let h = (info.surface.height_pt * info.surface.scale)
        .round()
        .max(16.0) as u32;
    // Keeps the virtual output alive (and its Wayland socket drained).
    let mut keeper: Option<std::sync::Arc<std::sync::atomic::AtomicBool>> = None;
    let source = match plan.mode.as_str() {
        "test-pattern" => videochan::test_pattern(w, h, plan.fps),
        m if m.starts_with("file:") => VideoSource::File {
            path: PathBuf::from(&m[5..]),
            fps: plan.fps,
            width: w,
            height: h,
            repeat: true,
        },
        #[cfg(feature = "display")]
        "virtual" => {
            use sidecard::display::{VirtualOutput, VirtualOutputSpec};
            let spec = VirtualOutputSpec {
                width: w as i32,
                height: h as i32,
                scale: info.surface.scale as f64,
                ..VirtualOutputSpec::ipad_air_11()
            };
            let out_name = spec.output_name();
            let mut vo = tokio::task::spawn_blocking(move || {
                VirtualOutput::create(spec, Duration::from_secs(5))
            })
            .await??;
            let node = vo.node_id;
            tracing::info!("virtual monitor {out_name} {w}x{h}, PipeWire node {node}");
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop2 = stop.clone();
            std::thread::spawn(move || {
                while !stop2.load(std::sync::atomic::Ordering::Relaxed) {
                    if !matches!(vo.poll(), Ok(true)) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
                drop(vo);
            });
            keeper = Some(stop);
            #[cfg(target_os = "linux")]
            {
                let nodes = info.dev_nodes.clone();
                let out = out_name.clone();
                if let Err(e) =
                    tokio::task::spawn_blocking(move || kde::map_to_output(&nodes, &out)).await?
                {
                    tracing::warn!("could not map the pen to {out_name}: {e:#}");
                }
            }
            videochan::pipewire(node, w, h, plan.fps, plan.encoder, plan.kbps)
        }
        m => bail!("unsupported video mode {m:?}"),
    };
    let res = async {
        let conn = transport::connect(&t, port, &[]).await?;
        videochan::run_video(
            conn,
            &identity,
            info.peer_fp,
            session_id,
            &name,
            source,
            Some(clock_rx),
        )
        .await
    }
    .await;
    if let Some(stop) = keeper {
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    res
}

async fn one_session(
    env: &Env,
    t: &Transport,
    ctx: &mut SessionCtx,
    video: &VideoPlan,
) -> Result<SessionEnd> {
    let prefer: Vec<String> = ctx
        .trust
        .devices
        .iter()
        .map(|d| d.fingerprint.clone())
        .collect();
    let c = transport::connect(t, env.cfg.connection.port, &prefer).await?;
    tracing::info!("connected via {}", c.description);
    let (tls, fp) = transport::tls_connect(c, &ctx.identity).await?;
    if ctx.pairing == PairingMode::Refuse && !ctx.trust.is_trusted(&fp) {
        tracing::warn!(
            "iPad certificate {} is not paired with this PC; run `sidecard pair`",
            pairing::format_fingerprint(&fp)
        );
    }
    let session_id: u64 = rand::random();
    if video.mode == "off" {
        return session::run_session(tls, fp, session_id, ctx).await;
    }
    let (tx, mut rx) = tokio::sync::watch::channel(None);
    ctx.on_active = Some(tx);
    let (clock_tx, clock_rx) = tokio::sync::watch::channel(None);
    ctx.clock_offset_tx = Some(clock_tx);
    let (t2, port, identity, name, plan) = (
        t.clone(),
        env.cfg.connection.port,
        ctx.identity.clone(),
        ctx.name.clone(),
        video.clone(),
    );
    let video_fut = async move {
        rx.wait_for(|v| v.is_some())
            .await
            .map_err(|_| anyhow::anyhow!("pen session ended"))?;
        let info = rx.borrow().clone().expect("checked");
        run_video_plan(t2, port, identity, info, session_id, name, plan, clock_rx).await
    };
    let end = {
        let input = session::run_session(tls, fp, session_id, ctx);
        tokio::pin!(input);
        tokio::pin!(video_fut);
        let mut video_done = false;
        loop {
            tokio::select! {
                r = &mut input => break r,
                v = &mut video_fut, if !video_done => {
                    video_done = true;
                    match v {
                        Ok(st) => tracing::info!(
                            "video channel ended after {} frames; encoder -> iPad p50 {:.1} ms p95 {:.1} ms ({} samples)",
                            st.frames_sent, st.transport_p50_us as f64 / 1000.0, st.transport_p95_us as f64 / 1000.0,
                            st.latency_samples
                        ),
                        Err(e) => tracing::warn!("video channel: {e:#}"),
                    }
                }
            }
        }
    };
    ctx.on_active = None;
    ctx.clock_offset_tx = None;
    end
}

async fn run_loop(
    env: Env,
    conn: ConnArgs,
    pairing: PairingMode,
    record: Option<PathBuf>,
    once: bool,
) -> Result<()> {
    let _lock = single_instance_lock()?;
    let t = env.transport(&conn)?;
    let video = VideoPlan::from(&env, &conn);
    let mut ctx = session_ctx(&env, pairing, conn.dry_run)?;
    if let Some(p) = &record {
        ctx.recorder = Some(Recorder::create(p)?);
    }
    tracing::info!(
        "identity fingerprint {}",
        pairing::format_fingerprint(&ctx.identity.fingerprint())
    );
    let mut backoff = Duration::from_millis(500);
    let mut last_err: Option<String> = None;
    loop {
        let res = tokio::select! {
            r = one_session(&env, &t, &mut ctx, &video) => r,
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("interrupted");
                return Ok(());
            }
        };
        match res {
            Ok(end) => {
                last_err = None;
                tracing::info!("session ended: {end:?}");
                backoff = Duration::from_millis(500);
                if matches!(
                    end,
                    SessionEnd::Goodbye(sidecar_protocol::GoodbyeCode::NotPaired, _)
                ) {
                    tracing::warn!("the iPad does not know this PC; run `sidecard pair`");
                }
            }
            Err(e) => {
                // Waiting for the iPad repeats the same error every few
                // seconds; only log it at warn level when it changes.
                let msg = format!("{e:#}");
                if last_err.as_deref() == Some(msg.as_str()) {
                    tracing::debug!("{msg}");
                } else {
                    tracing::warn!("{msg}");
                    last_err = Some(msg);
                }
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
        }
        if once {
            return Ok(());
        }
        // After a successful pairing, later sessions don't ask again.
        tokio::time::sleep(backoff).await;
    }
}

fn print_latency(stats: &SessionStats) {
    let r = stats.input_latency.report(stats.rtt_ns);
    let t = stats.transport_latency.report(stats.rtt_ns);
    println!("\nPen latency report");
    println!(
        "  iPad:              {} ({})",
        stats.peer_name, stats.peer_model
    );
    println!(
        "  samples injected:  {} in {} batches, {} gestures",
        stats.samples, stats.batches, stats.gestures
    );
    println!(
        "  clock sync:        rtt {:.3} ms (offset error <= rtt/2)",
        stats.rtt_ns.unwrap_or(0) as f64 / 1e6
    );
    let ms = |us: u32| us as f64 / 1000.0;
    println!(
        "  touch -> uinput:   p50 {:.2} ms  p95 {:.2} ms  p99 {:.2} ms  max {:.2} ms",
        ms(r.p50_us),
        ms(r.p95_us),
        ms(r.p99_us),
        ms(r.max_us)
    );
    println!(
        "  network only:      p50 {:.2} ms  p95 {:.2} ms  p99 {:.2} ms  max {:.2} ms",
        ms(t.p50_us),
        ms(t.p95_us),
        ms(t.p99_us),
        ms(t.max_us)
    );
    println!("\n  touch -> uinput histogram:");
    print!("{}", stats.input_latency.histogram());
    println!(
        "\n  'touch -> uinput' runs from UITouch.timestamp (digitizer sample time on\n  the iPad) to the write() into /dev/uinput. It excludes the compositor,\n  the application and the display. See docs/latency.md."
    );
}

async fn latency(env: Env, conn: ConnArgs, duration: u64, json: Option<PathBuf>) -> Result<()> {
    let _lock = single_instance_lock()?;
    let t = env.transport(&conn)?;
    let mut ctx = session_ctx(&env, PairingMode::Refuse, conn.dry_run)?;
    ctx.max_duration = Some(Duration::from_secs(duration));
    println!("Measuring for {duration} s: draw and hover on the iPad now.");
    let video = VideoPlan::from(&env, &conn);
    let end = one_session(&env, &t, &mut ctx, &video).await?;
    tracing::info!("session ended: {end:?}");
    print_latency(&ctx.stats);
    if let Some(p) = json {
        let r = ctx.stats.input_latency.report(ctx.stats.rtt_ns);
        let n = ctx.stats.transport_latency.report(ctx.stats.rtt_ns);
        let v = serde_json::json!({
            "peer": ctx.stats.peer_name, "model": ctx.stats.peer_model,
            "samples": ctx.stats.samples, "rtt_us": r.rtt_us,
            "touch_to_uinput_us": {"p50": r.p50_us, "p95": r.p95_us, "p99": r.p99_us, "max": r.max_us},
            "network_us": {"p50": n.p50_us, "p95": n.p95_us, "p99": n.p99_us, "max": n.max_us},
        });
        std::fs::write(&p, serde_json::to_string_pretty(&v)?)?;
    }
    Ok(())
}

async fn devices() -> Result<()> {
    let path = usbmux::socket_path();
    match usbmux::list_devices(&path).await {
        Ok(list) if list.is_empty() => println!("usbmuxd: no devices"),
        Ok(list) => {
            for d in list {
                println!(
                    "usbmuxd: {} via {} (device id {})",
                    d.udid, d.connection_type, d.device_id
                );
            }
        }
        Err(e) => println!("usbmuxd: {e:#}"),
    }
    match transport::discover(Duration::from_secs(3)).await {
        Ok(found) if found.is_empty() => println!(
            "mDNS: no iPad advertising {}",
            sidecar_protocol::BONJOUR_SERVICE_TYPE
        ),
        Ok(found) => {
            for d in found {
                println!(
                    "mDNS: {} at {:?} fp={}",
                    d.name,
                    d.addrs,
                    d.fp.unwrap_or_default()
                );
            }
        }
        Err(e) => println!("mDNS: {e:#}"),
    }
    Ok(())
}

async fn replay(
    env: Env,
    file: PathBuf,
    speed: f64,
    dry_run: bool,
    repeat: bool,
    settle: f64,
) -> Result<()> {
    let events = recording::read(&file)?;
    let surface = events
        .iter()
        .find_map(|e| match e {
            RecEvent::Surface {
                width_pt,
                height_pt,
                scale,
                points_per_inch,
            } => Some(sidecar_protocol::SurfaceInfo {
                width_pt: *width_pt,
                height_pt: *height_pt,
                scale: *scale,
                points_per_inch: *points_per_inch,
            }),
            _ => None,
        })
        .unwrap_or(sidecar_protocol::SurfaceInfo {
            width_pt: 1180.0,
            height_pt: 820.0,
            scale: 2.0,
            points_per_inch: 132.0,
        });
    let geo = TabletGeometry::from_surface(&surface);
    let opts = DeviceOptions {
        direct: env.cfg.tablet.direct,
        keyboard_keys: env.cfg.keyboard_keys(),
    };
    let mut sink = (sink_factory(dry_run, env.cfg.kde.output.clone()))(geo, &opts)?;
    for n in sink.dev_nodes() {
        println!("virtual device: {}", n.display());
    }
    let gcfg = env.cfg.gestures()?;
    tokio::time::sleep(Duration::from_secs_f64(settle)).await;
    loop {
        let mut mapper = PenMapper::new(geo, env.cfg.mapper());
        let start = tokio::time::Instant::now();
        for (i, e) in events.iter().enumerate() {
            if speed > 0.0 {
                tokio::time::sleep_until(
                    start + Duration::from_secs_f64(e.t_ms() / 1000.0 / speed),
                )
                .await;
            }
            if let Some(s) = e.to_sample((e.t_ms() * 1e6) as u64, i as u32)? {
                sink.emit_all(DeviceKind::Pen, &mapper.sample(&s))?;
            } else if let Some(g) = e.to_gesture((e.t_ms() * 1e6) as u64)? {
                let out = gestures::handle(&gcfg, &mut mapper, &g);
                sink.emit_all(DeviceKind::Pen, &out.pen_frames)?;
                sink.emit_all(DeviceKind::Keys, &out.key_frames)?;
                if let Some(t) = out.tool_changed {
                    println!("tool -> {t:?}");
                }
            }
        }
        sink.emit_all(DeviceKind::Pen, &mapper.release_all())?;
        if !repeat {
            break;
        }
    }
    // Give readers (evtest, libinput) time to drain before the device vanishes.
    tokio::time::sleep(Duration::from_millis(500)).await;
    Ok(())
}

async fn fake(
    listen: String,
    replay: Option<PathBuf>,
    speed: f64,
    repeat: bool,
    state_dir: Option<PathBuf>,
    reject: bool,
) -> Result<()> {
    let dir = state_dir.unwrap_or_else(|| config::config_dir().join("fake-ipad"));
    let id = Identity::load_or_create(&dir, "fake iPad")?;
    let mut trust = TrustStore::load(&dir.join("trusted.toml"))?;
    let events = match replay {
        Some(p) => recording::read(&p)?,
        None => recording::demo(),
    };
    let opts = FakeOpts {
        events,
        speed,
        auto_accept: !reject,
        repeat,
        ..Default::default()
    };
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    println!(
        "fake iPad listening on {listen}, fingerprint {}",
        pairing::format_fingerprint(&id.fingerprint())
    );
    loop {
        let st = fake_ipad::serve_one(&listener, &id, &mut trust, &opts).await;
        match st {
            Ok(st) => println!(
                "fake iPad: session done: paired={} samples={} gestures={} pongs={} haptics={} latency={:?} goodbye={:?} video={:?}",
                st.paired, st.samples_sent, st.gestures_sent, st.pings_answered, st.haptics.len(), st.last_latency, st.goodbye
            , st.video),
            Err(e) => println!("fake iPad: session error: {e:#}"),
        }
        if !repeat {
            return Ok(());
        }
    }
}

fn info(env: &Env) -> Result<()> {
    let id = env.identity()?;
    println!("config dir:   {}", env.dir.display());
    println!(
        "fingerprint:  {}",
        pairing::format_fingerprint(&id.fingerprint())
    );
    let trust = env.trust()?;
    if trust.devices.is_empty() {
        println!("paired iPads: none (run `sidecard pair`)");
    }
    for d in &trust.devices {
        println!("paired iPad:  {} {}", d.name, d.fingerprint);
    }
    Ok(())
}

#[cfg(feature = "display")]
async fn display_probe(
    width: i32,
    height: i32,
    scale: f64,
    name: String,
    seconds: u64,
    existing: Option<String>,
) -> Result<()> {
    use sidecard::display::{CursorMode, VirtualOutput, VirtualOutputSpec};
    let spec = VirtualOutputSpec {
        name,
        width,
        height,
        scale,
        ..VirtualOutputSpec::ipad_air_11()
    };
    let out_name = existing.clone().unwrap_or_else(|| spec.output_name());
    let mut vo = tokio::task::spawn_blocking(move || match existing {
        Some(o) => VirtualOutput::stream_existing(&o, CursorMode::Embedded, Duration::from_secs(5)),
        None => VirtualOutput::create(spec, Duration::from_secs(5)),
    })
    .await??;
    println!(
        "virtual output {out_name}: {}x{} @ scale {} (zkde_screencast v{}), PipeWire node {}",
        vo.spec.width, vo.spec.height, vo.spec.scale, vo.protocol_version, vo.node_id
    );
    println!(
        "check: kscreen-doctor -o | grep -A3 {out_name}   |   pw-cli info {}",
        vo.node_id
    );
    println!("pin the pen to it: [kde] output = \"{out_name}\" in config.toml");
    let deadline =
        (seconds > 0).then(|| tokio::time::Instant::now() + Duration::from_secs(seconds));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if !vo.poll()? {
                    println!("KWin closed the virtual output");
                    break;
                }
                if deadline.is_some_and(|d| tokio::time::Instant::now() >= d) {
                    break;
                }
            }
        }
    }
    drop(vo);
    println!("virtual output closed");
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    rt.block_on(async {
        match &cli.cmd {
            Cmd::Run {
                conn,
                pair,
                record,
                once,
            } => {
                let env = Env::load(&cli)?;
                let mode = if *pair {
                    PairingMode::Terminal
                } else {
                    PairingMode::Refuse
                };
                run_loop(env, conn.clone(), mode, record.clone(), *once).await
            }
            Cmd::Pair { conn } => {
                let env = Env::load(&cli)?;
                run_loop(env, conn.clone(), PairingMode::Terminal, None, false).await
            }
            Cmd::Latency {
                conn,
                duration,
                json,
            } => latency(Env::load(&cli)?, conn.clone(), *duration, json.clone()).await,
            Cmd::Devices => devices().await,
            Cmd::Replay {
                file,
                speed,
                dry_run,
                repeat,
                settle,
            } => {
                replay(
                    Env::load(&cli)?,
                    file.clone(),
                    *speed,
                    *dry_run,
                    *repeat,
                    *settle,
                )
                .await
            }
            Cmd::FakeIpad {
                listen,
                replay,
                speed,
                repeat,
                state_dir,
                reject_pairing,
            } => {
                fake(
                    listen.clone(),
                    replay.clone(),
                    *speed,
                    *repeat,
                    state_dir.clone(),
                    *reject_pairing,
                )
                .await
            }
            Cmd::GenDemo => {
                recording::write_all(&recording::demo(), &mut std::io::stdout().lock())?;
                Ok(())
            }
            Cmd::Info => info(&Env::load(&cli)?),
            Cmd::Unpair { who } => {
                let env = Env::load(&cli)?;
                let n = env.trust()?.remove(who)?;
                if n == 0 {
                    bail!("no paired iPad matches {who:?}");
                }
                println!("removed {n} pairing(s)");
                Ok(())
            }
            Cmd::ExampleConfig => {
                print!("{}", config::EXAMPLE);
                Ok(())
            }
            #[cfg(feature = "display")]
            Cmd::DisplayProbe {
                width,
                height,
                scale,
                name,
                seconds,
                existing,
            } => {
                display_probe(
                    *width,
                    *height,
                    *scale,
                    name.clone(),
                    *seconds,
                    existing.clone(),
                )
                .await
            }
        }
    })
}
