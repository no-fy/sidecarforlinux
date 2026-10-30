//! Message definitions. Field order here *is* the wire order; see
//! `docs/protocol.md` for the normative tables.

use crate::wire::{Reader, Writer};
use crate::{ProtocolError, HELLO_MAGIC};

/// Message type bytes.
pub mod ty {
    pub const HELLO: u8 = 0x01;
    pub const HELLO_ACK: u8 = 0x02;
    pub const PAIR_COMMIT: u8 = 0x03;
    pub const PAIR_NONCE: u8 = 0x04;
    pub const PAIR_REVEAL: u8 = 0x05;
    pub const PAIR_DECISION: u8 = 0x06;
    pub const GOODBYE: u8 = 0x07;

    pub const PEN_BATCH: u8 = 0x10;
    pub const PENCIL_GESTURE: u8 = 0x11;
    pub const SURFACE_INFO: u8 = 0x12;

    pub const HAPTIC: u8 = 0x20;
    pub const MAPPING_INFO: u8 = 0x21;

    pub const PING: u8 = 0x30;
    pub const PONG: u8 = 0x31;
    pub const LATENCY_REPORT: u8 = 0x32;

    pub const VIDEO_CONFIG: u8 = 0x40;
    pub const VIDEO_FRAME: u8 = 0x41;
    pub const VIDEO_FEEDBACK: u8 = 0x42;
}

/// Which logical channel a connection carries. Each channel is its own
/// TCP/TLS connection so a large video frame never delays pen samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Channel {
    /// Control + pen input + haptics + clock sync.
    Input = 0,
    /// Video stream (phase 2).
    Video = 1,
}

impl Channel {
    fn from_u8(v: u8) -> Result<Self, ProtocolError> {
        match v {
            0 => Ok(Channel::Input),
            1 => Ok(Channel::Video),
            _ => Err(ProtocolError::BadValue {
                field: "channel",
                value: v as u32,
            }),
        }
    }
}

/// Capability bits exchanged in Hello/HelloAck.
pub mod caps {
    /// Peer can send/receive PencilGesture messages.
    pub const GESTURES: u32 = 1 << 0;
    /// Peer can send/receive Haptic messages.
    pub const HAPTICS: u32 = 1 << 1;
    /// Peer supports the video channel.
    pub const VIDEO: u32 = 1 << 2;
    /// Samples may carry predicted touches (flag `PREDICTED`).
    pub const PREDICTED: u32 = 1 << 3;
    /// iPad: the pencil reports barrel roll (Apple Pencil Pro).
    pub const ROLL: u32 = 1 << 4;
    /// iPad: hover is available.
    pub const HOVER: u32 = 1 << 5;
    /// Video: HEVC decode supported.
    pub const HEVC: u32 = 1 << 6;
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hello {
    pub major: u16,
    pub minor: u16,
    pub channel: Channel,
    /// Does the daemon already trust (has pinned) the iPad certificate it
    /// saw in this TLS handshake?
    pub trusts_peer: bool,
    pub caps: u32,
    /// Random per-daemon-session id; the video channel repeats it so the iPad
    /// can associate both connections.
    pub session_id: u64,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HelloAck {
    pub major: u16,
    pub minor: u16,
    /// Does the iPad already trust the daemon certificate?
    pub trusts_peer: bool,
    pub caps: u32,
    pub surface: SurfaceInfo,
    pub device_name: String,
    pub model: String,
}

/// Size of the iPad drawing surface. Pen coordinates are normalised to it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SurfaceInfo {
    pub width_pt: f32,
    pub height_pt: f32,
    /// UIScreen scale (points → pixels).
    pub scale: f32,
    /// Points per inch of the physical screen (132 on 264 ppi iPads).
    pub points_per_inch: f32,
}

impl SurfaceInfo {
    pub const WIRE_LEN: usize = 16;
    pub fn width_mm(&self) -> f32 {
        self.width_pt / self.points_per_inch * 25.4
    }
    pub fn height_mm(&self) -> f32 {
        self.height_pt / self.points_per_inch * 25.4
    }
}

/// Pen sample phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PenPhase {
    /// In proximity, not touching.
    Hover = 0,
    /// Tip touched down (first contact sample).
    Down = 1,
    /// Touching and moving.
    Move = 2,
    /// Tip lifted (last contact sample). Tool stays in proximity.
    Up = 3,
    /// Touch cancelled by the system: lift without committing.
    Cancel = 4,
    /// Left proximity (hover ended / out of range).
    Out = 5,
}

impl PenPhase {
    pub fn from_u8(v: u8) -> Result<Self, ProtocolError> {
        Ok(match v {
            0 => PenPhase::Hover,
            1 => PenPhase::Down,
            2 => PenPhase::Move,
            3 => PenPhase::Up,
            4 => PenPhase::Cancel,
            5 => PenPhase::Out,
            _ => {
                return Err(ProtocolError::BadValue {
                    field: "phase",
                    value: v as u32,
                })
            }
        })
    }
    pub fn is_contact(self) -> bool {
        matches!(self, PenPhase::Down | PenPhase::Move)
    }
}

/// Bits of `PenSample::flags`.
pub mod sample_flags {
    /// From `predictedTouches(for:)`; may never really happen.
    pub const PREDICTED: u8 = 1 << 0;
    /// From `coalescedTouches(for:)` (not the "main" touch of the event).
    pub const COALESCED: u8 = 1 << 1;
    /// Force is an estimate that UIKit expects to refine.
    pub const PRESSURE_ESTIMATED: u8 = 1 << 2;
    /// Altitude/azimuth are estimates expecting refinement.
    pub const ANGLES_ESTIMATED: u8 = 1 << 3;
    /// `roll` is meaningful (Apple Pencil Pro).
    pub const ROLL_VALID: u8 = 1 << 4;
    /// Sample comes from the hover gesture recognizer.
    pub const FROM_HOVER: u8 = 1 << 5;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenSample {
    /// iPad clock (seconds since boot, as UITouch.timestamp) in nanoseconds.
    pub timestamp_ns: u64,
    pub seq: u32,
    pub phase: PenPhase,
    pub flags: u8,
    /// Normalised [0,1] across the surface width, left → right.
    pub x: f32,
    /// Normalised [0,1] across the surface height, top → bottom.
    pub y: f32,
    /// force / maximumPossibleForce, [0,1]. 0 while hovering.
    pub pressure: f32,
    /// Radians, 0 = parallel to the screen, π/2 = perpendicular.
    pub altitude: f32,
    /// Radians in view coordinates (x right, y down), direction the cap end
    /// of the pencil points to.
    pub azimuth: f32,
    /// Barrel roll in radians (Pencil Pro); see `ROLL_VALID`.
    pub roll: f32,
    /// Hover height normalised [0,1] (UIHoverGestureRecognizer.zOffset);
    /// 0 when touching.
    pub z_offset: f32,
    /// UITouch.estimationUpdateIndex (0 if none).
    pub estimation_index: u32,
}

impl PenSample {
    pub const WIRE_LEN: usize = 48;

    fn write(&self, w: &mut Writer) {
        w.u64(self.timestamp_ns);
        w.u32(self.seq);
        w.u8(self.phase as u8);
        w.u8(self.flags);
        w.u16(0);
        w.f32(self.x);
        w.f32(self.y);
        w.f32(self.pressure);
        w.f32(self.altitude);
        w.f32(self.azimuth);
        w.f32(self.roll);
        w.f32(self.z_offset);
        w.u32(self.estimation_index);
    }

    fn read(r: &mut Reader) -> Result<Self, ProtocolError> {
        let timestamp_ns = r.u64()?;
        let seq = r.u32()?;
        let phase = PenPhase::from_u8(r.u8()?)?;
        let flags = r.u8()?;
        let _reserved = r.u16()?;
        Ok(PenSample {
            timestamp_ns,
            seq,
            phase,
            flags,
            x: r.f32()?,
            y: r.f32()?,
            pressure: r.f32()?,
            altitude: r.f32()?,
            azimuth: r.f32()?,
            roll: r.f32()?,
            z_offset: r.f32()?,
            estimation_index: r.u32()?,
        })
    }

    pub fn has(&self, flag: u8) -> bool {
        self.flags & flag != 0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PenBatch {
    /// iPad clock when the batch was handed to the network stack.
    pub send_time_ns: u64,
    pub samples: Vec<PenSample>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GestureKind {
    DoubleTap = 1,
    Squeeze = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum GesturePhase {
    /// Discrete gesture (double-tap).
    Discrete = 0,
    Began = 1,
    Changed = 2,
    Ended = 3,
    Cancelled = 4,
}

/// Hover pose attached to a gesture (`UIPencilHoverPose`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HoverPose {
    pub x: f32,
    pub y: f32,
    pub z_offset: f32,
    pub altitude: f32,
    pub azimuth: f32,
    pub roll: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PencilGesture {
    pub timestamp_ns: u64,
    pub kind: GestureKind,
    pub phase: GesturePhase,
    /// `UIPencilPreferredAction` raw value the user chose in iPadOS Settings.
    pub preferred_action: u8,
    pub hover_pose: Option<HoverPose>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HapticKind {
    /// UICanvasFeedbackGenerator.alignmentOccurred(at:)
    Alignment = 1,
    /// UICanvasFeedbackGenerator.pathCompleted(at:)
    PathCompleted = 2,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Haptic {
    pub kind: HapticKind,
    /// Normalised location; NaN = "where the pencil currently is".
    pub x: f32,
    pub y: f32,
}

/// Informational: where the daemon maps the surface.
#[derive(Debug, Clone, PartialEq)]
pub struct MappingInfo {
    pub output_name: String,
    /// Mapped area in the output, normalised [0,1] (x, y, w, h).
    pub area: [f32; 4],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ping {
    pub id: u32,
    /// Daemon CLOCK_MONOTONIC ns when sent.
    pub t1_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pong {
    pub id: u32,
    /// Echo of `Ping::t1_ns`.
    pub t1_ns: u64,
    /// iPad clock when the ping was received.
    pub t2_ns: u64,
    /// iPad clock when the pong was sent.
    pub t3_ns: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencyReport {
    /// Window the stats cover.
    pub samples: u32,
    pub p50_us: u32,
    pub p95_us: u32,
    pub p99_us: u32,
    pub max_us: u32,
    /// Clock-sync round-trip time.
    pub rtt_us: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum GoodbyeCode {
    Normal = 0,
    VersionMismatch = 1,
    NotPaired = 2,
    PairingRejected = 3,
    ProtocolError = 4,
    Busy = 5,
    ShuttingDown = 6,
}

impl GoodbyeCode {
    fn from_u16(v: u16) -> Self {
        match v {
            1 => GoodbyeCode::VersionMismatch,
            2 => GoodbyeCode::NotPaired,
            3 => GoodbyeCode::PairingRejected,
            4 => GoodbyeCode::ProtocolError,
            5 => GoodbyeCode::Busy,
            6 => GoodbyeCode::ShuttingDown,
            _ => GoodbyeCode::Normal,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VideoCodec {
    H264 = 1,
    Hevc = 2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoConfig {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    /// Frames per second ×1000 (60000 = 60 fps).
    pub fps_milli: u32,
    /// Parameter sets without start codes: H.264 = [SPS, PPS],
    /// HEVC = [VPS, SPS, PPS].
    pub parameter_sets: Vec<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VideoFrame {
    pub frame_id: u32,
    pub keyframe: bool,
    /// Daemon clock when the frame was captured.
    pub capture_ns: u64,
    /// Daemon clock when encoding finished.
    pub encoded_ns: u64,
    /// AVCC/HVCC sample: NAL units each prefixed by a 4-byte *big-endian*
    /// length, exactly what CMSampleBuffer wants.
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFeedback {
    pub frame_id: u32,
    pub request_keyframe: bool,
    pub received_ns: u64,
    pub decoded_ns: u64,
    pub displayed_ns: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Hello(Hello),
    HelloAck(HelloAck),
    /// iPad → daemon: SHA-256 commitment to the iPad pairing nonce.
    PairCommit([u8; 32]),
    /// daemon → iPad: daemon pairing nonce.
    PairNonce([u8; 32]),
    /// iPad → daemon: the nonce committed to in PairCommit.
    PairReveal([u8; 32]),
    /// Both: user accepted (true) or rejected the pairing code.
    PairDecision(bool),
    Goodbye {
        code: GoodbyeCode,
        reason: String,
    },
    PenBatch(PenBatch),
    PencilGesture(PencilGesture),
    SurfaceInfo(SurfaceInfo),
    Haptic(Haptic),
    MappingInfo(MappingInfo),
    Ping(Ping),
    Pong(Pong),
    LatencyReport(LatencyReport),
    VideoConfig(VideoConfig),
    VideoFrame(VideoFrame),
    VideoFeedback(VideoFeedback),
    /// A type this implementation doesn't know; skipped by receivers.
    Unknown {
        ty: u8,
        payload: Vec<u8>,
    },
}

fn write_surface(w: &mut Writer, s: &SurfaceInfo) {
    w.f32(s.width_pt);
    w.f32(s.height_pt);
    w.f32(s.scale);
    w.f32(s.points_per_inch);
}

fn read_surface(r: &mut Reader) -> Result<SurfaceInfo, ProtocolError> {
    Ok(SurfaceInfo {
        width_pt: r.f32()?,
        height_pt: r.f32()?,
        scale: r.f32()?,
        points_per_inch: r.f32()?,
    })
}

impl Message {
    pub fn type_byte(&self) -> u8 {
        match self {
            Message::Hello(_) => ty::HELLO,
            Message::HelloAck(_) => ty::HELLO_ACK,
            Message::PairCommit(_) => ty::PAIR_COMMIT,
            Message::PairNonce(_) => ty::PAIR_NONCE,
            Message::PairReveal(_) => ty::PAIR_REVEAL,
            Message::PairDecision(_) => ty::PAIR_DECISION,
            Message::Goodbye { .. } => ty::GOODBYE,
            Message::PenBatch(_) => ty::PEN_BATCH,
            Message::PencilGesture(_) => ty::PENCIL_GESTURE,
            Message::SurfaceInfo(_) => ty::SURFACE_INFO,
            Message::Haptic(_) => ty::HAPTIC,
            Message::MappingInfo(_) => ty::MAPPING_INFO,
            Message::Ping(_) => ty::PING,
            Message::Pong(_) => ty::PONG,
            Message::LatencyReport(_) => ty::LATENCY_REPORT,
            Message::VideoConfig(_) => ty::VIDEO_CONFIG,
            Message::VideoFrame(_) => ty::VIDEO_FRAME,
            Message::VideoFeedback(_) => ty::VIDEO_FEEDBACK,
            Message::Unknown { ty, .. } => *ty,
        }
    }

    /// Payload bytes (without frame header).
    pub fn encode_payload(&self, out: &mut Vec<u8>) {
        let mut w = Writer::new(out);
        match self {
            Message::Hello(h) => {
                w.bytes(&HELLO_MAGIC);
                w.u16(h.major);
                w.u16(h.minor);
                w.u8(h.channel as u8);
                w.u8(h.trusts_peer as u8);
                w.u16(0);
                w.u32(h.caps);
                w.u64(h.session_id);
                w.string(&h.name);
            }
            Message::HelloAck(a) => {
                w.u16(a.major);
                w.u16(a.minor);
                w.u8(a.trusts_peer as u8);
                w.u8(0);
                w.u16(0);
                w.u32(a.caps);
                write_surface(&mut w, &a.surface);
                w.string(&a.device_name);
                w.string(&a.model);
            }
            Message::PairCommit(b) | Message::PairNonce(b) | Message::PairReveal(b) => w.bytes(b),
            Message::PairDecision(acc) => w.u8(*acc as u8),
            Message::Goodbye { code, reason } => {
                w.u16(*code as u16);
                w.string(reason);
            }
            Message::PenBatch(b) => {
                w.u64(b.send_time_ns);
                w.u16(b.samples.len() as u16);
                w.u16(PenSample::WIRE_LEN as u16);
                for s in &b.samples {
                    s.write(&mut w);
                }
            }
            Message::PencilGesture(g) => {
                w.u64(g.timestamp_ns);
                w.u8(g.kind as u8);
                w.u8(g.phase as u8);
                w.u8(g.hover_pose.is_some() as u8);
                w.u8(g.preferred_action);
                let p = g.hover_pose.unwrap_or(HoverPose {
                    x: 0.0,
                    y: 0.0,
                    z_offset: 0.0,
                    altitude: 0.0,
                    azimuth: 0.0,
                    roll: 0.0,
                });
                w.f32(p.x);
                w.f32(p.y);
                w.f32(p.z_offset);
                w.f32(p.altitude);
                w.f32(p.azimuth);
                w.f32(p.roll);
            }
            Message::SurfaceInfo(s) => write_surface(&mut w, s),
            Message::Haptic(h) => {
                w.u8(h.kind as u8);
                w.u8(0);
                w.u16(0);
                w.f32(h.x);
                w.f32(h.y);
            }
            Message::MappingInfo(m) => {
                for v in m.area {
                    w.f32(v);
                }
                w.string(&m.output_name);
            }
            Message::Ping(p) => {
                w.u32(p.id);
                w.u64(p.t1_ns);
            }
            Message::Pong(p) => {
                w.u32(p.id);
                w.u64(p.t1_ns);
                w.u64(p.t2_ns);
                w.u64(p.t3_ns);
            }
            Message::LatencyReport(l) => {
                w.u32(l.samples);
                w.u32(l.p50_us);
                w.u32(l.p95_us);
                w.u32(l.p99_us);
                w.u32(l.max_us);
                w.u32(l.rtt_us);
            }
            Message::VideoConfig(v) => {
                w.u8(v.codec as u8);
                w.u8(v.parameter_sets.len() as u8);
                w.u16(0);
                w.u32(v.width);
                w.u32(v.height);
                w.u32(v.fps_milli);
                for ps in &v.parameter_sets {
                    w.blob16(ps);
                }
            }
            Message::VideoFrame(f) => {
                w.u32(f.frame_id);
                w.u8(f.keyframe as u8);
                w.u8(0);
                w.u16(0);
                w.u64(f.capture_ns);
                w.u64(f.encoded_ns);
                w.bytes(&f.data);
            }
            Message::VideoFeedback(f) => {
                w.u32(f.frame_id);
                w.u8(f.request_keyframe as u8);
                w.u8(0);
                w.u16(0);
                w.u64(f.received_ns);
                w.u64(f.decoded_ns);
                w.u64(f.displayed_ns);
            }
            Message::Unknown { payload, .. } => w.bytes(payload),
        }
    }

    /// Full frame (header + payload) appended to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let mut payload = Vec::with_capacity(64);
        self.encode_payload(&mut payload);
        crate::encode_frame(self.type_byte(), &payload, out);
    }

    pub fn to_frame(&self) -> Vec<u8> {
        let mut v = Vec::new();
        self.encode(&mut v);
        v
    }

    pub fn decode(ty: u8, payload: &[u8]) -> Result<Message, ProtocolError> {
        let mut r = Reader::new(ty, payload);
        let msg = match ty {
            ty::HELLO => {
                if r.array::<4>()? != HELLO_MAGIC {
                    return Err(ProtocolError::BadMagic);
                }
                let major = r.u16()?;
                let minor = r.u16()?;
                let channel = Channel::from_u8(r.u8()?)?;
                let trusts_peer = r.u8()? != 0;
                let _ = r.u16()?;
                Message::Hello(Hello {
                    major,
                    minor,
                    channel,
                    trusts_peer,
                    caps: r.u32()?,
                    session_id: r.u64()?,
                    name: r.string()?,
                })
            }
            ty::HELLO_ACK => {
                let major = r.u16()?;
                let minor = r.u16()?;
                let trusts_peer = r.u8()? != 0;
                let _ = r.u8()?;
                let _ = r.u16()?;
                Message::HelloAck(HelloAck {
                    major,
                    minor,
                    trusts_peer,
                    caps: r.u32()?,
                    surface: read_surface(&mut r)?,
                    device_name: r.string()?,
                    model: r.string()?,
                })
            }
            ty::PAIR_COMMIT => Message::PairCommit(r.array()?),
            ty::PAIR_NONCE => Message::PairNonce(r.array()?),
            ty::PAIR_REVEAL => Message::PairReveal(r.array()?),
            ty::PAIR_DECISION => Message::PairDecision(r.u8()? != 0),
            ty::GOODBYE => Message::Goodbye {
                code: GoodbyeCode::from_u16(r.u16()?),
                reason: r.string()?,
            },
            ty::PEN_BATCH => {
                let send_time_ns = r.u64()?;
                let count = r.u16()? as usize;
                let sample_len = r.u16()? as usize;
                if sample_len < PenSample::WIRE_LEN {
                    return Err(ProtocolError::BadValue {
                        field: "sample_len",
                        value: sample_len as u32,
                    });
                }
                let mut samples = Vec::with_capacity(count);
                for _ in 0..count {
                    let raw = r.bytes(sample_len)?;
                    let mut sr = Reader::new(ty, &raw[..PenSample::WIRE_LEN]);
                    samples.push(PenSample::read(&mut sr)?);
                }
                Message::PenBatch(PenBatch {
                    send_time_ns,
                    samples,
                })
            }
            ty::PENCIL_GESTURE => {
                let timestamp_ns = r.u64()?;
                let kind = match r.u8()? {
                    1 => GestureKind::DoubleTap,
                    2 => GestureKind::Squeeze,
                    v => {
                        return Err(ProtocolError::BadValue {
                            field: "gesture kind",
                            value: v as u32,
                        })
                    }
                };
                let phase = match r.u8()? {
                    0 => GesturePhase::Discrete,
                    1 => GesturePhase::Began,
                    2 => GesturePhase::Changed,
                    3 => GesturePhase::Ended,
                    4 => GesturePhase::Cancelled,
                    v => {
                        return Err(ProtocolError::BadValue {
                            field: "gesture phase",
                            value: v as u32,
                        })
                    }
                };
                let has_pose = r.u8()? & 1 != 0;
                let preferred_action = r.u8()?;
                let pose = HoverPose {
                    x: r.f32()?,
                    y: r.f32()?,
                    z_offset: r.f32()?,
                    altitude: r.f32()?,
                    azimuth: r.f32()?,
                    roll: r.f32()?,
                };
                Message::PencilGesture(PencilGesture {
                    timestamp_ns,
                    kind,
                    phase,
                    preferred_action,
                    hover_pose: has_pose.then_some(pose),
                })
            }
            ty::SURFACE_INFO => Message::SurfaceInfo(read_surface(&mut r)?),
            ty::HAPTIC => {
                let kind = match r.u8()? {
                    1 => HapticKind::Alignment,
                    2 => HapticKind::PathCompleted,
                    v => {
                        return Err(ProtocolError::BadValue {
                            field: "haptic kind",
                            value: v as u32,
                        })
                    }
                };
                let _ = r.u8()?;
                let _ = r.u16()?;
                Message::Haptic(Haptic {
                    kind,
                    x: r.f32()?,
                    y: r.f32()?,
                })
            }
            ty::MAPPING_INFO => {
                let area = [r.f32()?, r.f32()?, r.f32()?, r.f32()?];
                Message::MappingInfo(MappingInfo {
                    area,
                    output_name: r.string()?,
                })
            }
            ty::PING => Message::Ping(Ping {
                id: r.u32()?,
                t1_ns: r.u64()?,
            }),
            ty::PONG => Message::Pong(Pong {
                id: r.u32()?,
                t1_ns: r.u64()?,
                t2_ns: r.u64()?,
                t3_ns: r.u64()?,
            }),
            ty::LATENCY_REPORT => Message::LatencyReport(LatencyReport {
                samples: r.u32()?,
                p50_us: r.u32()?,
                p95_us: r.u32()?,
                p99_us: r.u32()?,
                max_us: r.u32()?,
                rtt_us: r.u32()?,
            }),
            ty::VIDEO_CONFIG => {
                let codec = match r.u8()? {
                    1 => VideoCodec::H264,
                    2 => VideoCodec::Hevc,
                    v => {
                        return Err(ProtocolError::BadValue {
                            field: "codec",
                            value: v as u32,
                        })
                    }
                };
                let n = r.u8()? as usize;
                let _ = r.u16()?;
                let width = r.u32()?;
                let height = r.u32()?;
                let fps_milli = r.u32()?;
                let mut parameter_sets = Vec::with_capacity(n);
                for _ in 0..n {
                    parameter_sets.push(r.blob16()?);
                }
                Message::VideoConfig(VideoConfig {
                    codec,
                    width,
                    height,
                    fps_milli,
                    parameter_sets,
                })
            }
            ty::VIDEO_FRAME => {
                let frame_id = r.u32()?;
                let keyframe = r.u8()? & 1 != 0;
                let _ = r.u8()?;
                let _ = r.u16()?;
                let capture_ns = r.u64()?;
                let encoded_ns = r.u64()?;
                Message::VideoFrame(VideoFrame {
                    frame_id,
                    keyframe,
                    capture_ns,
                    encoded_ns,
                    data: r.rest().to_vec(),
                })
            }
            ty::VIDEO_FEEDBACK => {
                let frame_id = r.u32()?;
                let request_keyframe = r.u8()? & 1 != 0;
                let _ = r.u8()?;
                let _ = r.u16()?;
                Message::VideoFeedback(VideoFeedback {
                    frame_id,
                    request_keyframe,
                    received_ns: r.u64()?,
                    decoded_ns: r.u64()?,
                    displayed_ns: r.u64()?,
                })
            }
            _ => Message::Unknown {
                ty,
                payload: payload.to_vec(),
            },
        };
        // Trailing bytes (from a newer minor version) are deliberately ignored.
        let _ = r.remaining();
        Ok(msg)
    }
}
