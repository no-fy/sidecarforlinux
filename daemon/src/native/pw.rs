//! PipeWire capture source: a consumer stream on KWin's virtual-output node
//! (or any `Video/Source` node), feeding a `FrameSink` on the same thread.
//!
//! Negotiation follows PipeWire's DMA-BUF rules (doc/dox/internals/
//! dma-buf.dox): per format one EnumFormat with the sink's modifiers flagged
//! MANDATORY | DONT_FIXATE, then one without a modifier (SHM fallback). The
//! producer (KWin) fixates the modifier. If an import fails, the pair is
//! dropped and the formats are announced again.
//!
//! Each `process` call drains the queue to the newest usable buffer (older
//! ones go straight back), converts it, hands it back, then encodes.

use super::frame::{
    fourcc_name, newest_usable, Converted, Encoded, Frame, FrameSink, ModifierBook, Pixels, Plane,
    DRM_ABGR8888, DRM_ARGB8888, DRM_XBGR8888, DRM_XRGB8888,
};
use super::stats::StageTimes;
use crate::clock::now_ns;
use anyhow::{anyhow, bail, Context, Result};
use pipewire as pw;
use pw::spa;
use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
use spa::param::video::{VideoFlags, VideoFormat, VideoInfoRaw};
use spa::param::ParamType;
use spa::pod::{ChoiceValue, Object, Pod, Property, PropertyFlags, Value};
use spa::utils::{Choice, ChoiceEnum, ChoiceFlags, Fraction, Id, Rectangle, SpaTypes};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Which node to read and what to ask for.
#[derive(Debug, Clone)]
pub struct CaptureSpec {
    /// Node id, or a node name / object serial (`target.object`).
    pub target: String,
    /// Preferred size (KWin's virtual output follows the negotiated size).
    pub width: u32,
    pub height: u32,
    /// Upper bound for the producer's frame rate (KWin's capture tick);
    /// 0 = none: as fast as the compositor ticks (its refresh rate).
    pub max_fps: u32,
}

/// What the capture thread reports to the async side.
#[derive(Debug)]
pub enum Event {
    /// The negotiated format, for the log.
    Format(String),
    Frame(Encoded),
    /// The thread ended (Ok when stopped on purpose).
    Ended(std::result::Result<(), EndError>),
}

/// Why capture stopped. `unsupported`: the configuration can never work
/// here (see `native::Unsupported`), so don't retry.
#[derive(Debug, Clone)]
pub struct EndError {
    pub msg: String,
    pub unsupported: bool,
}

impl EndError {
    pub fn from_error(e: &anyhow::Error) -> Self {
        EndError {
            msg: format!("{e:#}"),
            unsupported: super::is_unsupported(e),
        }
    }

    pub fn transient(msg: String) -> Self {
        EndError {
            msg,
            unsupported: false,
        }
    }
}

enum Cmd {
    Keyframe,
    Stop,
}

/// Handle to the capture thread. Dropping it stops the thread (without
/// waiting: it may be blocked handing over a frame nobody reads any more).
pub struct Capture {
    cmd: pw::channel::Sender<Cmd>,
}

impl Capture {
    /// Start capturing. `make_sink` runs on the capture thread (VA-API
    /// objects are made there) and gets the channel for encoded frames.
    pub fn spawn<F>(spec: CaptureSpec, make_sink: F, out: mpsc::Sender<Event>) -> Result<Capture>
    where
        F: FnOnce(mpsc::Sender<Event>) -> Result<Box<dyn FrameSink>> + Send + 'static,
    {
        let (tx, rx) = pw::channel::channel::<Cmd>();
        std::thread::Builder::new()
            .name("sidecard-video".into())
            .spawn(move || {
                let r = run(spec, make_sink, rx, &out);
                let _ = out.blocking_send(Event::Ended(r.map_err(|e| EndError::from_error(&e))));
            })
            .context("starting the capture thread")?;
        Ok(Capture { cmd: tx })
    }

    /// Ask for an IDR as soon as possible (re-encoding the last picture if
    /// the screen is idle).
    pub fn request_keyframe(&self) {
        let _ = self.cmd.send(Cmd::Keyframe);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Stop);
    }
}

/// SPA names bytes in memory order, DRM names little-endian words.
pub fn spa_to_drm(f: VideoFormat) -> Option<u32> {
    match f {
        VideoFormat::BGRx => Some(DRM_XRGB8888),
        VideoFormat::BGRA => Some(DRM_ARGB8888),
        VideoFormat::RGBx => Some(DRM_XBGR8888),
        VideoFormat::RGBA => Some(DRM_ABGR8888),
        _ => None,
    }
}

pub fn drm_to_spa(f: u32) -> Option<VideoFormat> {
    match f {
        DRM_XRGB8888 => Some(VideoFormat::BGRx),
        DRM_ARGB8888 => Some(VideoFormat::BGRA),
        DRM_XBGR8888 => Some(VideoFormat::RGBx),
        DRM_ABGR8888 => Some(VideoFormat::RGBA),
        _ => None,
    }
}

/// Formats offered for SHM (the sinks convert all of them).
const SHM_FORMATS: [u32; 4] = [DRM_XRGB8888, DRM_ARGB8888, DRM_XBGR8888, DRM_ABGR8888];

fn serialize(obj: Object) -> Result<Vec<u8>> {
    Ok(spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(obj),
    )
    .map_err(|e| anyhow!("serializing a PipeWire param: {e:?}"))?
    .0
    .into_inner())
}

fn prop(key: FormatProperties, value: Value) -> Property {
    Property::new(key.as_raw(), value)
}

/// The most we ask for: `max_fps`, or 1000/s when uncapped (0).
fn max_rate(spec: &CaptureSpec) -> u32 {
    if spec.max_fps == 0 {
        1000
    } else {
        spec.max_fps
    }
}

/// One EnumFormat object; `modifiers` = None for SHM.
fn enum_format(spec: &CaptureSpec, fourcc: u32, modifiers: Option<&[u64]>) -> Option<Object> {
    let format = drm_to_spa(fourcc)?;
    let mut props = vec![
        prop(
            FormatProperties::MediaType,
            Value::Id(Id(MediaType::Video.as_raw())),
        ),
        prop(
            FormatProperties::MediaSubtype,
            Value::Id(Id(MediaSubtype::Raw.as_raw())),
        ),
        prop(
            FormatProperties::VideoFormat,
            Value::Id(Id(format.as_raw())),
        ),
        prop(
            FormatProperties::VideoSize,
            Value::Choice(ChoiceValue::Rectangle(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: Rectangle {
                        width: spec.width,
                        height: spec.height,
                    },
                    min: Rectangle {
                        width: 1,
                        height: 1,
                    },
                    max: Rectangle {
                        width: 16384,
                        height: 16384,
                    },
                },
            ))),
        ),
        // Variable frame rate: frames come when the screen changes. A range
        // so producers with a fixed rate (test sources) match too.
        prop(
            FormatProperties::VideoFramerate,
            Value::Choice(ChoiceValue::Fraction(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: Fraction { num: 0, denom: 1 },
                    min: Fraction { num: 0, denom: 1 },
                    max: Fraction {
                        num: 1000,
                        denom: 1,
                    },
                },
            ))),
        ),
        // KWin's capture tick runs at the negotiated max framerate. With no
        // cap we offer up to 1000/1 with that as our default: PipeWire keeps
        // a default that fits both ranges (spa/pod/filter.h), so KWin's own
        // (the output's refresh rate) wins. Same shape as Sunshine's offer.
        prop(
            FormatProperties::VideoMaxFramerate,
            Value::Choice(ChoiceValue::Fraction(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Range {
                    default: Fraction {
                        num: max_rate(spec),
                        denom: 1,
                    },
                    min: Fraction { num: 0, denom: 1 },
                    max: Fraction {
                        num: max_rate(spec),
                        denom: 1,
                    },
                },
            ))),
        ),
    ];
    if let Some(mods) = modifiers {
        let mods: Vec<i64> = mods.iter().map(|&m| m as i64).collect();
        props.push(Property {
            key: FormatProperties::VideoModifier.as_raw(),
            flags: PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE,
            value: Value::Choice(ChoiceValue::Long(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: *mods.first()?,
                    alternatives: mods,
                },
            ))),
        });
    }
    Some(Object {
        type_: SpaTypes::ObjectParamFormat.as_raw(),
        id: ParamType::EnumFormat.as_raw(),
        properties: props,
    })
}

/// All EnumFormat params: DMA-BUF ones first, then SHM.
fn enum_formats(spec: &CaptureSpec, book: &ModifierBook) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for (fourcc, mods) in book.formats() {
        if let Some(o) = enum_format(spec, *fourcc, Some(mods)) {
            out.push(serialize(o)?);
        }
    }
    for fourcc in SHM_FORMATS {
        if let Some(o) = enum_format(spec, fourcc, None) {
            out.push(serialize(o)?);
        }
    }
    Ok(out)
}

/// Buffers + header meta, once the format is known.
fn buffer_params(dmabuf: bool) -> Result<Vec<Vec<u8>>> {
    let types: i32 = if dmabuf {
        1 << spa::sys::SPA_DATA_DmaBuf
    } else {
        (1 << spa::sys::SPA_DATA_MemFd) | (1 << spa::sys::SPA_DATA_MemPtr)
    };
    let buffers = Object {
        type_: SpaTypes::ObjectParamBuffers.as_raw(),
        id: ParamType::Buffers.as_raw(),
        properties: vec![Property::new(
            spa::sys::SPA_PARAM_BUFFERS_dataType,
            Value::Choice(ChoiceValue::Int(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Flags {
                    default: types,
                    flags: vec![],
                },
            ))),
        )],
    };
    let header = Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: ParamType::Meta.as_raw(),
        properties: vec![
            Property::new(
                spa::sys::SPA_PARAM_META_type,
                Value::Id(Id(spa::sys::SPA_META_Header)),
            ),
            Property::new(
                spa::sys::SPA_PARAM_META_size,
                Value::Int(std::mem::size_of::<spa::sys::spa_meta_header>() as i32),
            ),
        ],
    };
    Ok(vec![serialize(buffers)?, serialize(header)?])
}

fn update(stream: &pw::stream::Stream, params: &[Vec<u8>]) -> Result<()> {
    let mut pods: Vec<&Pod> = params
        .iter()
        .map(|b| Pod::from_bytes(b).ok_or_else(|| anyhow!("bad pod")))
        .collect::<Result<_>>()?;
    stream
        .update_params(&mut pods)
        .context("updating stream params")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Negotiated {
    fourcc: u32,
    width: u32,
    height: u32,
    /// Some = DMA-BUF with this modifier.
    modifier: Option<u64>,
    /// Negotiated max frame rate, rounded (0 = unknown or unlimited).
    max_fps: u32,
}

struct State {
    sink: Box<dyn FrameSink>,
    spec: CaptureSpec,
    book: ModifierBook,
    fmt: Option<Negotiated>,
    /// A keyframe was asked for and not produced yet.
    keyframe: bool,
    /// Daemon time of the last frame from the producer.
    last_frame_ns: u64,
    /// Daemon time of the last frame handed over (incl. repeats).
    last_sent_ns: u64,
    /// A picture was pushed (an idle keyframe can re-encode it).
    have_picture: bool,
    out: mpsc::Sender<Event>,
    error: Option<anyhow::Error>,
    skip_logged: bool,
    streaming: bool,
}

impl State {
    fn fail(&mut self, e: anyhow::Error) {
        if self.error.is_none() {
            self.error = Some(e);
        }
    }

    /// Re-encode the last picture (idle keyframe or keep-alive).
    fn repeat(&mut self, force_idr: bool) -> Result<()> {
        // A sink that can't repeat (hybrid) won't later either: the request
        // is cleared and the keep-alive clock restarts regardless.
        self.sink.repeat(force_idr)?;
        if force_idr {
            self.keyframe = false;
        }
        self.last_sent_ns = now_ns();
        Ok(())
    }
}

/// How long without frames counts as "idle" for a requested keyframe.
const IDLE_NS: u64 = 40_000_000;
/// Re-send the last picture after this long without frames, like
/// `pipewiresrc keepalive-time=1000` in the GStreamer backend: KWin sends
/// nothing while the screen is still, and an iPad that lost its picture
/// (renderer switch, decoder reset) only asks for a keyframe when a frame
/// arrives.
const KEEPALIVE_NS: u64 = 1_000_000_000;

fn header_meta(buf: &pw::buffer::Buffer) -> Option<(i64, bool)> {
    use spa::buffer::meta::{MetaHeader, MetaHeaderFlags};
    buf.find_meta::<MetaHeader>()
        .map(|h| (h.pts(), h.flags().contains(MetaHeaderFlags::CORRUPTED)))
}

fn usable(buf: &mut pw::buffer::Buffer) -> bool {
    use spa::buffer::ChunkFlags;
    if header_meta(buf).is_some_and(|(_, corrupted)| corrupted) {
        return false;
    }
    let datas = buf.datas_mut();
    let Some(d) = datas.first() else {
        return false;
    };
    if d.chunk().flags().contains(ChunkFlags::CORRUPTED) {
        return false; // cursor-only update (KWin metadata cursor mode)
    }
    // For DMA-BUF the chunk size may legitimately be 0 (dma-buf.dox).
    d.type_() == spa::buffer::DataType::DmaBuf || d.chunk().size() > 0
}

fn on_format(st: &mut State, pod: &Pod) -> Result<Option<Vec<Vec<u8>>>> {
    let mut info = VideoInfoRaw::new();
    info.parse(pod)
        .map_err(|e| anyhow!("parsing the PipeWire format: {e:?}"))?;
    let has_modifier = pod
        .as_object()
        .ok()
        .and_then(|o| o.find_prop(Id(FormatProperties::VideoModifier.as_raw())))
        .is_some();
    if has_modifier
        && info
            .flags()
            .contains(VideoFlags::MODIFIER_FIXATION_REQUIRED)
    {
        // The producer still has to pick one modifier; a second Format follows.
        tracing::debug!("PipeWire: waiting for the producer to fixate the modifier");
        return Ok(None);
    }
    let fourcc = spa_to_drm(info.format())
        .ok_or_else(|| anyhow!("unsupported PipeWire video format {:?}", info.format()))?;
    let size = info.size();
    let fmt = Negotiated {
        fourcc,
        width: size.width,
        height: size.height,
        modifier: has_modifier.then(|| info.modifier()),
        max_fps: {
            let mf = info.max_framerate();
            if mf.denom == 0 || mf.num == 0 || mf.num / mf.denom >= 1000 {
                0
            } else {
                (mf.num + mf.denom / 2) / mf.denom
            }
        },
    };
    let mf = info.max_framerate();
    let desc = format!(
        "PipeWire format: {} {}x{}, max {}/{} fps, {}",
        fourcc_name(fourcc),
        fmt.width,
        fmt.height,
        mf.num,
        mf.denom,
        match fmt.modifier {
            Some(m) => format!("DMA-BUF (zero copy), modifier {m:#018x}"),
            None => "shared memory (CPU copy)".to_string(),
        }
    );
    let _ = st.out.try_send(Event::Format(desc));
    st.fmt = Some(fmt);
    st.sink.buffers_changed();
    Ok(Some(buffer_params(fmt.modifier.is_some())?))
}

/// Returns params to re-announce if an import failed.
fn on_process(st: &mut State, stream: &pw::stream::Stream) -> Result<Option<Vec<Vec<u8>>>> {
    // Wait for the encoder first, then take the newest buffer: a busy
    // encoder starts on the newest picture, not one that waited for it.
    st.sink.wait_ready()?;
    let arrived = now_ns();
    let (buf, stale) = newest_usable(std::iter::from_fn(|| stream.dequeue_buffer()), usable);
    let Some(mut buf) = buf else {
        return Ok(None);
    };
    let Some(fmt) = st.fmt else {
        return Ok(None);
    };
    st.last_frame_ns = arrived;
    let tick = header_meta(&buf).and_then(|(pts, _)| StageTimes::plausible_tick(pts, arrived));
    let times = StageTimes {
        tick_ns: tick,
        arrived_ns: arrived,
        stale_skipped: stale,
        ..Default::default()
    };
    // Always take the request; restored below if nothing was encoded.
    let force = std::mem::take(&mut st.keyframe);
    let converted = {
        let datas = buf.datas_mut();
        match fmt.modifier {
            Some(modifier) => {
                let planes: Vec<Plane> = datas
                    .iter()
                    .filter(|d| d.type_() == spa::buffer::DataType::DmaBuf)
                    .map(|d| Plane {
                        fd: d.fd() as _,
                        offset: d.chunk().offset(),
                        pitch: d.chunk().stride().max(0) as u32,
                    })
                    .collect();
                if planes.is_empty() {
                    bail!("PipeWire buffer without DMA-BUF planes");
                }
                let frame = Frame {
                    width: fmt.width,
                    height: fmt.height,
                    max_fps: fmt.max_fps,
                    pixels: Pixels::DmaBuf {
                        fourcc: fmt.fourcc,
                        modifier,
                        planes: &planes,
                    },
                    times,
                };
                st.sink.push(&frame, force)?
            }
            None => {
                let d = &mut datas[0];
                let (offset, size, stride) = {
                    let c = d.chunk();
                    (c.offset() as usize, c.size() as usize, c.stride())
                };
                let stride = if stride > 0 {
                    stride as u32
                } else {
                    fmt.width * 4
                };
                let bytes = d
                    .data()
                    .ok_or_else(|| anyhow!("PipeWire SHM buffer is not mapped"))?;
                let need = stride as usize * fmt.height as usize;
                let end = offset + size.max(need);
                if bytes.len() < offset + need {
                    bail!(
                        "short PipeWire buffer: {} bytes, need {}",
                        bytes.len(),
                        offset + need
                    );
                }
                let frame = Frame {
                    width: fmt.width,
                    height: fmt.height,
                    max_fps: fmt.max_fps,
                    pixels: Pixels::Shm {
                        fourcc: fmt.fourcc,
                        data: &bytes[offset..end.min(bytes.len())],
                        stride,
                    },
                    times,
                };
                st.sink.push(&frame, force)?
            }
        }
    };
    // The sink has read the pixels: KWin can have the buffer back.
    drop(buf);
    if converted != Converted::Done {
        st.keyframe |= force;
    }
    match converted {
        Converted::Done => {
            st.have_picture = true;
            st.last_sent_ns = now_ns();
            Ok(None)
        }
        Converted::Skipped(why) => {
            if !std::mem::replace(&mut st.skip_logged, true) {
                tracing::warn!("video frame skipped: {why}");
            }
            Ok(None)
        }
        Converted::ImportFailed { fourcc, modifier } => {
            st.book.remove(fourcc, modifier);
            tracing::warn!(
                "could not import {} DMA-BUF with modifier {modifier:#x}; renegotiating{}",
                fourcc_name(fourcc),
                if st.book.is_empty() {
                    " (shared memory from now on)"
                } else {
                    ""
                }
            );
            st.fmt = None;
            Ok(Some(enum_formats(&st.spec, &st.book)?))
        }
    }
}

fn run<F>(
    spec: CaptureSpec,
    make_sink: F,
    cmd: pw::channel::Receiver<Cmd>,
    out: &mpsc::Sender<Event>,
) -> Result<()>
where
    F: FnOnce(mpsc::Sender<Event>) -> Result<Box<dyn FrameSink>>,
{
    pw::init();
    let sink = make_sink(out.clone())?;
    let book = ModifierBook::new(sink.dmabuf_formats());
    let mainloop = pw::main_loop::MainLoopRc::new(None).context("PipeWire main loop")?;
    let context = pw::context::ContextRc::new(&mainloop, None).context("PipeWire context")?;
    let core = context
        .connect_rc(None)
        .context("connecting to PipeWire (is it running in this session?)")?;
    // WirePlumber needs media.type to link a stream (found in the VM).
    let mut props = pw::properties::properties! {
        *pw::keys::MEDIA_TYPE => "Video",
        *pw::keys::MEDIA_CATEGORY => "Capture",
        *pw::keys::MEDIA_ROLE => "Screen",
    };
    let node_id: Option<u32> = spec.target.parse().ok();
    if node_id.is_none() {
        props.insert(*pw::keys::TARGET_OBJECT, spec.target.as_str());
    }
    let stream = pw::stream::StreamRc::new(core, "sidecard-video", props)?;
    let st = Rc::new(RefCell::new(State {
        sink,
        spec: spec.clone(),
        book,
        fmt: None,
        keyframe: false,
        last_frame_ns: 0,
        last_sent_ns: 0,
        have_picture: false,
        out: out.clone(),
        error: None,
        skip_logged: false,
        streaming: false,
    }));

    let (s1, s2, s3) = (st.clone(), st.clone(), st.clone());
    let (ml1, ml2, ml3) = (mainloop.clone(), mainloop.clone(), mainloop.clone());
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(move |_, _, old, new| {
            tracing::debug!("PipeWire stream {old:?} -> {new:?}");
            let mut st = s1.borrow_mut();
            match new {
                pw::stream::StreamState::Streaming => st.streaming = true,
                pw::stream::StreamState::Error(e) => {
                    st.fail(anyhow!("PipeWire stream error: {e}"));
                    ml1.quit();
                }
                pw::stream::StreamState::Unconnected if st.streaming => {
                    st.fail(anyhow!("the PipeWire stream went away"));
                    ml1.quit();
                }
                _ => {}
            }
        })
        .param_changed(move |stream, _, id, param| {
            let Some(param) = param else { return };
            if id != ParamType::Format.as_raw() {
                return;
            }
            let r = on_format(&mut s2.borrow_mut(), param);
            match r.and_then(|p| p.map_or(Ok(()), |p| update(stream, &p))) {
                Ok(()) => {}
                Err(e) => {
                    s2.borrow_mut().fail(e);
                    ml2.quit();
                }
            }
        })
        .process(move |stream, _| {
            // Borrow only while working; update_params runs after.
            let r = on_process(&mut s3.borrow_mut(), stream);
            match r.and_then(|p| p.map_or(Ok(()), |p| update(stream, &p))) {
                Ok(()) => {}
                Err(e) => {
                    s3.borrow_mut().fail(e);
                    ml3.quit();
                }
            }
        })
        .register()?;

    let params = enum_formats(&spec, &st.borrow().book)?;
    let mut pods: Vec<&Pod> = params.iter().filter_map(|b| Pod::from_bytes(b)).collect();
    stream.connect(
        spa::utils::Direction::Input,
        node_id,
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
        &mut pods,
    )?;

    // KWin sends nothing while nothing changes: a requested keyframe
    // re-encodes the last picture as an IDR, and after a second without
    // frames the last picture goes out again (keep-alive).
    let idle_keyframe = {
        let st = st.clone();
        let ml = mainloop.clone();
        move || {
            let mut s = st.borrow_mut();
            if !s.have_picture {
                return;
            }
            let now = now_ns();
            let r = if s.keyframe && now.saturating_sub(s.last_frame_ns) >= IDLE_NS {
                s.repeat(true)
            } else if s.last_sent_ns != 0 && now.saturating_sub(s.last_sent_ns) >= KEEPALIVE_NS {
                s.repeat(false)
            } else {
                Ok(())
            };
            if let Err(e) = r {
                s.fail(e);
                ml.quit();
            }
        }
    };
    let idle2 = idle_keyframe.clone();
    let timer = mainloop.loop_().add_timer(move |_| idle2());
    timer
        .update_timer(
            Some(Duration::from_millis(50)),
            Some(Duration::from_millis(50)),
        )
        .into_sync_result()
        .context("PipeWire timer")?;
    let _cmd = cmd.attach(mainloop.loop_(), {
        let st = st.clone();
        let ml = mainloop.clone();
        move |c| match c {
            Cmd::Stop => ml.quit(),
            Cmd::Keyframe => {
                st.borrow_mut().keyframe = true;
                idle_keyframe();
            }
        }
    });

    mainloop.run();
    let _ = stream.disconnect();
    let err = st.borrow_mut().error.take();
    match err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spa::pod::deserialize::PodDeserializer;

    #[test]
    fn spa_and_drm_formats_map_both_ways() {
        for f in SHM_FORMATS {
            assert_eq!(spa_to_drm(drm_to_spa(f).unwrap()), Some(f));
        }
        assert_eq!(spa_to_drm(VideoFormat::BGRx), Some(DRM_XRGB8888));
        assert_eq!(spa_to_drm(VideoFormat::NV12), None);
    }

    fn props_of(bytes: &[u8]) -> Vec<Property> {
        match PodDeserializer::deserialize_any_from(bytes).unwrap().1 {
            Value::Object(o) => o.properties,
            v => panic!("{v:?}"),
        }
    }

    #[test]
    fn dmabuf_formats_come_first_with_unfixated_modifiers() {
        let spec = CaptureSpec {
            target: "42".into(),
            width: 2360,
            height: 1640,
            max_fps: 0,
        };
        let y_tiled = 0x0100_0000_0000_0002u64;
        let book = ModifierBook::new(vec![(DRM_XRGB8888, vec![y_tiled, 0])]);
        let params = enum_formats(&spec, &book).unwrap();
        assert_eq!(params.len(), 1 + SHM_FORMATS.len());
        let first = props_of(&params[0]);
        let m = first
            .iter()
            .find(|p| p.key == FormatProperties::VideoModifier.as_raw())
            .expect("modifier on the DMA-BUF format");
        assert_eq!(
            m.flags,
            PropertyFlags::MANDATORY | PropertyFlags::DONT_FIXATE
        );
        assert_eq!(
            m.value,
            Value::Choice(ChoiceValue::Long(Choice(
                ChoiceFlags::empty(),
                ChoiceEnum::Enum {
                    default: y_tiled as i64,
                    alternatives: vec![y_tiled as i64, 0]
                }
            )))
        );
        let maxfr = first
            .iter()
            .find(|p| p.key == FormatProperties::VideoMaxFramerate.as_raw())
            .unwrap();
        // Uncapped: up to 1000/1, so the compositor's own rate wins.
        assert!(matches!(
            &maxfr.value,
            Value::Choice(ChoiceValue::Fraction(Choice(_, ChoiceEnum::Range { default, min, max })))
                if *default == Fraction { num: 1000, denom: 1 }
                    && *min == Fraction { num: 0, denom: 1 }
                    && *max == Fraction { num: 1000, denom: 1 }
        ));
        // Capped at 30.
        let capped = CaptureSpec {
            max_fps: 30,
            ..spec.clone()
        };
        let p = props_of(&enum_formats(&capped, &book).unwrap()[0]);
        let m = p
            .iter()
            .find(|p| p.key == FormatProperties::VideoMaxFramerate.as_raw())
            .unwrap();
        assert!(matches!(
            &m.value,
            Value::Choice(ChoiceValue::Fraction(Choice(_, ChoiceEnum::Range { default, max, .. })))
                if *default == Fraction { num: 30, denom: 1 } && *max == Fraction { num: 30, denom: 1 }
        ));
        // The SHM fallbacks carry no modifier.
        for p in &params[1..] {
            assert!(props_of(p)
                .iter()
                .all(|p| p.key != FormatProperties::VideoModifier.as_raw()));
        }
        // Buffers: DMA-BUF only, or MemFd|MemPtr.
        let b = props_of(&buffer_params(true).unwrap()[0]);
        assert!(matches!(
            &b[0].value,
            Value::Choice(ChoiceValue::Int(Choice(_, ChoiceEnum::Flags { default, .. })))
                if *default == 1 << spa::sys::SPA_DATA_DmaBuf
        ));
    }
}
