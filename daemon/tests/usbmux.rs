//! The USB path without an iPad: a fake usbmuxd (unix socket, real plist
//! protocol) that proxies `Connect` to the fake iPad over TCP, then the
//! normal TLS session with pairing on top.

use sidecar_protocol::{GoodbyeCode, DEFAULT_PORT};
use sidecard::events::DeviceKind;
use sidecard::fake_ipad::{self, FakeOpts};
use sidecard::gestures::GestureConfig;
use sidecard::identity::{Identity, TrustStore};
use sidecard::recording;
use sidecard::session::{self, PairingMode, SessionCtx, SessionEnd, SessionStats};
use sidecard::sink::{DeviceOptions, EventSink, RecordingSink};
use sidecard::tablet::MapperConfig;
use sidecard::{transport, usbmux};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream, UnixListener};

const DEVICE_ID: u64 = 7;
const UDID: &str = "00008112-000A1B2C3D4E5F60";

/// Minimal usbmuxd: answers ListDevices with one USB iPad and turns Connect
/// into a TCP tunnel to `target`. Records the requested port.
async fn fake_usbmuxd(listener: UnixListener, target: String, seen_ports: Arc<Mutex<Vec<u64>>>) {
    loop {
        let Ok((mut s, _)) = listener.accept().await else {
            return;
        };
        let target = target.clone();
        let seen = seen_ports.clone();
        tokio::spawn(async move {
            let (tag, req) = usbmux::recv_plist(&mut s).await.unwrap();
            let ty = req
                .get("MessageType")
                .and_then(|v| v.as_string())
                .unwrap()
                .to_string();
            let mut reply = plist::Dictionary::new();
            match ty.as_str() {
                "ListDevices" => {
                    let mut props = plist::Dictionary::new();
                    props.insert("ConnectionType".into(), "USB".into());
                    props.insert("DeviceID".into(), plist::Value::Integer(DEVICE_ID.into()));
                    props.insert("SerialNumber".into(), UDID.into());
                    props.insert("ProductID".into(), plist::Value::Integer(0x12ab.into()));
                    let mut dev = plist::Dictionary::new();
                    dev.insert("DeviceID".into(), plist::Value::Integer(DEVICE_ID.into()));
                    dev.insert("MessageType".into(), "Attached".into());
                    dev.insert("Properties".into(), plist::Value::Dictionary(props));
                    reply.insert(
                        "DeviceList".into(),
                        plist::Value::Array(vec![plist::Value::Dictionary(dev)]),
                    );
                    usbmux::send_plist(&mut s, tag, &reply).await.unwrap();
                }
                "Connect" => {
                    let dev = req
                        .get("DeviceID")
                        .and_then(|v| v.as_unsigned_integer())
                        .unwrap();
                    let port = req
                        .get("PortNumber")
                        .and_then(|v| v.as_unsigned_integer())
                        .unwrap();
                    seen.lock().unwrap().push(port);
                    let ok = dev == DEVICE_ID && port == usbmux::port_to_usbmux(DEFAULT_PORT);
                    reply.insert("MessageType".into(), "Result".into());
                    reply.insert(
                        "Number".into(),
                        plist::Value::Integer((if ok { 0u64 } else { 3 }).into()),
                    );
                    usbmux::send_plist(&mut s, tag, &reply).await.unwrap();
                    if ok {
                        let mut tcp = TcpStream::connect(&target).await.unwrap();
                        let _ = tokio::io::copy_bidirectional(&mut s, &mut tcp).await;
                    }
                }
                other => panic!("unexpected usbmuxd request {other}"),
            }
        });
    }
}

#[derive(Clone, Default)]
struct Shared(Arc<Mutex<RecordingSink>>);
impl EventSink for Shared {
    fn emit(
        &mut self,
        dev: DeviceKind,
        frame: &[sidecard::events::RawEvent],
    ) -> std::io::Result<()> {
        self.0.lock().unwrap().emit(dev, frame)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn usb_path_via_fake_usbmuxd_pairs_and_streams() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("usbmuxd");
    let seen = Arc::new(Mutex::new(Vec::new()));

    // Fake iPad on TCP, fake usbmuxd tunnelling to it.
    let ipad_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ipad_addr = ipad_listener.local_addr().unwrap().to_string();
    tokio::spawn(fake_usbmuxd(
        UnixListener::bind(&sock).unwrap(),
        ipad_addr,
        seen.clone(),
    ));
    let ipad_id = Arc::new(Identity::generate("ipad").unwrap());
    let ipad_id2 = ipad_id.clone();
    let fake = tokio::spawn(async move {
        let mut trust = TrustStore::in_memory();
        let opts = FakeOpts {
            events: recording::demo(),
            speed: 8.0,
            ..Default::default()
        };
        fake_ipad::serve_one(&ipad_listener, &ipad_id2, &mut trust, &opts).await
    });

    // Device listing sees the iPad.
    let devs = usbmux::list_devices(&sock).await.unwrap();
    assert_eq!(devs.len(), 1);
    assert_eq!(devs[0].udid, UDID);
    assert_eq!(usbmux::choose(&devs, None).unwrap().device_id, DEVICE_ID);

    // Wrong port is refused by "the device".
    let err = usbmux::connect(&sock, DEVICE_ID, 1234)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("connection refused"), "{err}");

    // The real path.
    let sink = Shared::default();
    let s2 = sink.clone();
    let mut ctx = SessionCtx {
        identity: Identity::generate("daemon").unwrap(),
        trust: TrustStore::in_memory(),
        pairing: PairingMode::AutoAccept,
        name: "pc".into(),
        mapper_cfg: MapperConfig::default(),
        gesture_cfg: GestureConfig::default(),
        device_opts: DeviceOptions {
            direct: true,
            keyboard_keys: vec![],
        },
        haptic_on_tool_switch: true,
        sink_factory: Box::new(move |_, _| Ok(Box::new(s2.clone()) as Box<dyn EventSink>)),
        max_duration: Some(Duration::from_secs(15)),
        max_samples: None,
        recorder: None,
        stats: SessionStats::default(),
    };
    let c = transport::connect_usb_at(&sock, Some(UDID), DEFAULT_PORT)
        .await
        .unwrap();
    assert!(c.via_usb);
    assert!(c.description.contains(UDID));
    let (tls, fp) = transport::tls_connect(c, &ctx.identity).await.unwrap();
    assert_eq!(fp, ipad_id.fingerprint());
    let end = session::run_session(tls, fp, 1, &mut ctx).await.unwrap();
    assert_eq!(
        end,
        SessionEnd::Goodbye(GoodbyeCode::Normal, "replay finished".into())
    );
    let st = fake.await.unwrap().unwrap();
    assert!(st.paired);
    assert!(ctx.stats.samples > 300);
    assert!(!sink.0.lock().unwrap().frames.is_empty());
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        &[
            usbmux::port_to_usbmux(1234),
            usbmux::port_to_usbmux(DEFAULT_PORT)
        ],
        "PortNumber must be sent in network byte order"
    );
}
