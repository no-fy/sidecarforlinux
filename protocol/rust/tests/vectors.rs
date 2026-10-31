//! Golden wire vectors shared with the Swift implementation
//! (`protocol/swift/Tests`). Regenerate with
//! `UPDATE_VECTORS=1 cargo test -p sidecar-protocol --test vectors`.

use sidecar_protocol::pairing;
use sidecar_protocol::*;
use std::path::PathBuf;

fn vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vectors/vectors.json")
}

fn sample(seq: u32, phase: PenPhase) -> PenSample {
    PenSample {
        timestamp_ns: 1_234_567_890_123 + seq as u64 * 4_166_667,
        seq,
        phase,
        flags: sample_flags::COALESCED | sample_flags::ROLL_VALID,
        x: 0.25,
        y: 0.75,
        pressure: 0.5,
        altitude: std::f32::consts::FRAC_PI_4,
        azimuth: std::f32::consts::FRAC_PI_2,
        roll: -0.5,
        z_offset: 0.0,
        estimation_index: 7,
    }
}

/// The canonical message set. The Swift tests build exactly the same values.
fn messages() -> Vec<(&'static str, Message)> {
    let surface = SurfaceInfo {
        width_pt: 1180.0,
        height_pt: 820.0,
        scale: 2.0,
        points_per_inch: 132.0,
    };
    vec![
        (
            "hello",
            Message::Hello(Hello {
                major: 1,
                minor: 0,
                channel: Channel::Input,
                trusts_peer: true,
                caps: caps::GESTURES | caps::HAPTICS,
                session_id: 0x0102_0304_0506_0708,
                name: "archbox".into(),
            }),
        ),
        (
            "hello_ack",
            Message::HelloAck(HelloAck {
                major: 1,
                minor: 0,
                trusts_peer: false,
                caps: caps::GESTURES | caps::HAPTICS | caps::ROLL | caps::HOVER,
                surface,
                device_name: "iPad".into(),
                model: "iPad Air".into(),
            }),
        ),
        ("pair_commit", Message::PairCommit([0xAA; 32])),
        ("pair_nonce", Message::PairNonce([0xBB; 32])),
        ("pair_reveal", Message::PairReveal([0xCC; 32])),
        ("pair_decision", Message::PairDecision(true)),
        (
            "goodbye",
            Message::Goodbye {
                code: GoodbyeCode::NotPaired,
                reason: "pair first".into(),
            },
        ),
        (
            "pen_batch",
            Message::PenBatch(PenBatch {
                send_time_ns: 1_234_567_999_999,
                samples: vec![sample(1, PenPhase::Down), sample(2, PenPhase::Move)],
            }),
        ),
        (
            "touch_batch",
            Message::TouchBatch(TouchBatch {
                send_time_ns: 1_234_568_000_000,
                touches: vec![
                    TouchSample {
                        timestamp_ns: 1_234_567_890_123,
                        id: 1,
                        phase: TouchPhase::Down,
                        flags: 0,
                        x: 0.25,
                        y: 0.5,
                        major_radius_pt: 20.5,
                    },
                    TouchSample {
                        timestamp_ns: 1_234_567_890_123,
                        id: 2,
                        phase: TouchPhase::Move,
                        flags: sample_flags::COALESCED,
                        x: 0.75,
                        y: 0.125,
                        major_radius_pt: 62.25,
                    },
                ],
            }),
        ),
        (
            "pencil_gesture_squeeze",
            Message::PencilGesture(PencilGesture {
                timestamp_ns: 99,
                kind: GestureKind::Squeeze,
                phase: GesturePhase::Ended,
                preferred_action: 5,
                hover_pose: Some(HoverPose {
                    x: 0.5,
                    y: 0.5,
                    z_offset: 0.25,
                    altitude: 1.0,
                    azimuth: 2.0,
                    roll: 3.0,
                }),
            }),
        ),
        (
            "pencil_gesture_tap",
            Message::PencilGesture(PencilGesture {
                timestamp_ns: 100,
                kind: GestureKind::DoubleTap,
                phase: GesturePhase::Discrete,
                preferred_action: 1,
                hover_pose: None,
            }),
        ),
        ("surface_info", Message::SurfaceInfo(surface)),
        (
            "haptic",
            Message::Haptic(Haptic {
                kind: HapticKind::Alignment,
                x: 0.5,
                y: 0.25,
            }),
        ),
        (
            "mapping_info",
            Message::MappingInfo(MappingInfo {
                output_name: "DP-1".into(),
                area: [0.0, 0.0, 1.0, 1.0],
            }),
        ),
        ("ping", Message::Ping(Ping { id: 3, t1_ns: 555 })),
        (
            "pong",
            Message::Pong(Pong {
                id: 3,
                t1_ns: 555,
                t2_ns: 1000,
                t3_ns: 1010,
            }),
        ),
        (
            "latency_report",
            Message::LatencyReport(LatencyReport {
                samples: 100,
                p50_us: 4000,
                p95_us: 6000,
                p99_us: 9000,
                max_us: 12000,
                rtt_us: 800,
            }),
        ),
        (
            "video_config",
            Message::VideoConfig(VideoConfig {
                codec: VideoCodec::Hevc,
                width: 2360,
                height: 1640,
                fps_milli: 60000,
                parameter_sets: vec![vec![0x40, 0x01], vec![0x42, 0x01, 0x01], vec![0x44, 0x01]],
            }),
        ),
        (
            "video_frame",
            Message::VideoFrame(VideoFrame {
                frame_id: 9,
                keyframe: true,
                capture_ns: 1,
                encoded_ns: 2,
                data: vec![0, 0, 0, 2, 0x26, 0x01],
            }),
        ),
        (
            "video_feedback",
            Message::VideoFeedback(VideoFeedback {
                frame_id: 9,
                request_keyframe: false,
                received_ns: 10,
                decoded_ns: 20,
                displayed_ns: 30,
            }),
        ),
    ]
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn build_json() -> serde_json::Value {
    let msgs: Vec<_> = messages()
        .into_iter()
        .map(|(name, m)| serde_json::json!({ "name": name, "frame_hex": hex(&m.to_frame()) }))
        .collect();
    let fp_d = pairing::fingerprint(b"daemon certificate DER");
    let fp_i = pairing::fingerprint(b"ipad certificate DER");
    let n_d = [0x11u8; 32];
    let n_i = [0x22u8; 32];
    serde_json::json!({
        "comment": "Generated by protocol/rust/tests/vectors.rs; do not edit by hand.",
        "protocol": { "major": PROTOCOL_MAJOR, "minor": PROTOCOL_MINOR },
        "messages": msgs,
        "pairing": {
            "daemon_cert_utf8": "daemon certificate DER",
            "ipad_cert_utf8": "ipad certificate DER",
            "fp_daemon": hex(&fp_d),
            "fp_ipad": hex(&fp_i),
            "nonce_daemon": hex(&n_d),
            "nonce_ipad": hex(&n_i),
            "commitment": hex(&pairing::commitment(&fp_i, &n_i)),
            "sas": pairing::sas(&fp_d, &fp_i, &n_d, &n_i),
            "sas_formatted": pairing::format_sas(pairing::sas(&fp_d, &fp_i, &n_d, &n_i)),
        }
    })
}

#[test]
fn golden_vectors_match() {
    let json = build_json();
    let text = serde_json::to_string_pretty(&json).unwrap() + "\n";
    let path = vectors_path();
    if std::env::var_os("UPDATE_VECTORS").is_some() {
        std::fs::write(&path, &text).unwrap();
    }
    let on_disk =
        std::fs::read_to_string(&path).expect("vectors.json missing; run with UPDATE_VECTORS=1");
    assert_eq!(
        on_disk, text,
        "wire format changed; regenerate vectors deliberately"
    );
}

#[test]
fn every_message_roundtrips() {
    for (name, m) in messages() {
        let frame = m.to_frame();
        let mut d = FrameDecoder::new();
        d.push(&frame);
        let (ty, payload) = d.next_frame().unwrap().unwrap();
        let back = Message::decode(ty, &payload).unwrap();
        assert_eq!(back, m, "{name}");
    }
}

#[test]
fn trailing_bytes_from_newer_minor_are_ignored() {
    for (name, m) in messages() {
        if matches!(m, Message::VideoFrame(_)) {
            continue; // data runs to the end of the frame by definition
        }
        let mut payload = Vec::new();
        m.encode_payload(&mut payload);
        payload.extend_from_slice(&[0xEE; 9]);
        let back = Message::decode(m.type_byte(), &payload).unwrap();
        if let Message::PenBatch(_) = m {
            // count/sample_len still describe the samples; trailing ignored.
        }
        assert_eq!(back, m, "{name}");
    }
}

#[test]
fn larger_samples_from_newer_minor_are_skipped_correctly() {
    // A v1.1 sender may grow PenSample to 56 bytes.
    let s = sample(1, PenPhase::Move);
    let mut one = Vec::new();
    Message::PenBatch(PenBatch {
        send_time_ns: 5,
        samples: vec![s],
    })
    .encode_payload(&mut one);
    let sample_bytes = &one[12..12 + PenSample::WIRE_LEN];
    let mut payload = Vec::new();
    payload.extend_from_slice(&5u64.to_le_bytes());
    payload.extend_from_slice(&2u16.to_le_bytes());
    payload.extend_from_slice(&56u16.to_le_bytes());
    for _ in 0..2 {
        payload.extend_from_slice(sample_bytes);
        payload.extend_from_slice(&[0x99; 8]);
    }
    match Message::decode(ty::PEN_BATCH, &payload).unwrap() {
        Message::PenBatch(b) => {
            assert_eq!(b.samples, vec![s, s]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn truncated_payloads_are_errors_not_panics() {
    for (name, m) in messages() {
        let mut payload = Vec::new();
        m.encode_payload(&mut payload);
        for cut in 0..payload.len() {
            let r = Message::decode(m.type_byte(), &payload[..cut]);
            // VideoFrame data and pair decision lengths may legally shrink
            // to their fixed header; everything else must error.
            if let Ok(back) = &r {
                assert!(
                    matches!(back, Message::VideoFrame(_)),
                    "{name}: cut at {cut} decoded as {back:?}"
                );
            }
        }
    }
}

#[test]
fn unknown_types_are_preserved() {
    let m = Message::decode(0x7E, b"abc").unwrap();
    assert_eq!(
        m,
        Message::Unknown {
            ty: 0x7E,
            payload: b"abc".to_vec()
        }
    );
}
