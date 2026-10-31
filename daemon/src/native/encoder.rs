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
use super::stats::StageTimes;
use super::va::{Buffer, Config, Context, Display, Surface};
use super::va_sys as sys;
use crate::clock::now_ns;
use anyhow::{anyhow, bail, Context as _, Result};
use std::path::PathBuf;

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
}

/// HRD/CPB length in milliseconds of the bitrate. GStreamer vah264enc's
/// default is 2 s; shorter bounds the burst after a big screen change.
pub const CPB_MS: u32 = 500;

/// Packed headers we write ourselves when the driver accepts them.
const WANT_PACKED: u32 = sys::VA_ENC_PACKED_HEADER_SEQUENCE | sys::VA_ENC_PACKED_HEADER_SLICE;

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

/// Per picture size: the encode context and its surfaces. Field order is
/// drop order: the coded buffer before its context, the context before its
/// config.
struct Session {
    coded: Buffer,
    ctx: Context,
    _config: Config,
    /// VPP output = encoder input, NV12 at the picture size.
    input: Surface,
    /// Reconstructed pictures (16-aligned): current and reference.
    recon: [Surface; 2],
    params: StreamParams,
    gop: Gop,
    cur: usize,
    /// The previous picture (the reference for the next P frame).
    last: Option<PicPlan>,
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
    packed: u32,
    has_packed_attr: bool,
    quality_levels: u32,
    formats: Vec<(u32, Vec<u64>)>,
    pending: Option<StageTimes>,
    have_picture: bool,
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
    pub fn open(opts: VaOptions) -> Result<VaEncoder> {
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
        let display = match &opts.device {
            Some(p) => Display::open(p)?,
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
                    anyhow!(
                        "no VA-API device can encode H.264 with {ep_name} ({}); see `vainfo`",
                        if tried.is_empty() {
                            "no /dev/dri/renderD* nodes".to_string()
                        } else {
                            tried.join("; ")
                        }
                    )
                })?
            }
        };
        let profile = h264_profile(&display, ep).ok_or_else(|| {
            anyhow!(
                "{} ({}) has no H.264 {ep_name} entry point (`vainfo` lists what it has)",
                display.path.display(),
                display.vendor
            )
        })?;
        let vp = va_profile(profile);
        if !display
            .entrypoints(sys::VAProfileNone)
            .contains(&sys::VAEntrypointVideoProc)
        {
            bail!(
                "{} has no VA video processing (needed for RGB -> NV12)",
                display.path.display()
            );
        }
        let rc = display
            .attribute(vp, ep, sys::VAConfigAttribRateControl)
            .unwrap_or(0);
        if rc & sys::VA_RC_CBR == 0 {
            bail!("the {ep_name} H.264 encoder has no CBR rate control (modes {rc:#x})");
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
        let mut formats = Vec::new();
        for drm in [DRM_XRGB8888, DRM_ARGB8888, DRM_XBGR8888, DRM_ABGR8888] {
            let va = va_fourcc(drm).unwrap();
            let mut mods = Vec::new();
            if let Some(m) = display.probe_modifier(va) {
                mods.push(m);
            }
            if !mods.contains(&DRM_FORMAT_MOD_LINEAR) {
                mods.push(DRM_FORMAT_MOD_LINEAR);
            }
            formats.push((drm, mods));
        }
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
        Ok(VaEncoder {
            session: None,
            imported: Vec::new(),
            upload: None,
            vpp_ctx,
            _vpp_config: vpp_config,
            opts,
            profile,
            entrypoint: ep,
            packed: packed_attr.unwrap_or(0) & WANT_PACKED,
            has_packed_attr: packed_attr.is_some(),
            quality_levels,
            formats,
            pending: None,
            have_picture: false,
            import_logged: false,
            display,
        })
    }

    fn session(&mut self, width: u32, height: u32) -> Result<&mut Session> {
        let fits = self
            .session
            .as_ref()
            .is_some_and(|s| (s.params.width, s.params.height) == (width, height));
        if !fits {
            self.session = None;
            let d = &self.display;
            let level =
                level_for(self.profile, width, height, self.opts.fps, self.opts.kbps).unwrap_or(62);
            let params = StreamParams {
                profile: self.profile,
                level_idc: level,
                width,
                height,
            };
            let (aw, ah) = (params.mb_width() * 16, params.mb_height() * 16);
            let mut attrs = vec![
                (sys::VAConfigAttribRTFormat, sys::VA_RT_FORMAT_YUV420),
                (sys::VAConfigAttribRateControl, sys::VA_RC_CBR),
            ];
            if self.has_packed_attr {
                attrs.push((sys::VAConfigAttribEncPackedHeaders, self.packed));
            }
            let config = d
                .config(va_profile(self.profile), self.entrypoint, &attrs)
                .context("H.264 encode config")?;
            let nv12 = |w, h| d.surface(sys::VA_RT_FORMAT_YUV420, sys::VA_FOURCC_NV12, w, h, None);
            let input = nv12(width, height)?;
            let recon = [nv12(aw, ah)?, nv12(aw, ah)?];
            let ctx = d
                .context(&config, aw, ah, &[recon[0].id, recon[1].id])
                .context("H.264 encode context")?;
            let coded = d.coded_buffer(&ctx, (aw * ah * 3 / 2) as usize + (64 << 10))?;
            tracing::info!(
                "VA-API encoding {width}x{height} (coded {aw}x{ah}), level {}.{}, CBR {} kbit/s, CPB {CPB_MS} ms, quality level {}, IDR on request",
                level / 10,
                level % 10,
                self.opts.kbps,
                self.opts.target_usage
            );
            self.session = Some(Session {
                coded,
                ctx,
                _config: config,
                input,
                recon,
                params,
                gop: Gop::default(),
                cur: 0,
                last: None,
            });
            self.have_picture = false;
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

    /// RGB → NV12 (BT.709, limited range) into the session's input surface.
    fn vpp(&self, src: sys::VASurfaceID) -> Result<()> {
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
            .run(&self.vpp_ctx, &s.input, &[buf])
            .context("VA video processing (RGB -> NV12)")
    }
}

impl FrameSink for VaEncoder {
    fn dmabuf_formats(&self) -> Vec<(u32, Vec<u64>)> {
        self.formats.clone()
    }

    fn buffers_changed(&mut self) {
        self.imported.clear();
    }

    fn convert(&mut self, frame: &Frame) -> Result<Converted> {
        self.session(frame.width, frame.height)?;
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
        self.vpp(src)?;
        let mut times = frame.times;
        times.converted_ns = now_ns();
        self.pending = Some(times);
        self.have_picture = true;
        Ok(Converted::Done)
    }

    fn encode(&mut self, force_idr: bool) -> Result<Option<Encoded>> {
        let mut times = match self.pending.take() {
            Some(t) => t,
            // An idle keyframe: re-encode the last picture.
            None if force_idr && self.have_picture => {
                let n = now_ns();
                StageTimes {
                    arrived_ns: n,
                    converted_ns: n,
                    ..Default::default()
                }
            }
            None => return Ok(None),
        };
        let (packed_flags, kbps, quality, levels) = (
            self.packed,
            self.opts.kbps,
            self.opts.target_usage as u32,
            self.quality_levels,
        );
        let d = &self.display;
        let s = self.session.as_mut().ok_or_else(|| anyhow!("no session"))?;
        let plan = s.gop.next(force_idr);
        let target = s.recon[s.cur].id;
        let reference = s.last.map(|p| (s.recon[1 - s.cur].id, p));
        let mut bufs: Vec<Buffer> = Vec::new();
        if plan.idr {
            bufs.push(d.param(
                &s.ctx,
                sys::VAEncSequenceParameterBufferType,
                &seq_param(&s.params, kbps),
            )?);
            bufs.push(d.misc(
                &s.ctx,
                sys::VAEncMiscParameterTypeRateControl,
                &rate_control(kbps),
            )?);
            bufs.push(d.misc(&s.ctx, sys::VAEncMiscParameterTypeHRD, &hrd(kbps))?);
            let fr = sys::VAEncMiscParameterFrameRate {
                framerate: self.opts.fps,
                ..Default::default()
            };
            bufs.push(d.misc(&s.ctx, sys::VAEncMiscParameterTypeFrameRate, &fr)?);
            if levels > 1 {
                let q = sys::VAEncMiscParameterBufferQualityLevel {
                    quality_level: quality.clamp(1, levels),
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
            &pic_param(&s.params, &plan, target, reference, s.coded.id),
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
        d.run(&s.ctx, &s.input, &bufs).context("VA H.264 encode")?;
        drop(bufs);
        let annexb = d.read_coded(&s.coded)?;
        s.last = Some(plan);
        s.cur = 1 - s.cur;
        times.encoded_ns = now_ns();
        Ok(Some(Encoded {
            annexb,
            width: s.params.width,
            height: s.params.height,
            times,
        }))
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
