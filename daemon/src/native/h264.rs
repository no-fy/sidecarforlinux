//! H.264 headers for the native VA-API encoder: SPS, PPS and slice headers
//! written by us (VA-API "packed headers"), the level for a size and frame
//! rate, and the frame-type schedule (IDR at start and on request, P frames
//! otherwise, one reference, no B-frames).
//!
//! Written from ITU-T H.264 (06/2019) §7.3 syntax; the parameter choices
//! follow what GStreamer `vah264enc` and FFmpeg `h264_vaapi` send to the
//! same drivers (see docs/research/native-video.md §3). No code copied.

/// MSB-first bit writer for RBSP syntax (`u(n)`, `ue(v)`, `se(v)`).
#[derive(Debug, Default, Clone)]
pub struct BitWriter {
    bytes: Vec<u8>,
    /// Bits used in the last byte (0 = byte aligned).
    used: u8,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bit(&mut self, b: bool) {
        if self.used == 0 {
            self.bytes.push(0);
        }
        if b {
            *self.bytes.last_mut().unwrap() |= 0x80 >> self.used;
        }
        self.used = (self.used + 1) % 8;
    }

    /// `u(n)`: the low `n` bits of `v`, MSB first.
    pub fn u(&mut self, n: u32, v: u32) {
        debug_assert!(n <= 32);
        for i in (0..n).rev() {
            self.bit((v >> i) & 1 == 1);
        }
    }

    pub fn flag(&mut self, b: bool) {
        self.bit(b);
    }

    /// `ue(v)`: unsigned Exp-Golomb.
    pub fn ue(&mut self, v: u32) {
        let x = v as u64 + 1;
        let len = 64 - x.leading_zeros(); // bits in x
        for _ in 0..len - 1 {
            self.bit(false);
        }
        for i in (0..len).rev() {
            self.bit((x >> i) & 1 == 1);
        }
    }

    /// `se(v)`: signed Exp-Golomb (k > 0 → 2k-1, k ≤ 0 → -2k).
    pub fn se(&mut self, v: i32) {
        let m = if v > 0 {
            2 * v as i64 - 1
        } else {
            -2 * v as i64
        };
        self.ue(m as u32);
    }

    /// `rbsp_trailing_bits()`: a 1, then zeros to the byte boundary.
    pub fn trailing_bits(&mut self) {
        self.bit(true);
        while self.used != 0 {
            self.bit(false);
        }
    }

    pub fn bit_len(&self) -> usize {
        if self.used == 0 {
            self.bytes.len() * 8
        } else {
            (self.bytes.len() - 1) * 8 + self.used as usize
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A NAL unit as VA-API wants it in a packed header: Annex-B start code,
/// NAL header, RBSP with emulation-prevention bytes
/// (`has_emulation_bytes = 1`). Returns (bytes, length in bits): a slice
/// header doesn't end on a byte boundary, the driver appends the slice data.
pub fn nal(nal_ref_idc: u8, nal_unit_type: u8, rbsp: &BitWriter) -> (Vec<u8>, usize) {
    let mut out = vec![
        0,
        0,
        0,
        1,
        ((nal_ref_idc & 3) << 5) | (nal_unit_type & 0x1f),
    ];
    let body = rbsp.bytes();
    // Only whole bytes can be checked for start-code emulation; a partial
    // last byte (slice header) is continued by the driver.
    let whole = if rbsp.used == 0 {
        body.len()
    } else {
        body.len() - 1
    };
    let mut zeros = 0;
    for &b in &body[..whole] {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    let mut bits = out.len() * 8;
    if whole < body.len() {
        out.push(body[whole]);
        bits += rbsp.used as usize;
    }
    (out, bits)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    ConstrainedBaseline,
    Main,
    High,
}

impl Profile {
    pub fn idc(self) -> u8 {
        match self {
            Profile::ConstrainedBaseline => 66,
            Profile::Main => 77,
            Profile::High => 100,
        }
    }

    pub fn cabac(self) -> bool {
        self != Profile::ConstrainedBaseline
    }

    pub fn transform_8x8(self) -> bool {
        self == Profile::High
    }
}

/// Table A-1: (level_idc, MaxMBPS, MaxFS, MaxBR in 1000 bit/s for
/// Baseline/Main, MaxDpbMbs).
const LEVELS: &[(u8, u32, u32, u32, u32)] = &[
    (10, 1_485, 99, 64, 396),
    (11, 3_000, 396, 192, 900),
    (12, 6_000, 396, 384, 2_376),
    (13, 11_880, 396, 768, 2_376),
    (20, 11_880, 396, 2_000, 2_376),
    (21, 19_800, 792, 4_000, 4_752),
    (22, 20_250, 1_620, 4_000, 8_100),
    (30, 40_500, 1_620, 10_000, 8_100),
    (31, 108_000, 3_600, 14_000, 18_000),
    (32, 216_000, 5_120, 20_000, 20_480),
    (40, 245_760, 8_192, 20_000, 32_768),
    (41, 245_760, 8_192, 50_000, 32_768),
    (42, 522_240, 8_704, 50_000, 34_816),
    (50, 589_824, 22_080, 135_000, 110_400),
    (51, 983_040, 36_864, 240_000, 184_320),
    (52, 2_073_600, 36_864, 240_000, 184_320),
    (60, 4_177_920, 139_264, 240_000, 696_320),
    (61, 8_355_840, 139_264, 480_000, 696_320),
    (62, 16_711_680, 139_264, 800_000, 696_320),
];

/// Smallest level for this picture size, frame rate, bitrate and one
/// reference frame. `None` if even 6.2 is too small.
pub fn level_for(profile: Profile, width: u32, height: u32, fps: u32, kbps: u32) -> Option<u8> {
    let (mbw, mbh) = (width.div_ceil(16), height.div_ceil(16));
    let fs = mbw * mbh;
    let mbps = fs as u64 * fps.max(1) as u64;
    // High profile allows 1.25x the bitrate (cpbBrVclFactor 1250 vs 1000).
    let br_factor = if profile == Profile::High { 1250 } else { 1000 };
    LEVELS
        .iter()
        .find(|&&(_, max_mbps, max_fs, max_br, max_dpb)| {
            let side = ((8 * max_fs) as f64).sqrt() as u32;
            mbps <= max_mbps as u64
                && fs <= max_fs
                && mbw <= side
                && mbh <= side
                && kbps as u64 * 1000 <= max_br as u64 * br_factor
                && fs <= max_dpb
        })
        .map(|l| l.0)
}

/// Everything the SPS, PPS and slice headers depend on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamParams {
    pub profile: Profile,
    pub level_idc: u8,
    /// Picture size (cropped); the coded size is rounded up to 16.
    pub width: u32,
    pub height: u32,
}

impl StreamParams {
    pub const LOG2_MAX_FRAME_NUM: u32 = 8;
    pub const MAX_FRAME_NUM: u32 = 1 << Self::LOG2_MAX_FRAME_NUM;

    pub fn mb_width(&self) -> u32 {
        self.width.div_ceil(16)
    }

    pub fn mb_height(&self) -> u32 {
        self.height.div_ceil(16)
    }

    /// (right, bottom) crop in 2-pixel units (4:2:0).
    pub fn crop(&self) -> (u32, u32) {
        (
            (self.mb_width() * 16 - self.width) / 2,
            (self.mb_height() * 16 - self.height) / 2,
        )
    }

    /// Sequence parameter set (NAL type 7).
    pub fn sps(&self) -> (Vec<u8>, usize) {
        let p = self.profile;
        let mut w = BitWriter::new();
        w.u(8, p.idc() as u32);
        // constraint_set0..5 + reserved_zero_2bits
        let (c0, c1, c4, c5) = match p {
            Profile::ConstrainedBaseline => (true, true, false, false),
            // set4: frame_mbs_only; set5: no B slices (Main/High).
            Profile::Main => (false, true, true, true),
            Profile::High => (false, false, true, true),
        };
        for c in [c0, c1, false, false, c4, c5, false, false] {
            w.flag(c);
        }
        w.u(8, self.level_idc as u32);
        w.ue(0); // seq_parameter_set_id
        if p == Profile::High {
            w.ue(1); // chroma_format_idc 4:2:0
            w.ue(0); // bit_depth_luma_minus8
            w.ue(0); // bit_depth_chroma_minus8
            w.flag(false); // qpprime_y_zero_transform_bypass_flag
            w.flag(false); // seq_scaling_matrix_present_flag
        }
        w.ue(Self::LOG2_MAX_FRAME_NUM - 4);
        // POC type 2: output order = decoding order, no POC in slice headers.
        w.ue(2);
        w.ue(1); // max_num_ref_frames
        w.flag(false); // gaps_in_frame_num_value_allowed_flag
        w.ue(self.mb_width() - 1);
        w.ue(self.mb_height() - 1);
        w.flag(true); // frame_mbs_only_flag
        w.flag(true); // direct_8x8_inference_flag
        let (cr, cb) = self.crop();
        w.flag(cr != 0 || cb != 0);
        if cr != 0 || cb != 0 {
            w.ue(0);
            w.ue(cr);
            w.ue(0);
            w.ue(cb);
        }
        w.flag(true); // vui_parameters_present_flag
        self.vui(&mut w);
        w.trailing_bits();
        nal(3, 7, &w)
    }

    /// VUI: BT.709 limited range (what VPP writes), and "no reordering,
    /// one frame of DPB" so the decoder can output every frame at once.
    fn vui(&self, w: &mut BitWriter) {
        w.flag(false); // aspect_ratio_info_present_flag (square pixels)
        w.flag(false); // overscan_info_present_flag
        w.flag(true); // video_signal_type_present_flag
        w.u(3, 5); // video_format: unspecified
        w.flag(false); // video_full_range_flag: limited
        w.flag(true); // colour_description_present_flag
        w.u(8, 1); // colour_primaries BT.709
        w.u(8, 1); // transfer_characteristics BT.709
        w.u(8, 1); // matrix_coefficients BT.709
        w.flag(false); // chroma_loc_info_present_flag
        w.flag(false); // timing_info_present_flag (frames come when the screen changes)
        w.flag(false); // nal_hrd_parameters_present_flag
        w.flag(false); // vcl_hrd_parameters_present_flag
        w.flag(false); // pic_struct_present_flag
        w.flag(true); // bitstream_restriction_flag
        w.flag(true); // motion_vectors_over_pic_boundaries_flag
        w.ue(0); // max_bytes_per_pic_denom
        w.ue(0); // max_bits_per_mb_denom
        w.ue(15); // log2_max_mv_length_horizontal
        w.ue(15); // log2_max_mv_length_vertical
        w.ue(0); // max_num_reorder_frames
        w.ue(1); // max_dec_frame_buffering
    }

    /// Picture parameter set (NAL type 8).
    pub fn pps(&self) -> (Vec<u8>, usize) {
        let p = self.profile;
        let mut w = BitWriter::new();
        w.ue(0); // pic_parameter_set_id
        w.ue(0); // seq_parameter_set_id
        w.flag(p.cabac()); // entropy_coding_mode_flag
        w.flag(false); // bottom_field_pic_order_in_frame_present_flag
        w.ue(0); // num_slice_groups_minus1
        w.ue(0); // num_ref_idx_l0_default_active_minus1
        w.ue(0); // num_ref_idx_l1_default_active_minus1
        w.flag(false); // weighted_pred_flag
        w.u(2, 0); // weighted_bipred_idc
        w.se(0); // pic_init_qp_minus26
        w.se(0); // pic_init_qs_minus26
        w.se(0); // chroma_qp_index_offset
        w.flag(true); // deblocking_filter_control_present_flag
        w.flag(false); // constrained_intra_pred_flag
        w.flag(false); // redundant_pic_cnt_present_flag
        if p.transform_8x8() {
            w.flag(true); // transform_8x8_mode_flag
            w.flag(false); // pic_scaling_matrix_present_flag
            w.se(0); // second_chroma_qp_index_offset
        }
        w.trailing_bits();
        nal(3, 8, &w)
    }

    /// Slice header for a whole-picture slice, up to (not including) the
    /// slice data. QP is left at the PPS default (`slice_qp_delta = 0`): the
    /// driver's rate control sets the real QP, as with GStreamer and FFmpeg.
    pub fn slice_header(&self, pic: &PicPlan) -> (Vec<u8>, usize) {
        let mut w = BitWriter::new();
        w.ue(0); // first_mb_in_slice
        w.ue(pic.slice_type() as u32);
        w.ue(0); // pic_parameter_set_id
        w.u(Self::LOG2_MAX_FRAME_NUM, pic.frame_num);
        if pic.idr {
            w.ue(pic.idr_pic_id);
        }
        // POC type 2: nothing. No redundant_pic_cnt, no B slices.
        if !pic.idr {
            w.flag(false); // num_ref_idx_active_override_flag
            w.flag(false); // ref_pic_list_modification_flag_l0
        }
        // dec_ref_pic_marking() (nal_ref_idc != 0)
        if pic.idr {
            w.flag(false); // no_output_of_prior_pics_flag
            w.flag(false); // long_term_reference_flag
        } else {
            w.flag(false); // adaptive_ref_pic_marking_mode_flag (sliding window)
        }
        if self.profile.cabac() && !pic.idr {
            w.ue(0); // cabac_init_idc
        }
        w.se(0); // slice_qp_delta
        w.ue(0); // disable_deblocking_filter_idc
        w.se(0); // slice_alpha_c0_offset_div2
        w.se(0); // slice_beta_offset_div2
        nal(3, if pic.idr { 5 } else { 1 }, &w)
    }
}

/// One picture's place in the stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PicPlan {
    pub idr: bool,
    /// `frame_num`, modulo MAX_FRAME_NUM.
    pub frame_num: u32,
    pub idr_pic_id: u32,
    /// TopFieldOrderCnt for POC type 2: twice the frames since the IDR.
    pub poc: u32,
}

impl PicPlan {
    /// slice_type: 0 = P, 2 = I (VA-API uses the same numbers).
    pub fn slice_type(&self) -> u8 {
        if self.idr {
            2
        } else {
            0
        }
    }
}

/// IDR at start and when asked, P frames referencing the previous picture
/// otherwise (no periodic IDR: the iPad asks when it needs one).
#[derive(Debug, Default)]
pub struct Gop {
    started: bool,
    since_idr: u32,
    idr_count: u32,
}

impl Gop {
    pub fn next(&mut self, force_idr: bool) -> PicPlan {
        let idr = force_idr || !self.started || self.since_idr >= (1 << 30);
        if idr {
            if self.started {
                self.idr_count = self.idr_count.wrapping_add(1);
            }
            self.started = true;
            self.since_idr = 0;
        } else {
            self.since_idr += 1;
        }
        PicPlan {
            idr,
            frame_num: self.since_idr % StreamParams::MAX_FRAME_NUM,
            // idr_pic_id is ue(v) in 0..=65535; consecutive IDRs must differ.
            idr_pic_id: self.idr_count % 65536,
            poc: 2 * self.since_idr,
        }
    }
}

/// Decides whether an iPad keyframe request needs a new IDR. The iPad asks
/// after *every* frame it couldn't show, so without this each request in a
/// burst would cost an IDR.
#[derive(Debug, Default)]
pub struct KeyframePolicy {
    /// frame_id of the newest keyframe sent.
    last_key: Option<u32>,
    /// An IDR was asked for and hasn't gone out yet.
    pending: bool,
}

impl KeyframePolicy {
    /// The iPad couldn't show `frame_id`. True = make an IDR now.
    pub fn on_request(&mut self, frame_id: u32) -> bool {
        if self.pending {
            return false;
        }
        if let Some(k) = self.last_key {
            // A keyframe after the failed frame is already on its way.
            if (k.wrapping_sub(frame_id) as i32) > 0 {
                return false;
            }
        }
        self.pending = true;
        true
    }

    /// A keyframe left the daemon (requested or not).
    pub fn on_keyframe_sent(&mut self, frame_id: u32) {
        self.last_key = Some(frame_id);
        self.pending = false;
    }
}

/// Exp-Golomb reader, for tests and for checking our own headers.
#[derive(Debug)]
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    /// `data`: RBSP (emulation-prevention bytes already removed).
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    pub fn bit(&mut self) -> Option<bool> {
        let b = self.data.get(self.pos / 8)?;
        let v = (b >> (7 - self.pos % 8)) & 1 == 1;
        self.pos += 1;
        Some(v)
    }

    pub fn u(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.bit()? as u32;
        }
        Some(v)
    }

    pub fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while !self.bit()? {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some(((1u64 << zeros) - 1 + self.u(zeros)? as u64) as u32)
    }

    pub fn se(&mut self) -> Option<i32> {
        let k = self.ue()? as i64;
        Some(if k % 2 == 1 {
            ((k + 1) / 2) as i32
        } else {
            -(k / 2) as i32
        })
    }

    pub fn bits_read(&self) -> usize {
        self.pos
    }
}

/// Strips start code, NAL header and emulation-prevention bytes.
pub fn rbsp_of(nal_with_start_code: &[u8]) -> Vec<u8> {
    let b = &nal_with_start_code[5..];
    let mut out = Vec::with_capacity(b.len());
    let mut zeros = 0;
    for &x in b {
        if zeros >= 2 && x == 3 {
            zeros = 0;
            continue;
        }
        out.push(x);
        zeros = if x == 0 { zeros + 1 } else { 0 };
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exp_golomb_matches_the_spec_table() {
        // Table 9-2: codeNum 0..8 bit strings.
        let want = [
            "1", "010", "011", "00100", "00101", "00110", "00111", "0001000", "0001001",
        ];
        for (v, s) in want.iter().enumerate() {
            let mut w = BitWriter::new();
            w.ue(v as u32);
            let got: String = (0..w.bit_len())
                .map(|i| {
                    if w.bytes()[i / 8] & (0x80 >> (i % 8)) != 0 {
                        '1'
                    } else {
                        '0'
                    }
                })
                .collect();
            assert_eq!(&got, s, "ue({v})");
        }
        // Table 9-3: se mapping.
        for (k, code) in [(0, 0), (1, 1), (-1, 2), (2, 3), (-2, 4), (3, 5)] {
            let mut a = BitWriter::new();
            a.se(k);
            let mut b = BitWriter::new();
            b.ue(code);
            assert_eq!(a.bytes(), b.bytes(), "se({k})");
        }
    }

    #[test]
    fn reader_round_trips_writer() {
        let mut w = BitWriter::new();
        let vals = [0u32, 1, 2, 7, 255, 65535, 1 << 20];
        for &v in &vals {
            w.ue(v);
            w.se(-(v as i32 / 2));
            w.u(5, v & 31);
        }
        w.trailing_bits();
        let bytes = w.bytes().to_vec();
        let mut r = BitReader::new(&bytes);
        for &v in &vals {
            assert_eq!(r.ue(), Some(v));
            assert_eq!(r.se(), Some(-(v as i32 / 2)));
            assert_eq!(r.u(5), Some(v & 31));
        }
    }

    #[test]
    fn emulation_prevention() {
        let mut w = BitWriter::new();
        for b in [0u8, 0, 1, 0, 0, 0, 0, 0, 3, 0xff] {
            w.u(8, b as u32);
        }
        let (n, bits) = nal(3, 6, &w);
        assert_eq!(
            n,
            vec![0, 0, 0, 1, 0x66, 0, 0, 3, 1, 0, 0, 3, 0, 0, 3, 0, 3, 0xff]
        );
        assert_eq!(bits, n.len() * 8);
        assert_eq!(rbsp_of(&n), w.bytes());
    }

    #[test]
    fn levels() {
        // The iPad Air 11" at 60 and 120 fps, 20 Mbit/s.
        assert_eq!(level_for(Profile::High, 2360, 1640, 60, 20_000), Some(51));
        assert_eq!(level_for(Profile::High, 2360, 1640, 120, 20_000), Some(52));
        assert_eq!(level_for(Profile::High, 1920, 1080, 30, 8_000), Some(40));
        assert_eq!(level_for(Profile::High, 1920, 1080, 60, 8_000), Some(42));
        // 13" iPad: 171x128 = 21888 MBs, x 60 = 1.31 M MB/s > 5.1's 983040.
        assert_eq!(level_for(Profile::Main, 2732, 2048, 60, 20_000), Some(52));
        assert_eq!(level_for(Profile::Main, 2732, 2048, 30, 20_000), Some(51));
        assert_eq!(level_for(Profile::Main, 2732, 2048, 24, 20_000), Some(50));
        assert_eq!(level_for(Profile::High, 16384, 16384, 60, 20_000), None);
    }

    fn ipad() -> StreamParams {
        StreamParams {
            profile: Profile::High,
            level_idc: 51,
            width: 2360,
            height: 1640,
        }
    }

    #[test]
    fn sps_fields_read_back() {
        let (n, bits) = ipad().sps();
        assert_eq!(&n[..5], &[0, 0, 0, 1, 0x67]);
        assert_eq!(bits % 8, 0);
        let rbsp = rbsp_of(&n);
        let mut r = BitReader::new(&rbsp);
        assert_eq!(r.u(8), Some(100));
        assert_eq!(r.u(8), Some(0b0000_1100)); // set4 + set5
        assert_eq!(r.u(8), Some(51));
        assert_eq!(r.ue(), Some(0)); // sps id
        assert_eq!(r.ue(), Some(1)); // 4:2:0
        assert_eq!((r.ue(), r.ue()), (Some(0), Some(0)));
        assert_eq!((r.bit(), r.bit()), (Some(false), Some(false)));
        assert_eq!(r.ue(), Some(4)); // log2_max_frame_num_minus4
        assert_eq!(r.ue(), Some(2)); // POC type
        assert_eq!(r.ue(), Some(1)); // refs
        assert_eq!(r.bit(), Some(false));
        assert_eq!(r.ue(), Some(147)); // 2368/16 - 1
        assert_eq!(r.ue(), Some(102)); // 1648/16 - 1
        assert_eq!((r.bit(), r.bit()), (Some(true), Some(true)));
        assert_eq!(r.bit(), Some(true)); // cropping
        assert_eq!(
            (r.ue(), r.ue(), r.ue(), r.ue()),
            (Some(0), Some(4), Some(0), Some(4))
        );
        assert_eq!(r.bit(), Some(true)); // VUI
    }

    #[test]
    fn slice_headers_differ_for_idr_and_p() {
        let s = ipad();
        let mut g = Gop::default();
        let idr = g.next(false);
        let p = g.next(false);
        assert!(idr.idr && !p.idr);
        assert_eq!((p.frame_num, p.poc), (1, 2));
        let (hi, bi) = s.slice_header(&idr);
        let (hp, bp) = s.slice_header(&p);
        assert_eq!(hi[4], 0x65);
        assert_eq!(hp[4], 0x61);
        assert!(bi > 40 && bp > 40 && bi != bp);
        let rbsp = rbsp_of(&hp);
        let mut r = BitReader::new(&rbsp);
        assert_eq!(r.ue(), Some(0));
        assert_eq!(r.ue(), Some(0)); // P
        assert_eq!(r.ue(), Some(0));
        assert_eq!(r.u(8), Some(1)); // frame_num
                                     // bit length = start code + header + exactly the bits written
        assert_eq!(
            bp,
            40 + {
                let mut w = BitWriter::new();
                w.ue(0);
                w.ue(0);
                w.ue(0);
                w.u(8, 1);
                w.flag(false);
                w.flag(false);
                w.flag(false);
                w.ue(0);
                w.se(0);
                w.ue(0);
                w.se(0);
                w.se(0);
                w.bit_len()
            }
        );
    }

    #[test]
    fn gop_frame_num_wraps_and_idr_ids_alternate() {
        let mut g = Gop::default();
        let first = g.next(false);
        assert!(first.idr);
        assert_eq!(first.idr_pic_id, 0);
        let mut last = first;
        for _ in 0..300 {
            last = g.next(false);
        }
        assert!(!last.idr);
        assert_eq!(last.frame_num, 300 % 256);
        assert_eq!(last.poc, 600);
        let again = g.next(true);
        assert!(again.idr);
        assert_eq!((again.frame_num, again.poc, again.idr_pic_id), (0, 0, 1));
        assert_eq!(g.next(true).idr_pic_id, 2);
    }

    #[test]
    fn keyframe_requests_are_deduplicated() {
        let mut k = KeyframePolicy::default();
        k.on_keyframe_sent(1);
        // Frames 5, 6, 7 couldn't be shown: one IDR.
        assert!(k.on_request(5));
        assert!(!k.on_request(6));
        assert!(!k.on_request(7));
        k.on_keyframe_sent(9);
        // Feedback for frames sent before the IDR still trickles in.
        assert!(!k.on_request(8));
        // A failure after the IDR needs a new one.
        assert!(k.on_request(12));
        k.on_keyframe_sent(13);
        // Frame ids wrap.
        k.on_keyframe_sent(2);
        assert!(!k.on_request(u32::MAX - 1));
        assert!(k.on_request(3));
    }
}
