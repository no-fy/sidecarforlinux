//! VA-API colour conversion + H.264 encoder (`[video] backend = "native"`).
//!
//! Per frame: import the compositor's DMA-BUF as a VA surface (cached per
//! buffer), video-process it into our own NV12 surface (BT.709, limited
//! range), then encode that surface. The source gets its buffer back
//! between the two (`FrameSink::convert` / `encode`). Shared-memory frames
//! are uploaded with one CPU copy instead.
//!
//! Configuration (docs/research/native-video.md §3): CBR, no B-frames, one
//! reference, POC type 2, IDR at start and on request, our own packed SPS/
//! PPS/slice headers when the driver takes them (as GStreamer vah264enc and
//! FFmpeg h264_vaapi do).
//!
//! **Built but untested on real hardware** (no GPU in the VM or on CI).

use super::frame::{
    fourcc_name, Converted, Encoded, Frame, FrameSink, Pixels, Plane, DRM_ABGR8888, DRM_ARGB8888,
    DRM_FORMAT_MOD_LINEAR, DRM_XBGR8888, DRM_XRGB8888,
};
use super::h264::{level_for, Gop, PicPlan, Profile, StreamParams};
use super::pw::{EndError, Event};
use super::stats::StageTimes;
use super::va::{Buffer, Config, Context, Display, Dpy, Surface};
use super::va_sys as sys;
use super::Unsupported;
use crate::clock::now_ns;
use anyhow::{anyhow, bail, Context as _, Result};
use std::path::PathBuf;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct VaOptions {
    /// DRM render node; None = the first that can encode H.264.
    pub device: Option<PathBuf>,
    /// VAEntrypointEncSliceLP (Intel's fixed-function encoder).
    pub low_power: bool,
    pub kbps: u32,
    pub fps: u32,
    /// VA quality level, 1 = best quality ... 7 = fastest.
    pub target_usage: u8,
    /// Constant QP (1-51); 0 = CBR at `kbps` when the encoder has it, else
    /// constant QP `DEFAULT_QP`.
    pub qp: u8,
}

/// Rate control the encoder runs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RateControl {
    Cbr,
    /// Constant QP: no bitrate target. What Gen9's low-power encoder offers
    /// without the HuC firmware.
    Cqp(u8),
}

/// Constant QP when none is configured: sharp text on a 2360x1640 desktop,
/// a few Mbit/s while drawing; bursts while whole windows move are fine
/// over USB (assumption, to be tuned with real numbers).
pub const DEFAULT_QP: u8 = 24;

/// `VAConfigAttribRateControl` bits as words, for the log.
pub fn rc_names(mask: u32) -> String {
    let names = [
        (sys::VA_RC_CBR, "CBR"),
        (sys::VA_RC_VBR, "VBR"),
        (sys::VA_RC_VCM, "VCM"),
        (sys::VA_RC_CQP, "CQP"),
        (sys::VA_RC_VBR_CONSTRAINED, "VBR_CONSTRAINED"),
        (sys::VA_RC_ICQ, "ICQ"),
        (sys::VA_RC_MB, "MB"),
        (sys::VA_RC_CFS, "CFS"),
        (sys::VA_RC_PARALLEL, "PARALLEL"),
        (sys::VA_RC_QVBR, "QVBR"),
        (sys::VA_RC_AVBR, "AVBR"),
        (sys::VA_RC_TCBRC, "TCBRC"),
    ];
    let v: Vec<&str> = names
        .iter()
        .filter(|(b, _)| mask & b != 0)
        .map(|(_, n)| *n)
        .collect();
    if v.is_empty() {
        "none".into()
    } else {
        v.join(" ")
    }
}

/// CBR if offered (and no QP forced), else constant QP if offered.
pub fn choose_rate_control(modes: u32, qp: u8) -> Option<RateControl> {
    if qp > 0 {
        return (modes & sys::VA_RC_CQP != 0).then_some(RateControl::Cqp(qp.min(51)));
    }
    if modes & sys::VA_RC_CBR != 0 {
        Some(RateControl::Cbr)
    } else if modes & sys::VA_RC_CQP != 0 {
        Some(RateControl::Cqp(DEFAULT_QP))
    } else {
        None
    }
}

/// `[video] target_usage` (1 = best quality ... 7 = fastest, like
/// GStreamer's) on the driver's own scale (1 = best ... `levels` =
/// fastest): Intel's iHD has 7 levels, Mesa's radeonsi 32. Round 1 sent 7
/// to a 32-level AMD driver, a slow high-quality preset.
pub fn quality_level(target_usage: u8, levels: u32) -> u32 {
    if levels <= 1 {
        return 1;
    }
    let tu = target_usage.clamp(1, 7) as u32;
    1 + ((tu - 1) * (levels - 1) + 3) / 6
}

/// Intel (i915) tiling modifiers, drm_fourcc.h: fourcc_mod_code(INTEL, n).
const I915_FORMAT_MOD_X_TILED: u64 = (1 << 56) | 1;
const I915_FORMAT_MOD_Y_TILED: u64 = (1 << 56) | 2;

/// Modifiers to offer for one format: what VA allocates for VPP input (the
/// probe), the plain tilings Intel's VA driver imports (KMS-grab capture
/// imports them too), then LINEAR. Order = preference.
pub fn modifier_offer(probed: Option<u64>, intel: bool) -> Vec<u64> {
    let mut v = Vec::new();
    let mut add = |m: u64| {
        if !v.contains(&m) {
            v.push(m);
        }
    };
    if let Some(m) = probed {
        add(m);
    }
    if intel {
        add(I915_FORMAT_MOD_Y_TILED);
        add(I915_FORMAT_MOD_X_TILED);
    }
    add(DRM_FORMAT_MOD_LINEAR);
    v
}

/// HRD/CPB length in milliseconds of the bitrate. GStreamer vah264enc's
/// default is 2 s; shorter bounds the burst after a big screen change.
pub const CPB_MS: u32 = 500;

/// Packed headers we write ourselves when the driver accepts them.
const WANT_PACKED: u32 = sys::VA_ENC_PACKED_HEADER_SEQUENCE | sys::VA_ENC_PACKED_HEADER_SLICE;

/// The frame rate the encoder plans for: the compositor's negotiated max
/// when it gave a plausible one, else `[video] fps`.
pub fn rate_hint(negotiated: u32, configured: u32) -> u32 {
    if (10..=240).contains(&negotiated) {
        negotiated
    } else {
        configured.max(1)
    }
}

pub fn va_fourcc(drm: u32) -> Option<u32> {
    match drm {
        DRM_XRGB8888 => Some(sys::VA_FOURCC_BGRX),
        DRM_ARGB8888 => Some(sys::VA_FOURCC_BGRA),
        DRM_XBGR8888 => Some(sys::VA_FOURCC_RGBX),
        DRM_ABGR8888 => Some(sys::VA_FOURCC_RGBA),
        _ => None,
    }
}

fn va_profile(p: Profile) -> sys::VAProfile {
    match p {
        Profile::High => sys::VAProfileH264High,
        Profile::Main => sys::VAProfileH264Main,
        Profile::ConstrainedBaseline => sys::VAProfileH264ConstrainedBaseline,
    }
}

fn invalid_pic() -> sys::VAPictureH264 {
    sys::VAPictureH264 {
        picture_id: sys::VA_INVALID_SURFACE,
        flags: sys::VA_PICTURE_H264_INVALID,
        ..Default::default()
    }
}

fn ref_pic(surface: sys::VASurfaceID, plan: &PicPlan) -> sys::VAPictureH264 {
    sys::VAPictureH264 {
        picture_id: surface,
        frame_idx: plan.frame_num,
        flags: sys::VA_PICTURE_H264_SHORT_TERM_REFERENCE,
        TopFieldOrderCnt: plan.poc as i32,
        BottomFieldOrderCnt: plan.poc as i32,
        ..Default::default()
    }
}

/// Sequence parameters, consistent with `StreamParams::sps()`.
pub fn seq_param(p: &StreamParams, kbps: u32) -> sys::VAEncSequenceParameterBufferH264 {
    let mut s = sys::VAEncSequenceParameterBufferH264 {
        seq_parameter_set_id: 0,
        level_idc: p.level_idc,
        // No periodic I/IDR frames: IDR on request only.
        intra_period: 0x7fff,
        intra_idr_period: 0x7fff,
        ip_period: 1,
        bits_per_second: kbps * 1000,
        max_num_ref_frames: 1,
        picture_width_in_mbs: p.mb_width() as u16,
        picture_height_in_mbs: p.mb_height() as u16,
        ..Default::default()
    };
    let mut f = sys::_VAEncSequenceParameterBufferH264__bindgen_ty_1__bindgen_ty_1::default();
    f.set_chroma_format_idc(1);
    f.set_frame_mbs_only_flag(1);
    f.set_direct_8x8_inference_flag(1);
    f.set_log2_max_frame_num_minus4(StreamParams::LOG2_MAX_FRAME_NUM - 4);
    f.set_pic_order_cnt_type(2);
    s.seq_fields = sys::_VAEncSequenceParameterBufferH264__bindgen_ty_1 { bits: f };
    let (cr, cb) = p.crop();
    if cr != 0 || cb != 0 {
        s.frame_cropping_flag = 1;
        s.frame_crop_right_offset = cr;
        s.frame_crop_bottom_offset = cb;
    }
    s.vui_parameters_present_flag = 1;
    let mut v = sys::_VAEncSequenceParameterBufferH264__bindgen_ty_2__bindgen_ty_1::default();
    v.set_bitstream_restriction_flag(1);
    v.set_motion_vectors_over_pic_boundaries_flag(1);
    v.set_log2_max_mv_length_horizontal(15);
    v.set_log2_max_mv_length_vertical(15);
    s.vui_fields = sys::_VAEncSequenceParameterBufferH264__bindgen_ty_2 { bits: v };
    s
}

/// Picture parameters; `reference` = the previous picture for P frames.
pub fn pic_param(
    p: &StreamParams,
    plan: &PicPlan,
    target: sys::VASurfaceID,
    reference: Option<(sys::VASurfaceID, PicPlan)>,
    coded: sys::VABufferID,
) -> sys::VAEncPictureParameterBufferH264 {
    let mut pic = sys::VAEncPictureParameterBufferH264 {
        CurrPic: sys::VAPictureH264 {
            picture_id: target,
            frame_idx: plan.frame_num,
            flags: 0,
            TopFieldOrderCnt: plan.poc as i32,
            BottomFieldOrderCnt: plan.poc as i32,
            ..Default::default()
        },
        ReferenceFrames: [invalid_pic(); 16],
        coded_buf: coded,
        frame_num: plan.frame_num as u16,
        pic_init_qp: 26,
        ..Default::default()
    };
    if let (false, Some((surface, rp))) = (plan.idr, reference) {
        pic.ReferenceFrames[0] = ref_pic(surface, &rp);
    }
    let mut f = sys::_VAEncPictureParameterBufferH264__bindgen_ty_1__bindgen_ty_1::default();
    f.set_idr_pic_flag(plan.idr as u32);
    f.set_reference_pic_flag(1);
    f.set_entropy_coding_mode_flag(p.profile.cabac() as u32);
    f.set_transform_8x8_mode_flag(p.profile.transform_8x8() as u32);
    f.set_deblocking_filter_control_present_flag(1);
    pic.pic_fields = sys::_VAEncPictureParameterBufferH264__bindgen_ty_1 { bits: f };
    pic
}

/// One slice covering the picture, consistent with `slice_header()`.
pub fn slice_param(
    p: &StreamParams,
    plan: &PicPlan,
    reference: Option<(sys::VASurfaceID, PicPlan)>,
) -> sys::VAEncSliceParameterBufferH264 {
    let mut s = sys::VAEncSliceParameterBufferH264 {
        macroblock_address: 0,
        num_macroblocks: p.mb_width() * p.mb_height(),
        macroblock_info: sys::VA_INVALID_ID,
        slice_type: plan.slice_type(),
        idr_pic_id: plan.idr_pic_id as u16,
        RefPicList0: [invalid_pic(); 32],
        RefPicList1: [invalid_pic(); 32],
        slice_qp_delta: p.slice_qp_delta,
        ..Default::default()
    };
    if let (false, Some((surface, rp))) = (plan.idr, reference) {
        s.RefPicList0[0] = ref_pic(surface, &rp);
    }
    s
}

pub fn rate_control(kbps: u32) -> sys::VAEncMiscParameterRateControl {
    let mut rc = sys::VAEncMiscParameterRateControl {
        bits_per_second: kbps * 1000,
        target_percentage: 100,
        window_size: CPB_MS,
        ..Default::default()
    };
    let mut f = sys::_VAEncMiscParameterRateControl__bindgen_ty_1__bindgen_ty_1::default();
    // Never drop a frame to save bits: the iPad shows what we send.
    f.set_disable_frame_skip(1);
    rc.rc_flags = sys::_VAEncMiscParameterRateControl__bindgen_ty_1 { bits: f };
    rc
}

pub fn hrd(kbps: u32) -> sys::VAEncMiscParameterHRD {
    let size = (kbps as u64 * CPB_MS as u64) as u32; // kbit/s * ms = bits
    sys::VAEncMiscParameterHRD {
        buffer_size: size,
        initial_buffer_fullness: size / 4 * 3,
        ..Default::default()
    }
}

fn packed(
    d: &Display,
    ctx: &Context,
    ty: u32,
    (bytes, bits): (Vec<u8>, usize),
    out: &mut Vec<Buffer>,
) -> Result<()> {
    let p = sys::VAEncPackedHeaderParameterBuffer {
        type_: ty,
        bit_length: bits as u32,
        has_emulation_bytes: 1,
        ..Default::default()
    };
    out.push(d.param(ctx, sys::VAEncPackedHeaderParameterBufferType, &p)?);
    out.push(d.buffer(
        ctx,
        sys::VAEncPackedHeaderDataBufferType,
        &bytes,
        "vaCreateBuffer (packed header)",
    )?);
    Ok(())
}

/// Frames in flight on the GPU at once: frame N+1's colour conversion runs
/// while frame N encodes (different engines).
pub const DEPTH: usize = 2;
/// Reconstructed pictures: with two encodes in flight, the one being
/// written, its reference, and the next one's target must all differ.
const RECON: usize = 3;

/// Which reconstructed picture encode number `k` writes, and which one it
/// references (the previous encode's).
pub fn recon_slots(k: u64) -> (usize, usize) {
    (
        (k % RECON as u64) as usize,
        ((k + RECON as u64 - 1) % RECON as u64) as usize,
    )
}

/// Per picture size: the encode context and its surfaces. Field order is
/// drop order: coded buffers before their context, the context before its
/// config.
struct Session {
    coded: [Buffer; DEPTH],
    ctx: Context,
    _config: Config,
    /// VPP output = encoder input, NV12 at the picture size, alternating so
    /// the next conversion doesn't overwrite a picture being encoded.
    inputs: [Surface; 2],
    /// Reconstructed pictures (16-aligned).
    recon: [Surface; RECON],
    params: StreamParams,
    /// Frame rate for the level and the rate control's per-frame budget.
    fps: u32,
    gop: Gop,
    /// Encodes submitted in this session (picks recon and coded slots).
    count: u64,
    /// The input surface with the newest picture.
    last_input: Option<usize>,
    /// The previous picture (the reference for the next P frame).
    last: Option<PicPlan>,
}

/// An encode the completion thread waits for.
struct Job {
    input: sys::VASurfaceID,
    coded: sys::VABufferID,
    width: u32,
    height: u32,
    times: StageTimes,
}

/// The completion thread and the count of encodes in flight.
struct Completion {
    jobs: Option<std::sync::mpsc::Sender<Job>>,
    in_flight: std::sync::Arc<(std::sync::Mutex<usize>, std::sync::Condvar)>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Waits for one encode; returns when it finished and its bytes.
type Worker = Box<dyn FnMut(&Job) -> Result<(u64, Vec<u8>)> + Send>;

impl Completion {
    fn start(dpy: Dpy, out: mpsc::Sender<Event>) -> Result<Completion> {
        // Encode done when the GPU no longer reads its input (as FFmpeg's
        // VA-API encoder waits), then read the bytes.
        Self::start_with(
            out,
            Box::new(move |job: &Job| {
                dpy.sync_surface(job.input)?;
                let t = now_ns();
                Ok((t, dpy.read_coded(job.coded)?))
            }),
        )
    }

    fn start_with(out: mpsc::Sender<Event>, mut work: Worker) -> Result<Completion> {
        let (tx, rx) = std::sync::mpsc::channel::<Job>();
        let in_flight =
            std::sync::Arc::new((std::sync::Mutex::new(0usize), std::sync::Condvar::new()));
        let flight = in_flight.clone();
        let thread = std::thread::Builder::new()
            .name("sidecard-va-done".into())
            .spawn(move || {
                let mut closed = false;
                for job in rx {
                    let ev = match work(&job) {
                        Ok((t, annexb)) => {
                            let mut times = job.times;
                            times.encoded_ns = t;
                            Event::Frame(Encoded {
                                annexb,
                                width: job.width,
                                height: job.height,
                                times,
                            })
                        }
                        Err(e) => Event::Ended(Err(EndError::transient(format!(
                            "VA H.264 encode: {e:#}"
                        )))),
                    };
                    if !closed && out.blocking_send(ev).is_err() {
                        closed = true; // keep draining so nobody waits forever
                    }
                    let (n, cv) = &*flight;
                    *n.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
                    cv.notify_all();
                }
            })
            .context("starting the VA completion thread")?;
        Ok(Completion {
            jobs: Some(tx),
            in_flight,
            thread: Some(thread),
        })
    }

    /// Wait until fewer than `max` encodes are in flight.
    fn wait_below(&self, max: usize) -> Result<()> {
        let (n, cv) = &*self.in_flight;
        let g = n.lock().unwrap_or_else(|e| e.into_inner());
        let (g, t) = cv
            .wait_timeout_while(g, std::time::Duration::from_secs(2), |n| *n >= max)
            .unwrap_or_else(|e| e.into_inner());
        if t.timed_out() && *g >= max {
            bail!("the GPU has not finished an encode for 2 s");
        }
        Ok(())
    }

    fn submitted(&self) {
        *self.in_flight.0.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    }

    fn cancelled(&self) {
        let (n, cv) = &*self.in_flight;
        *n.lock().unwrap_or_else(|e| e.into_inner()) -= 1;
        cv.notify_all();
    }

    fn send(&self, job: Job) {
        if let Some(tx) = &self.jobs {
            if tx.send(job).is_err() {
                self.cancelled();
            }
        }
    }
}

impl Drop for Completion {
    fn drop(&mut self) {
        if let Err(e) = self.wait_below(1) {
            tracing::warn!("closing the encoder with work in flight: {e:#}");
        }
        self.jobs = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ImportKey {
    fourcc: u32,
    modifier: u64,
    width: u32,
    height: u32,
    /// (dev, inode, offset, pitch) per plane.
    planes: [(u64, u64, u32, u32); 4],
    n: usize,
}

fn import_key(
    fourcc: u32,
    modifier: u64,
    width: u32,
    height: u32,
    planes: &[Plane],
) -> Result<ImportKey> {
    if planes.is_empty() || planes.len() > 4 {
        bail!("DMA-BUF with {} planes", planes.len());
    }
    let mut k = ImportKey {
        fourcc,
        modifier,
        width,
        height,
        planes: [(0, 0, 0, 0); 4],
        n: planes.len(),
    };
    for (i, p) in planes.iter().enumerate() {
        // SAFETY: fstat on an fd the source keeps open.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(p.fd, &mut st) } != 0 {
            bail!("fstat on a DMA-BUF fd: {}", std::io::Error::last_os_error());
        }
        k.planes[i] = (st.st_dev, st.st_ino, p.offset, p.pitch);
    }
    Ok(k)
}

pub struct VaEncoder {
    /// First: dropping it waits for the GPU and joins the thread before
    /// any surface, buffer or the display goes away.
    completion: Completion,
    session: Option<Session>,
    /// Imported compositor buffers (KWin cycles through 2-4).
    imported: Vec<(ImportKey, Surface)>,
    /// SHM fallback: (DRM fourcc, surface).
    upload: Option<(u32, Surface)>,
    vpp_ctx: Context,
    _vpp_config: Config,
    opts: VaOptions,
    profile: Profile,
    entrypoint: sys::VAEntrypoint,
    rate_control: RateControl,
    packed: u32,
    has_packed_attr: bool,
    quality_levels: u32,
    formats: Vec<(u32, Vec<u64>)>,
    import_logged: bool,
    display: Display,
}

fn h264_profile(d: &Display, ep: sys::VAEntrypoint) -> Option<Profile> {
    let have = d.profiles();
    [Profile::High, Profile::Main, Profile::ConstrainedBaseline]
        .into_iter()
        .find(|&p| have.contains(&va_profile(p)) && d.entrypoints(va_profile(p)).contains(&ep))
}

impl VaEncoder {
    /// Open the encoder; encoded frames go to `out` (from the completion
    /// thread).
    pub fn open(opts: VaOptions, out: mpsc::Sender<Event>) -> Result<VaEncoder> {
        let ep = if opts.low_power {
            sys::VAEntrypointEncSliceLP
        } else {
            sys::VAEntrypointEncSlice
        };
        let ep_name = if opts.low_power {
            "EncSliceLP (vaapi-lp)"
        } else {
            "EncSlice"
        };
        let unsupported = |m: String| anyhow::Error::new(Unsupported(m));
        let display = match &opts.device {
            Some(p) => Display::open(p)
                .map_err(|e| unsupported(format!("[video] va_device {}: {e:#}", p.display())))?,
            None => {
                let mut tried = Vec::new();
                let mut found = None;
                for node in Display::render_nodes() {
                    match Display::open(&node) {
                        Ok(d) if h264_profile(&d, ep).is_some() => {
                            found = Some(d);
                            break;
                        }
                        Ok(d) => {
                            tried.push(format!("{}: no H.264 {ep_name}", node.display()));
                            drop(d);
                        }
                        Err(e) => tried.push(format!("{e:#}")),
                    }
                }
                found.ok_or_else(|| {
                    unsupported(format!(
                        "no VA-API device can encode H.264 with {ep_name} ({}); see `vainfo`",
                        if tried.is_empty() {
                            "no /dev/dri/renderD* nodes".to_string()
                        } else {
                            tried.join("; ")
                        }
                    ))
                })?
            }
        };
        let profile = h264_profile(&display, ep).ok_or_else(|| {
            unsupported(format!(
                "{} ({}) has no H.264 {ep_name} entry point (`vainfo` lists what it has)",
                display.path.display(),
                display.vendor
            ))
        })?;
        let vp = va_profile(profile);
        if !display
            .entrypoints(sys::VAProfileNone)
            .contains(&sys::VAEntrypointVideoProc)
        {
            return Err(unsupported(format!(
                "{} has no VA video processing (needed for RGB -> NV12)",
                display.path.display()
            )));
        }
        // What each H.264 entry point offers, for the log (LP on Gen9 needs
        // the HuC firmware for any bitrate control).
        let modes_of = |e| {
            h264_profile(&display, e)
                .and_then(|p| display.attribute(va_profile(p), e, sys::VAConfigAttribRateControl))
        };
        let both = [
            (sys::VAEntrypointEncSlice, "EncSlice"),
            (sys::VAEntrypointEncSliceLP, "EncSliceLP"),
        ]
        .iter()
        .map(|&(e, n)| match modes_of(e) {
            Some(m) => format!("{n}: {}", rc_names(m)),
            None => format!("{n}: not offered"),
        })
        .collect::<Vec<_>>()
        .join("; ");
        tracing::info!("VA-API H.264 rate control by entry point: {both}");
        let rc_modes = display
            .attribute(vp, ep, sys::VAConfigAttribRateControl)
            .unwrap_or(0);
        let rate_control = choose_rate_control(rc_modes, opts.qp).ok_or_else(|| {
            unsupported(if opts.qp > 0 {
                format!(
                    "[video] qp = {} needs constant-QP rate control, which the {ep_name} H.264 encoder lacks (it has: {})",
                    opts.qp,
                    rc_names(rc_modes)
                )
            } else {
                format!(
                    "the {ep_name} H.264 encoder has neither CBR nor constant QP (it has: {}); try encoder = \"vaapi\"",
                    rc_names(rc_modes)
                )
            })
        })?;
        if let (RateControl::Cqp(q), 0) = (rate_control, opts.qp) {
            tracing::warn!(
                "the {ep_name} H.264 encoder has no CBR ({}; on Gen9 low-power encoding needs the HuC firmware for that): using constant QP {q} instead ([video] qp sets it)",
                rc_names(rc_modes)
            );
        }
        let packed_attr = display.attribute(vp, ep, sys::VAConfigAttribEncPackedHeaders);
        let quality_levels = display
            .attribute(vp, ep, sys::VAConfigAttribEncQualityRange)
            .unwrap_or(0);
        let vpp_config = display
            .config(sys::VAProfileNone, sys::VAEntrypointVideoProc, &[])
            .context("VA video processing config")?;
        let vpp_ctx = display
            .context(&vpp_config, 0, 0, &[])
            .context("VA video processing context")?;
        let intel = display.vendor.contains("Intel");
        let mut formats = Vec::new();
        let mut probes = Vec::new();
        for drm in [DRM_XRGB8888, DRM_ARGB8888, DRM_XBGR8888, DRM_ABGR8888] {
            let va = va_fourcc(drm).unwrap();
            let probed = display.probe_modifier(va);
            probes.push(format!(
                "{} {}",
                fourcc_name(drm),
                probed.map_or("failed".to_string(), |m| format!("{m:#x}"))
            ));
            formats.push((drm, modifier_offer(probed, intel)));
        }
        tracing::info!(
            "VA-API modifier the driver allocates for VPP input: {}",
            probes.join(", ")
        );
        let device = format!("{} ({})", display.path.display(), display.vendor);
        tracing::info!(
            "VA-API encoder: {device}, H.264 {:?} via {ep_name}, packed headers {:#x}, quality levels {}, DMA-BUF import offered: {}",
            profile,
            packed_attr.unwrap_or(0) & WANT_PACKED,
            quality_levels,
            formats
                .iter()
                .map(|(f, m)| format!(
                    "{} [{}]",
                    fourcc_name(*f),
                    m.iter().map(|m| format!("{m:#x}")).collect::<Vec<_>>().join(", ")
                ))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let completion = Completion::start(display.handle(), out)?;
        Ok(VaEncoder {
            completion,
            session: None,
            imported: Vec::new(),
            upload: None,
            vpp_ctx,
            _vpp_config: vpp_config,
            opts,
            profile,
            entrypoint: ep,
            rate_control,
            packed: packed_attr.unwrap_or(0) & WANT_PACKED,
            has_packed_attr: packed_attr.is_some(),
            quality_levels,
            formats,
            import_logged: false,
            display,
        })
    }

    fn session(&mut self, width: u32, height: u32, max_fps: u32) -> Result<&mut Session> {
        let fps = rate_hint(max_fps, self.opts.fps);
        let fits = self
            .session
            .as_ref()
            .is_some_and(|s| (s.params.width, s.params.height, s.fps) == (width, height, fps));
        if !fits {
            // Nothing may still read the old session's surfaces.
            self.completion.wait_below(1)?;
            self.session = None;
            let d = &self.display;
            let level = level_for(self.profile, width, height, fps, self.opts.kbps).unwrap_or(62);
            let params = StreamParams {
                profile: self.profile,
                level_idc: level,
                width,
                height,
                slice_qp_delta: match self.rate_control {
                    RateControl::Cbr => 0,
                    RateControl::Cqp(q) => q as i8 - 26,
                },
            };
            let (aw, ah) = (params.mb_width() * 16, params.mb_height() * 16);
            let mut attrs = vec![
                (sys::VAConfigAttribRTFormat, sys::VA_RT_FORMAT_YUV420),
                (
                    sys::VAConfigAttribRateControl,
                    match self.rate_control {
                        RateControl::Cbr => sys::VA_RC_CBR,
                        RateControl::Cqp(_) => sys::VA_RC_CQP,
                    },
                ),
            ];
            if self.has_packed_attr {
                attrs.push((sys::VAConfigAttribEncPackedHeaders, self.packed));
            }
            let config = d
                .config(va_profile(self.profile), self.entrypoint, &attrs)
                .context("H.264 encode config")?;
            let nv12 = |w, h| d.surface(sys::VA_RT_FORMAT_YUV420, sys::VA_FOURCC_NV12, w, h, None);
            let inputs = [nv12(width, height)?, nv12(width, height)?];
            let recon = [nv12(aw, ah)?, nv12(aw, ah)?, nv12(aw, ah)?];
            let ids: Vec<_> = recon.iter().map(|r| r.id).collect();
            let ctx = d
                .context(&config, aw, ah, &ids)
                .context("H.264 encode context")?;
            let size = (aw * ah * 3 / 2) as usize + (64 << 10);
            let coded = [d.coded_buffer(&ctx, size)?, d.coded_buffer(&ctx, size)?];
            tracing::info!(
                "VA-API encoding {width}x{height} (coded {aw}x{ah}) for up to {fps} frames/s, level {}.{}, {}, quality level {} of {} (target_usage {}), IDR on request, {DEPTH} frames in flight",
                level / 10,
                level % 10,
                match self.rate_control {
                    RateControl::Cbr => format!("CBR {} kbit/s, CPB {CPB_MS} ms", self.opts.kbps),
                    RateControl::Cqp(q) => format!("constant QP {q}"),
                },
                quality_level(self.opts.target_usage, self.quality_levels),
                self.quality_levels,
                self.opts.target_usage
            );
            self.session = Some(Session {
                coded,
                ctx,
                _config: config,
                inputs,
                recon,
                params,
                fps,
                gop: Gop::default(),
                count: 0,
                last_input: None,
                last: None,
            });
        }
        Ok(self.session.as_mut().unwrap())
    }

    fn import(
        &mut self,
        fourcc: u32,
        modifier: u64,
        width: u32,
        height: u32,
        planes: &[Plane],
    ) -> Result<sys::VASurfaceID> {
        let key = import_key(fourcc, modifier, width, height, planes)?;
        if let Some((_, s)) = self.imported.iter().find(|(k, _)| *k == key) {
            return Ok(s.id);
        }
        let va =
            va_fourcc(fourcc).ok_or_else(|| anyhow!("no VA format for {}", fourcc_name(fourcc)))?;
        let mut desc = sys::VADRMPRIMESurfaceDescriptor {
            fourcc: va,
            width,
            height,
            num_layers: 1,
            ..Default::default()
        };
        // One object per distinct DMA-BUF (planes may share one).
        let mut objects: Vec<(u64, u64)> = Vec::new();
        desc.layers[0].drm_format = fourcc;
        desc.layers[0].num_planes = planes.len() as u32;
        for (i, p) in planes.iter().enumerate() {
            let id = (key.planes[i].0, key.planes[i].1);
            let obj = match objects.iter().position(|o| *o == id) {
                Some(o) => o,
                None => {
                    let o = objects.len();
                    objects.push(id);
                    // SAFETY: size of the DMA-BUF via lseek, then rewind
                    // (the fd's file offset is shared with the compositor).
                    let size = unsafe {
                        let s = libc::lseek(p.fd, 0, libc::SEEK_END);
                        libc::lseek(p.fd, 0, libc::SEEK_SET);
                        s
                    };
                    desc.objects[o] = sys::_VADRMPRIMESurfaceDescriptor__bindgen_ty_1 {
                        fd: p.fd,
                        size: size.max(0) as u32,
                        drm_format_modifier: modifier,
                    };
                    o
                }
            };
            desc.layers[0].object_index[i] = obj as u32;
            desc.layers[0].offset[i] = p.offset;
            desc.layers[0].pitch[i] = p.pitch;
        }
        desc.num_objects = objects.len() as u32;
        let surface = self
            .display
            .import_prime(&mut desc, sys::VA_RT_FORMAT_RGB32)?;
        if !self.import_logged {
            self.import_logged = true;
            tracing::info!(
                "imported a {} DMA-BUF (modifier {modifier:#x}, {} plane(s)) into VA-API: zero copy",
                fourcc_name(fourcc),
                planes.len()
            );
        }
        if self.imported.len() >= 8 {
            self.imported.remove(0);
        }
        let id = surface.id;
        self.imported.push((key, surface));
        Ok(id)
    }

    /// Queue RGB → NV12 (BT.709, limited range) into input surface `i`.
    fn submit_vpp(&self, src: sys::VASurfaceID, i: usize) -> Result<()> {
        let s = self.session.as_ref().unwrap();
        let p = sys::VAProcPipelineParameterBuffer {
            surface: src,
            // Driver default for RGB input (sRGB, full range).
            surface_color_standard: sys::VAProcColorStandardNone,
            output_background_color: 0xff00_0000,
            output_color_standard: sys::VAProcColorStandardBT709,
            input_color_properties: sys::VAProcColorProperties {
                color_range: sys::VA_SOURCE_RANGE_FULL as u8,
                ..Default::default()
            },
            output_color_properties: sys::VAProcColorProperties {
                color_range: sys::VA_SOURCE_RANGE_REDUCED as u8,
                colour_primaries: 1,
                transfer_characteristics: 1,
                matrix_coefficients: 1,
                ..Default::default()
            },
            ..Default::default()
        };
        let buf = self
            .display
            .param(&self.vpp_ctx, sys::VAProcPipelineParameterBufferType, &p)?;
        self.display
            .submit(&self.vpp_ctx, &s.inputs[i], &[buf])
            .context("VA video processing (RGB -> NV12)")
    }

    /// Queue the encode of input surface `i`; returns the job to wait for.
    fn submit_encode(&mut self, i: usize, force_idr: bool, times: StageTimes) -> Result<Job> {
        let rc = self.rate_control;
        let (packed_flags, kbps, quality, levels) = (
            self.packed,
            self.opts.kbps,
            self.opts.target_usage,
            self.quality_levels,
        );
        let d = &self.display;
        let s = self.session.as_mut().ok_or_else(|| anyhow!("no session"))?;
        let plan = s.gop.next(force_idr);
        let (cur, prev) = recon_slots(s.count);
        let target = s.recon[cur].id;
        let reference = s.last.map(|p| (s.recon[prev].id, p));
        let coded = s.coded[(s.count % DEPTH as u64) as usize].id;
        let mut bufs: Vec<Buffer> = Vec::new();
        if plan.idr {
            bufs.push(d.param(
                &s.ctx,
                sys::VAEncSequenceParameterBufferType,
                &seq_param(&s.params, kbps),
            )?);
            if rc == RateControl::Cbr {
                bufs.push(d.misc(
                    &s.ctx,
                    sys::VAEncMiscParameterTypeRateControl,
                    &rate_control(kbps),
                )?);
                bufs.push(d.misc(&s.ctx, sys::VAEncMiscParameterTypeHRD, &hrd(kbps))?);
            }
            let fr = sys::VAEncMiscParameterFrameRate {
                framerate: s.fps,
                ..Default::default()
            };
            bufs.push(d.misc(&s.ctx, sys::VAEncMiscParameterTypeFrameRate, &fr)?);
            if levels > 1 {
                let q = sys::VAEncMiscParameterBufferQualityLevel {
                    quality_level: quality_level(quality, levels),
                    ..Default::default()
                };
                bufs.push(d.misc(&s.ctx, sys::VAEncMiscParameterTypeQualityLevel, &q)?);
            }
            if packed_flags & sys::VA_ENC_PACKED_HEADER_SEQUENCE != 0 {
                // SPS + PPS in one packed sequence header, as FFmpeg does.
                let (mut sps, sb) = s.params.sps();
                let (pps, pb) = s.params.pps();
                sps.extend(pps);
                packed(
                    d,
                    &s.ctx,
                    sys::VAEncPackedHeaderSequence,
                    (sps, sb + pb),
                    &mut bufs,
                )?;
            }
        }
        bufs.push(d.param(
            &s.ctx,
            sys::VAEncPictureParameterBufferType,
            &pic_param(&s.params, &plan, target, reference, coded),
        )?);
        if packed_flags & sys::VA_ENC_PACKED_HEADER_SLICE != 0 {
            packed(
                d,
                &s.ctx,
                sys::VAEncPackedHeaderSlice,
                s.params.slice_header(&plan),
                &mut bufs,
            )?;
        }
        bufs.push(d.param(
            &s.ctx,
            sys::VAEncSliceParameterBufferType,
            &slice_param(&s.params, &plan, reference),
        )?);
        self.completion.submitted();
        if let Err(e) = d.submit(&s.ctx, &s.inputs[i], &bufs) {
            self.completion.cancelled();
            return Err(e.context("VA H.264 encode"));
        }
        // Parameter buffers can go now (FFmpeg frees them right after
        // vaEndPicture too).
        drop(bufs);
        s.last = Some(plan);
        s.count += 1;
        Ok(Job {
            input: s.inputs[i].id,
            coded,
            width: s.params.width,
            height: s.params.height,
            times,
        })
    }
}

impl FrameSink for VaEncoder {
    fn dmabuf_formats(&self) -> Vec<(u32, Vec<u64>)> {
        self.formats.clone()
    }

    fn buffers_changed(&mut self) {
        self.imported.clear();
    }

    fn wait_ready(&mut self) -> Result<()> {
        self.completion.wait_below(DEPTH)
    }

    fn push(&mut self, frame: &Frame, force_idr: bool) -> Result<Converted> {
        self.wait_ready()?;
        self.session(frame.width, frame.height, frame.max_fps)?;
        let src = match frame.pixels {
            Pixels::DmaBuf {
                fourcc,
                modifier,
                planes,
            } => match self.import(fourcc, modifier, frame.width, frame.height, planes) {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!("{e:#}");
                    return Ok(Converted::ImportFailed { fourcc, modifier });
                }
            },
            Pixels::Shm {
                fourcc,
                data,
                stride,
            } => {
                let Some(va) = va_fourcc(fourcc) else {
                    return Ok(Converted::Skipped(format!(
                        "pixel format {} not supported",
                        fourcc_name(fourcc)
                    )));
                };
                let fits = self.upload.as_ref().is_some_and(|(f, s)| {
                    *f == fourcc && (s.width, s.height) == (frame.width, frame.height)
                });
                if !fits {
                    self.upload = None;
                    let s = self.display.surface(
                        sys::VA_RT_FORMAT_RGB32,
                        va,
                        frame.width,
                        frame.height,
                        Some(sys::VA_SURFACE_ATTRIB_USAGE_HINT_VPP_READ),
                    )?;
                    tracing::info!(
                        "no DMA-BUF from the compositor: uploading {} frames through the CPU",
                        fourcc_name(fourcc)
                    );
                    self.upload = Some((fourcc, s));
                }
                let (_, s) = self.upload.as_ref().unwrap();
                self.display
                    .upload(s, va, data, stride as usize)
                    .context("uploading a shared-memory frame")?;
                s.id
            }
        };
        let dpy = self.display.handle();
        let mut times = frame.times;
        // Ready: KWin's own GPU render into the buffer has finished (the
        // DMA-BUF's implicit fence), or our upload is in.
        dpy.sync_surface(src)
            .context("waiting for the compositor's render")?;
        times.ready_ns = now_ns();
        // Convert into the input surface the in-flight encode isn't reading,
        // then queue the encode right behind it: no CPU wait in between.
        let i = self
            .session
            .as_ref()
            .and_then(|s| s.last_input)
            .map_or(0, |l| 1 - l);
        self.submit_vpp(src, i)?;
        let mut job = self.submit_encode(i, force_idr, times)?;
        // The conversion has read the compositor's buffer once the GPU is
        // done with it: then KWin can have it back.
        let synced = dpy.sync_surface(src);
        job.times.converted_ns = now_ns();
        self.session.as_mut().unwrap().last_input = Some(i);
        // Hand the job over even if that wait failed: it is in flight.
        self.completion.send(job);
        synced.context("VA video processing (RGB -> NV12)")?;
        Ok(Converted::Done)
    }

    fn repeat(&mut self, force_idr: bool) -> Result<bool> {
        let Some(i) = self.session.as_ref().and_then(|s| s.last_input) else {
            return Ok(false);
        };
        self.wait_ready()?;
        let n = now_ns();
        let times = StageTimes {
            arrived_ns: n,
            converted_ns: n,
            repeat: true,
            ..Default::default()
        };
        let job = self.submit_encode(i, force_idr, times)?;
        self.completion.send(job);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params() -> StreamParams {
        StreamParams {
            profile: Profile::High,
            level_idc: 51,
            width: 2360,
            height: 1640,
            slice_qp_delta: 0,
        }
    }

    #[test]
    fn seq_params_match_the_sps_we_write() {
        let s = seq_param(&params(), 20_000);
        assert_eq!(
            (s.picture_width_in_mbs, s.picture_height_in_mbs),
            (148, 103)
        );
        assert_eq!(
            (
                s.frame_cropping_flag,
                s.frame_crop_right_offset,
                s.frame_crop_bottom_offset
            ),
            (1, 4, 4)
        );
        assert_eq!(s.bits_per_second, 20_000_000);
        assert_eq!((s.ip_period, s.max_num_ref_frames), (1, 1));
        // SAFETY: we wrote `bits`.
        let f = unsafe { s.seq_fields.bits };
        assert_eq!(f.pic_order_cnt_type(), 2);
        assert_eq!(f.log2_max_frame_num_minus4(), 4);
        assert_eq!(f.frame_mbs_only_flag(), 1);
        let v = unsafe { s.vui_fields.bits };
        assert_eq!(v.bitstream_restriction_flag(), 1);
        assert_eq!(v.timing_info_present_flag(), 0);
    }

    #[test]
    fn p_frames_reference_the_previous_picture_and_idrs_nothing() {
        let p = params();
        let mut g = Gop::default();
        let idr = g.next(false);
        let pic = pic_param(&p, &idr, 10, None, 99);
        let f = unsafe { pic.pic_fields.bits };
        assert_eq!((f.idr_pic_flag(), f.reference_pic_flag()), (1, 1));
        assert_eq!(f.entropy_coding_mode_flag(), 1);
        assert_eq!(f.transform_8x8_mode_flag(), 1);
        assert!(pic
            .ReferenceFrames
            .iter()
            .all(|r| r.flags == sys::VA_PICTURE_H264_INVALID));
        assert_eq!(pic.coded_buf, 99);
        assert_eq!(slice_param(&p, &idr, None).slice_type, 2);

        let pf = g.next(false);
        let pic = pic_param(&p, &pf, 11, Some((10, idr)), 99);
        assert_eq!(unsafe { pic.pic_fields.bits }.idr_pic_flag(), 0);
        assert_eq!(pic.CurrPic.picture_id, 11);
        assert_eq!((pic.frame_num, pic.CurrPic.TopFieldOrderCnt), (1, 2));
        assert_eq!(pic.ReferenceFrames[0].picture_id, 10);
        assert_eq!(
            pic.ReferenceFrames[0].flags,
            sys::VA_PICTURE_H264_SHORT_TERM_REFERENCE
        );
        assert_eq!(pic.ReferenceFrames[1].flags, sys::VA_PICTURE_H264_INVALID);
        let sl = slice_param(&p, &pf, Some((10, idr)));
        assert_eq!((sl.slice_type, sl.num_macroblocks), (0, 148 * 103));
        assert_eq!(sl.RefPicList0[0].picture_id, 10);
        assert_eq!(sl.RefPicList0[1].picture_id, sys::VA_INVALID_SURFACE);
        assert_eq!((sl.slice_qp_delta, sl.cabac_init_idc), (0, 0));

        // A requested IDR mid-stream drops the reference.
        let again = g.next(true);
        let pic = pic_param(&p, &again, 10, Some((11, pf)), 99);
        assert_eq!(pic.ReferenceFrames[0].flags, sys::VA_PICTURE_H264_INVALID);
        assert_eq!(slice_param(&p, &again, Some((11, pf))).idr_pic_id, 1);
    }

    #[test]
    fn recon_slots_never_overwrite_a_reference_in_flight() {
        // With at most DEPTH encodes in flight, encode k writes a slot that
        // neither its own reference nor encode k-1 (still running) uses.
        for k in 1..20u64 {
            let (cur, prev) = recon_slots(k);
            let (cur_prev, _) = recon_slots(k - 1);
            assert_eq!(prev, cur_prev, "k={k} references what k-1 wrote");
            assert_ne!(cur, prev);
            if k >= 2 {
                let (_, prev_prev) = recon_slots(k - 1);
                assert_ne!(cur, prev_prev, "k={k} must not overwrite k-1's reference");
            }
        }
        assert_eq!(recon_slots(0), (0, 2));
    }

    fn job(id: u32) -> Job {
        Job {
            input: id,
            coded: id,
            width: 2,
            height: 2,
            times: StageTimes::default(),
        }
    }

    #[test]
    fn completion_delivers_in_order_and_limits_frames_in_flight() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let gate = Arc::new(AtomicBool::new(false));
        let g = gate.clone();
        let c = Completion::start_with(
            tx,
            Box::new(move |j: &Job| {
                // The "GPU" holds the first job until the gate opens.
                while j.input == 1 && !g.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                Ok((u64::from(j.input), vec![j.input as u8]))
            }),
        )
        .unwrap();
        for i in 1..=DEPTH as u32 {
            c.wait_below(DEPTH).unwrap();
            c.submitted();
            c.send(job(i));
        }
        // Full: a third frame has to wait.
        let t = std::time::Instant::now();
        let opener = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            gate.store(true, Ordering::SeqCst);
        });
        c.wait_below(DEPTH).unwrap();
        assert!(t.elapsed() >= std::time::Duration::from_millis(40));
        opener.join().unwrap();
        c.submitted();
        c.send(job(3));
        drop(c); // waits for everything, joins the thread
        let got: Vec<u8> = rt.block_on(async {
            let mut v = Vec::new();
            while let Some(Event::Frame(e)) = rx.recv().await {
                assert_eq!(e.times.encoded_ns, u64::from(e.annexb[0]));
                v.push(e.annexb[0]);
            }
            v
        });
        assert_eq!(got, vec![1, 2, 3]);
    }

    #[test]
    fn completion_reports_a_failed_encode_and_keeps_going() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let (tx, mut rx) = mpsc::channel(16);
        let c = Completion::start_with(
            tx,
            Box::new(|j: &Job| {
                if j.input == 2 {
                    bail!("GPU hang")
                }
                Ok((0, vec![]))
            }),
        )
        .unwrap();
        for i in 1..=3 {
            c.wait_below(DEPTH).unwrap();
            c.submitted();
            c.send(job(i));
        }
        c.wait_below(1).unwrap(); // nothing stuck in flight
        drop(c);
        let kinds: Vec<String> = rt.block_on(async {
            let mut v = Vec::new();
            while let Some(e) = rx.recv().await {
                v.push(match e {
                    Event::Frame(_) => "frame".to_string(),
                    Event::Ended(Err(e)) => format!("ended: {}", e.msg),
                    other => format!("{other:?}"),
                });
            }
            v
        });
        assert_eq!(
            kinds,
            vec!["frame", "ended: VA H.264 encode: GPU hang", "frame"]
        );
    }

    #[test]
    fn quality_level_spans_the_drivers_range() {
        // Intel iHD: 7 levels, the same numbers.
        for tu in 1..=7 {
            assert_eq!(quality_level(tu, 7), tu as u32);
        }
        // Mesa radeonsi: 32 levels; 7 must be the fastest, 1 the best.
        assert_eq!(quality_level(7, 32), 32);
        assert_eq!(quality_level(1, 32), 1);
        assert_eq!(quality_level(4, 32), 17);
        assert_eq!(quality_level(9, 32), 32); // clamped
        assert_eq!(quality_level(7, 0), 1);
    }

    #[test]
    fn rate_control_choice() {
        let cbr_cqp = sys::VA_RC_CBR | sys::VA_RC_VBR | sys::VA_RC_CQP;
        assert_eq!(choose_rate_control(cbr_cqp, 0), Some(RateControl::Cbr));
        // Gen9 low power without HuC: modes 0x10 = CQP only.
        assert_eq!(
            choose_rate_control(0x10, 0),
            Some(RateControl::Cqp(DEFAULT_QP))
        );
        assert_eq!(choose_rate_control(0x10, 30), Some(RateControl::Cqp(30)));
        assert_eq!(choose_rate_control(cbr_cqp, 20), Some(RateControl::Cqp(20)));
        assert_eq!(choose_rate_control(sys::VA_RC_CBR, 20), None);
        assert_eq!(choose_rate_control(sys::VA_RC_VBR, 0), None);
        assert_eq!(rc_names(0x10), "CQP");
        assert_eq!(rc_names(cbr_cqp), "CBR VBR CQP");
    }

    #[test]
    fn intel_gets_tiled_modifiers_before_linear() {
        assert_eq!(
            modifier_offer(Some(I915_FORMAT_MOD_Y_TILED), true),
            vec![I915_FORMAT_MOD_Y_TILED, I915_FORMAT_MOD_X_TILED, 0]
        );
        assert_eq!(
            modifier_offer(None, true),
            vec![I915_FORMAT_MOD_Y_TILED, I915_FORMAT_MOD_X_TILED, 0]
        );
        let amd = 0x0200_0000_0001_2345u64;
        assert_eq!(modifier_offer(Some(amd), false), vec![amd, 0]);
        assert_eq!(modifier_offer(Some(0), false), vec![0]);
    }

    #[test]
    fn constant_qp_reaches_the_slice() {
        let p = StreamParams {
            slice_qp_delta: -2,
            ..params()
        };
        let mut g = Gop::default();
        assert_eq!(slice_param(&p, &g.next(false), None).slice_qp_delta, -2);
    }

    #[test]
    fn rate_hint_prefers_the_negotiated_rate() {
        assert_eq!(rate_hint(120, 60), 120);
        assert_eq!(rate_hint(0, 60), 60); // unknown
        assert_eq!(rate_hint(1000, 60), 60); // "unlimited" test sources
        assert_eq!(rate_hint(0, 0), 1);
    }

    #[test]
    fn rate_control_is_cbr_with_a_short_cpb() {
        let rc = rate_control(20_000);
        assert_eq!(
            (rc.bits_per_second, rc.target_percentage),
            (20_000_000, 100)
        );
        assert_eq!(rc.window_size, CPB_MS);
        assert_eq!(unsafe { rc.rc_flags.bits }.disable_frame_skip(), 1);
        let h = hrd(20_000);
        assert_eq!(h.buffer_size, 10_000_000);
        assert_eq!(h.initial_buffer_fullness, 7_500_000);
    }

    #[test]
    fn import_keys_identify_buffers() {
        // Two planes of one (fake) DMA-BUF: a memfd stands in.
        use std::os::fd::AsRawFd;
        let f = std::fs::File::from(memfd().expect("memfd"));
        let fd = f.as_raw_fd();
        let planes = [
            Plane {
                fd,
                offset: 0,
                pitch: 9472,
            },
            Plane {
                fd,
                offset: 15_000_000,
                pitch: 512,
            },
        ];
        let a = import_key(DRM_XRGB8888, 7, 2360, 1640, &planes).unwrap();
        let b = import_key(DRM_XRGB8888, 7, 2360, 1640, &planes).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.n, 2);
        assert_eq!(a.planes[0].1, a.planes[1].1, "same inode");
        let c = import_key(DRM_XRGB8888, 8, 2360, 1640, &planes).unwrap();
        assert_ne!(a, c, "another modifier is another import");
        assert!(import_key(DRM_XRGB8888, 7, 1, 1, &[]).is_err());
    }

    fn memfd() -> std::io::Result<std::os::fd::OwnedFd> {
        // SAFETY: memfd_create with a static name.
        let fd = unsafe { libc::memfd_create(c"sidecard-test".as_ptr(), 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: we own the new fd.
        Ok(unsafe { std::os::fd::FromRawFd::from_raw_fd(fd) })
    }
}
