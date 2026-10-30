//! Golden video vector shared with the Swift decoder test: the x264 test
//! clip (protocol/vectors/test-176x144.h264, Annex-B) converted to protocol
//! messages by the daemon. Regenerate with UPDATE_VECTORS=1.

use sidecar_protocol::{FrameDecoder, Message, VideoCodec};
use sidecard::video::annexb_to_messages;
use std::path::PathBuf;

fn vectors() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../protocol/vectors")
}

#[test]
fn x264_clip_to_protocol_frames() {
    let clip = std::fs::read(vectors().join("test-176x144.h264")).unwrap();
    let msgs = annexb_to_messages(VideoCodec::H264, 176, 144, 30000, &clip);
    let configs = msgs
        .iter()
        .filter(|m| matches!(m, Message::VideoConfig(_)))
        .count();
    let frames: Vec<_> = msgs
        .iter()
        .filter_map(|m| match m {
            Message::VideoFrame(f) => Some(f),
            _ => None,
        })
        .collect();
    assert_eq!(
        configs, 1,
        "SPS/PPS identical on the 2nd IDR: no second config"
    );
    assert_eq!(frames.len(), 8);
    assert_eq!(
        frames.iter().map(|f| f.keyframe).collect::<Vec<_>>(),
        vec![true, false, false, false, true, false, false, false]
    );
    assert!(matches!(&msgs[0], Message::VideoConfig(c) if c.parameter_sets[0][0] & 0x1f == 7));

    let mut bytes = Vec::new();
    for m in &msgs {
        m.encode(&mut bytes);
    }
    let path = vectors().join("video-176x144.frames");
    if std::env::var_os("UPDATE_VECTORS").is_some() {
        std::fs::write(&path, &bytes).unwrap();
    }
    assert_eq!(
        std::fs::read(&path).expect("run with UPDATE_VECTORS=1"),
        bytes
    );

    // And it round-trips through the frame decoder.
    let mut d = FrameDecoder::new();
    d.push(&bytes);
    let mut back = Vec::new();
    while let Some((ty, p)) = d.next_frame().unwrap() {
        back.push(Message::decode(ty, &p).unwrap());
    }
    assert_eq!(back, msgs);
}
