//! Cross-checks our H.264 headers with FFmpeg's `trace_headers` bitstream
//! filter, an independent H.264 syntax parser. Needs `ffmpeg` on PATH; CI
//! sets SIDECAR_REQUIRE_FFMPEG=1 so a missing ffmpeg fails instead of
//! skipping.

use sidecard::native::h264::{Gop, Profile, StreamParams};
use std::process::Command;

/// A slice header followed by stand-in slice data: alignment bits (1s, as
/// CABAC requires) and a few bytes FFmpeg's header parser doesn't look at.
fn slice(header: (Vec<u8>, usize)) -> Vec<u8> {
    let (mut b, bits) = header;
    if bits % 8 != 0 {
        let last = b.last_mut().unwrap();
        *last |= 0xff >> (bits % 8);
    }
    b.extend_from_slice(&[0x55, 0xaa, 0x55, 0x80]);
    b
}

fn stream(profile: Profile) -> Vec<u8> {
    let s = StreamParams {
        profile,
        level_idc: 51,
        width: 2360,
        height: 1640,
    };
    let mut g = Gop::default();
    let mut out = Vec::new();
    out.extend(s.sps().0);
    out.extend(s.pps().0);
    out.extend(slice(s.slice_header(&g.next(false))));
    out.extend(slice(s.slice_header(&g.next(false))));
    out.extend(slice(s.slice_header(&g.next(false))));
    // A requested IDR.
    out.extend(s.sps().0);
    out.extend(s.pps().0);
    out.extend(slice(s.slice_header(&g.next(true))));
    out
}

fn trace(data: &[u8]) -> Option<String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.h264");
    std::fs::write(&path, data).unwrap();
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "debug", "-f", "h264", "-i"])
        .arg(&path)
        .args(["-c", "copy", "-bsf:v", "trace_headers", "-f", "null", "-"])
        .output();
    match out {
        Ok(o) => Some(String::from_utf8_lossy(&o.stderr).into_owned()),
        Err(e) => {
            assert!(
                std::env::var_os("SIDECAR_REQUIRE_FFMPEG").is_none(),
                "ffmpeg required but not runnable: {e}"
            );
            eprintln!("ffmpeg not found, skipping ({e})");
            None
        }
    }
}

/// Every value FFmpeg traced for `field`, in order.
fn values(log: &str, field: &str) -> Vec<i64> {
    log.lines()
        .filter_map(|l| {
            let l = l.split("] ").last()?;
            let mut it = l.split_whitespace();
            it.next()?; // bit position
            (it.next()? == field).then(|| it.last()?.trim_start_matches('=').parse().ok())?
        })
        .collect()
}

#[test]
fn ffmpeg_parses_our_headers() {
    for profile in [Profile::High, Profile::Main, Profile::ConstrainedBaseline] {
        let Some(log) = trace(&stream(profile)) else {
            return;
        };
        let bad: Vec<&str> = log
            .lines()
            .filter(|l| {
                let l = l.to_ascii_lowercase();
                // The decoder probing the stand-in slice data complains about
                // macroblocks; that is expected and not about the headers.
                (l.contains("invalid") || l.contains("error") || l.contains("failed"))
                    && !l.contains("error while decoding mb")
                    && !l.contains("concealing")
            })
            .collect();
        assert!(
            bad.is_empty(),
            "{profile:?}: FFmpeg complained:\n{}",
            bad.join("\n")
        );
        // trace_headers prints the parameter sets once per packet that has
        // them, plus once from the extradata: compare the distinct values.
        let one = |field: &str| {
            let mut v = values(&log, field);
            v.dedup();
            v
        };
        assert_eq!(one("profile_idc"), vec![profile.idc() as i64], "{log}");
        assert_eq!(one("pic_width_in_mbs_minus1"), vec![147]);
        assert_eq!(one("pic_height_in_map_units_minus1"), vec![102]);
        assert_eq!(one("frame_crop_right_offset"), vec![4]);
        assert_eq!(one("frame_crop_bottom_offset"), vec![4]);
        assert_eq!(one("pic_order_cnt_type"), vec![2]);
        assert_eq!(one("max_num_reorder_frames"), vec![0]);
        assert_eq!(one("max_dec_frame_buffering"), vec![1]);
        assert_eq!(one("matrix_coefficients"), vec![1]);
        assert_eq!(one("video_full_range_flag"), vec![0]);
        assert_eq!(
            one("entropy_coding_mode_flag"),
            vec![profile.cabac() as i64]
        );
        assert!(
            log.contains("2360x1640"),
            "FFmpeg should see the cropped size:\n{log}"
        );
        // IDR, P, P, IDR: slice types and frame numbers as scheduled.
        assert_eq!(values(&log, "slice_type"), vec![2, 0, 0, 2], "{log}");
        assert_eq!(values(&log, "frame_num"), vec![0, 1, 2, 0]);
        assert_eq!(values(&log, "idr_pic_id"), vec![0, 1]);
        assert_eq!(values(&log, "slice_qp_delta"), vec![0, 0, 0, 0]);
    }
}
