//! Daemon ↔ fake iPad over real TCP + TLS: pairing, replay of the synthetic
//! recording, gestures, haptics, clock sync. Events land in an in-memory sink
//! (the uinput variant runs in CI, see tests/uinput.rs).

use sidecar_protocol::{GoodbyeCode, HapticKind};
use sidecard::events::*;
use sidecard::fake_ipad::{self, FakeOpts, FakeStats};
use sidecard::gestures::GestureConfig;
use sidecard::identity::{Identity, TrustStore};
use sidecard::recording;
use sidecard::session::{self, PairingMode, SessionCtx, SessionEnd, SessionStats};
use sidecard::sink::{DeviceOptions, EventSink, RecordingSink};
use sidecard::tablet::{Frame, MapperConfig};
use sidecard::transport;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<RecordingSink>>);

impl EventSink for Shared {
    fn emit(&mut self, dev: DeviceKind, frame: &[RawEvent]) -> std::io::Result<()> {
        self.0.lock().unwrap().emit(dev, frame)
    }
}

fn ctx(id: Identity, trust: TrustStore, pairing: PairingMode, sink: Shared) -> SessionCtx {
    SessionCtx {
        identity: id,
        trust,
        pairing,
        name: "test-pc".into(),
        mapper_cfg: MapperConfig::default(),
        gesture_cfg: GestureConfig::default(),
        device_opts: DeviceOptions {
            direct: true,
            keyboard_keys: vec![],
        },
        haptic_on_tool_switch: true,
        sink_factory: Box::new(move |_, _| Ok(Box::new(sink.clone()) as Box<dyn EventSink>)),
        max_duration: Some(Duration::from_secs(20)),
        max_samples: None,
        recorder: None,
        stats: SessionStats::default(),
    }
}

struct Run {
    daemon: anyhow::Result<SessionEnd>,
    fake: FakeStats,
    ctx: SessionCtx,
    fake_trust: TrustStore,
}

async fn run_pair(
    daemon_ctx: SessionCtx,
    fake_id: Arc<Identity>,
    mut fake_trust: TrustStore,
    opts: FakeOpts,
) -> Run {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let fake = tokio::spawn(async move {
        let st = fake_ipad::serve_one(&listener, &fake_id, &mut fake_trust, &opts).await;
        (st, fake_trust)
    });
    let mut ctx = daemon_ctx;
    let daemon = async {
        let c = transport::connect_tcp(&addr).await?;
        let (tls, fp) = transport::tls_connect(c, &ctx.identity).await?;
        session::run_session(tls, fp, 42, &mut ctx).await
    }
    .await;
    let (fake_res, fake_trust) = tokio::time::timeout(Duration::from_secs(20), fake)
        .await
        .unwrap()
        .unwrap();
    Run {
        daemon,
        fake: fake_res.unwrap_or_default(),
        ctx,
        fake_trust,
    }
}

fn all_events(sink: &Shared) -> Vec<RawEvent> {
    sink.0
        .lock()
        .unwrap()
        .frames
        .iter()
        .flat_map(|(_, f): &(DeviceKind, Frame)| f.clone())
        .collect()
}

fn opts() -> FakeOpts {
    FakeOpts {
        events: recording::demo(),
        speed: 4.0,
        ..Default::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unpaired_ipad_is_refused_then_pairing_then_reconnect() {
    let daemon_id = Identity::generate("daemon").unwrap();
    let daemon_fp = daemon_id.fingerprint();
    let fake_id = Arc::new(Identity::generate("ipad").unwrap());

    // 1. Normal `run` mode refuses an unknown iPad and injects nothing.
    let sink = Shared::default();
    let r = run_pair(
        ctx(
            daemon_id,
            TrustStore::in_memory(),
            PairingMode::Refuse,
            sink.clone(),
        ),
        fake_id.clone(),
        TrustStore::in_memory(),
        opts(),
    )
    .await;
    let err = r.daemon.unwrap_err().to_string();
    assert!(err.contains("not paired"), "{err}");
    assert_eq!(
        r.fake.goodbye.as_ref().map(|g| g.0),
        Some(GoodbyeCode::NotPaired)
    );
    assert!(all_events(&sink).is_empty());
    assert!(!r.fake_trust.is_trusted(&daemon_fp));

    // 2. `pair` mode: both sides accept, both pin, and the session runs.
    let sink = Shared::default();
    let r = run_pair(
        ctx(
            r.ctx.identity,
            r.ctx.trust,
            PairingMode::AutoAccept,
            sink.clone(),
        ),
        fake_id.clone(),
        r.fake_trust,
        opts(),
    )
    .await;
    let end = r.daemon.unwrap();
    assert_eq!(
        end,
        SessionEnd::Goodbye(GoodbyeCode::Normal, "replay finished".into())
    );
    assert!(r.fake.paired);
    assert!(r.ctx.trust.is_trusted(&fake_id.fingerprint()));
    assert!(r.fake_trust.is_trusted(&daemon_fp));

    let ev = all_events(&sink);
    let has = |ty, code| ev.iter().any(|e| e.ty == ty && e.code == code);
    for (ty, code) in [
        (EV_KEY, BTN_TOOL_PEN),
        (EV_KEY, BTN_TOUCH),
        (EV_KEY, BTN_TOOL_RUBBER), // double-tap toggled the eraser
        (EV_KEY, BTN_STYLUS2),     // squeeze
        (EV_ABS, ABS_X),
        (EV_ABS, ABS_Y),
        (EV_ABS, ABS_PRESSURE),
        (EV_ABS, ABS_TILT_X),
        (EV_ABS, ABS_TILT_Y),
        (EV_ABS, ABS_Z),
        (EV_ABS, ABS_DISTANCE),
    ] {
        assert!(has(ty, code), "missing {}", code_name(ty, code));
    }
    let max_p = ev
        .iter()
        .filter(|e| e.code == ABS_PRESSURE && e.ty == EV_ABS)
        .map(|e| e.value)
        .max()
        .unwrap();
    assert!(max_p > 8000, "pressure peak {max_p}");
    // Never left touching or in proximity.
    let last_touch = ev
        .iter()
        .rev()
        .find(|e| e.ty == EV_KEY && e.code == BTN_TOUCH)
        .unwrap();
    assert_eq!(last_touch.value, 0);

    // Tool switch → one haptic back to the iPad.
    assert_eq!(r.fake.haptics.len(), 1);
    assert_eq!(r.fake.haptics[0].kind, HapticKind::Alignment);
    // Clock sync: the fake iPad clock runs 1000 s ahead.
    let off = r.ctx.stats.clock_offset_ns.expect("clock synced");
    assert!((off - 1_000_000_000_000).abs() < 5_000_000, "offset {off}");
    assert!(r.fake.pings_answered >= 5);
    // Latency over loopback should be small but positive; the fake iPad
    // batches at 120 Hz so up to ~8.3 ms coalescing delay is expected.
    let rep = r.ctx.stats.input_latency.report(r.ctx.stats.rtt_ns);
    assert!(rep.samples > 300, "{rep:?}");
    assert!(rep.p50_us < 15_000, "{rep:?}");
    let samples_sent = r.fake.samples_sent;
    assert_eq!(r.ctx.stats.samples, samples_sent);

    // 3. Reconnect with pinned certificates: no pairing needed even in Refuse mode.
    let sink = Shared::default();
    let r = run_pair(
        ctx(
            r.ctx.identity,
            r.ctx.trust,
            PairingMode::Refuse,
            sink.clone(),
        ),
        fake_id.clone(),
        r.fake_trust,
        opts(),
    )
    .await;
    assert!(r.daemon.is_ok(), "{:?}", r.daemon);
    assert!(!r.fake.paired, "no second pairing");
    assert!(!all_events(&sink).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ipad_rejecting_pairing_pins_nothing() {
    let daemon_id = Identity::generate("daemon").unwrap();
    let fake_id = Arc::new(Identity::generate("ipad").unwrap());
    let sink = Shared::default();
    let r = run_pair(
        ctx(
            daemon_id,
            TrustStore::in_memory(),
            PairingMode::AutoAccept,
            sink.clone(),
        ),
        fake_id.clone(),
        TrustStore::in_memory(),
        FakeOpts {
            auto_accept: false,
            ..opts()
        },
    )
    .await;
    assert!(r.daemon.is_err());
    assert!(r.ctx.trust.devices.is_empty());
    assert!(r.fake_trust.devices.is_empty());
    assert!(all_events(&sink).is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_with_other_identity_is_not_trusted() {
    // iPad paired with daemon A; daemon B (different key) must pair again.
    let a = Identity::generate("daemon A").unwrap();
    let fake_id = Arc::new(Identity::generate("ipad").unwrap());
    let mut fake_trust = TrustStore::in_memory();
    fake_trust.add(&a.fingerprint(), "A").unwrap();
    let mut b_trust = TrustStore::in_memory();
    b_trust.add(&fake_id.fingerprint(), "ipad").unwrap(); // B trusts the iPad...
    let b = Identity::generate("daemon B").unwrap();
    let sink = Shared::default();
    let r = run_pair(
        ctx(b, b_trust, PairingMode::Refuse, sink.clone()),
        fake_id,
        fake_trust,
        opts(),
    )
    .await;
    // ...but the iPad doesn't trust B, so B (refuse mode) must not proceed.
    assert!(r.daemon.is_err());
    assert!(all_events(&sink).is_empty());
}
