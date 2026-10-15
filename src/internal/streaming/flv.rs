//! The stream's encoded video handed to ffmpeg as FLV: each frame with the
//! time it is for. A bare stream (Annex B) carries no times, so ffmpeg counted
//! frames — and a frame the media engine dropped would have moved every frame
//! after it a slot earlier against the sound. With times, a dropped frame is
//! one held a slot longer, the rest where they belong.
//!
//! The frames go in as the media engine makes them (each unit behind its
//! length) after the codec's configuration record, as FLV carries them; HEVC
//! in Enhanced FLV, as `YouTube` takes it. Read with `-f flv -probesize 32
//! -analyzeduration 0`, ffmpeg starts at once (measured 0.04 s; its default
//! look-ahead waited 4.6 s).

use super::VideoCodec;

/// FLV's file header — version 1, video only — and the size of the tag
/// before the first, none.
const HEADER: [u8; 13] = [b'F', b'L', b'V', 1, 1, 0, 0, 0, 9, 0, 0, 0, 0];

/// A video tag's type.
const VIDEO: u8 = 9;

/// Writes one stream's frames as FLV.
#[derive(Debug)]
pub struct FlvWriter {
    codec: VideoCodec,
    started: bool,
    /// The configuration record last written: a new one is written before
    /// the first frame and whenever it changes.
    config: Option<Vec<u8>>,
}

impl FlvWriter {
    pub fn new(codec: VideoCodec) -> Self {
        Self {
            codec,
            started: false,
            config: None,
        }
    }

    /// The FLV for one frame, at `ms` from the start: the file's header
    /// before the first, the configuration record `config` before any frame
    /// it changed for. `units` are the frame's units behind their lengths, as
    /// the record says. Nothing for a frame before any record: a decoder
    /// could not read it.
    pub fn frame(&mut self, ms: u32, key: bool, units: &[u8], config: Option<&[u8]>) -> Vec<u8> {
        let mut out = Vec::with_capacity(units.len() + 64);
        if let Some(config) = config.filter(|c| self.config.as_deref() != Some(*c)) {
            self.start(&mut out);
            self.tag(&mut out, ms, true, true, config);
            self.config = Some(config.to_vec());
        }
        if self.config.is_some() {
            self.tag(&mut out, ms, key, false, units);
        }
        out
    }

    fn start(&mut self, out: &mut Vec<u8>) {
        if !self.started {
            out.extend_from_slice(&HEADER);
            self.started = true;
        }
    }

    /// One video tag: the configuration record (`header`) or a frame.
    fn tag(&self, out: &mut Vec<u8>, ms: u32, key: bool, header: bool, payload: &[u8]) {
        let frame_type: u8 = if key { 1 } else { 2 };
        let mut body = Vec::with_capacity(payload.len() + 8);
        match self.codec {
            VideoCodec::H264 => {
                // Frame type and AVC; a sequence header or units; no
                // composition offset, there being no B-frames.
                body.push(frame_type << 4 | 7);
                body.push(u8::from(!header));
                body.extend_from_slice(&[0, 0, 0]);
            }
            VideoCodec::Hevc => {
                // Enhanced FLV: the extended header, frame type, sequence
                // start or frames with no composition offset, and the codec.
                let packet: u8 = if header { 0 } else { 3 };
                body.push(0x80 | frame_type << 4 | packet);
                body.extend_from_slice(b"hvc1");
            }
        }
        body.extend_from_slice(payload);
        let size = u32::try_from(body.len()).unwrap_or(u32::MAX) & 0x00FF_FFFF;
        out.push(VIDEO);
        out.extend_from_slice(&size.to_be_bytes()[1..]);
        out.extend_from_slice(&(ms & 0x00FF_FFFF).to_be_bytes()[1..]);
        out.push((ms >> 24) as u8);
        out.extend_from_slice(&[0, 0, 0]);
        out.extend_from_slice(&body);
        out.extend_from_slice(&(size + 11).to_be_bytes());
    }
}

/// When the `slot`th frame at `fps` is, in whole milliseconds.
pub fn slot_ms(slot: u64, fps: u32) -> u32 {
    let ms = (u128::from(slot) * 1000 + u128::from(fps.max(1)) / 2) / u128::from(fps.max(1));
    u32::try_from(ms).unwrap_or(u32::MAX)
}

/// A frame's `units` (each behind its four-byte length) without the filler
/// ones — padding the media engine adds to hold a constant rate, NAL type
/// 12 in H.264 and 38 in HEVC — so the stream is the same picture at the
/// rate it needs (see `RateMode::Variable`). The engine declares no buffer
/// model the padding is part of (checked: its parameter sets carry no
/// timing or HRD), so nothing it says is made untrue. The units as they are
/// where there is none, or where they do not read whole.
pub fn without_filler(units: &[u8], codec: VideoCodec) -> std::borrow::Cow<'_, [u8]> {
    let filler = |unit: &[u8]| match codec {
        VideoCodec::H264 => unit.first().is_some_and(|b| b & 0x1f == 12),
        VideoCodec::Hevc => unit.first().is_some_and(|b| (b >> 1) & 0x3f == 38),
    };
    let mut kept = Vec::with_capacity(units.len());
    let mut at = 0;
    let mut dropped = false;
    while at < units.len() {
        let Some(len) = units
            .get(at..at + 4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
        else {
            return std::borrow::Cow::Borrowed(units);
        };
        let Some(unit) = units.get(at + 4..at + 4 + len) else {
            return std::borrow::Cow::Borrowed(units);
        };
        if filler(unit) {
            dropped = true;
        } else {
            kept.extend_from_slice(&units[at..at + 4 + len]);
        }
        at += 4 + len;
    }
    if dropped {
        std::borrow::Cow::Owned(kept)
    } else {
        std::borrow::Cow::Borrowed(units)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(bytes: &[u8]) -> Vec<u8> {
        let mut out = (bytes.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(bytes);
        out
    }

    /// The media engine's padding goes, the picture stays: H.264's filler
    /// (12) and HEVC's (38) dropped, every other unit kept as it was; units
    /// that do not read whole are left alone.
    #[test]
    fn padding_is_dropped_and_the_picture_kept() {
        let idr = unit(&[0x65, 1, 2, 3]);
        let filler = unit(&[0x0c, 0xff, 0xff, 0xff, 0x80]);
        let sei = unit(&[0x06, 5, 0]);
        let h264 = [sei.clone(), idr.clone(), filler.clone()].concat();
        assert_eq!(&*without_filler(&h264, VideoCodec::H264), [sei, idr.clone()].concat().as_slice());
        assert!(matches!(without_filler(&idr, VideoCodec::H264), std::borrow::Cow::Borrowed(_)));

        let hevc_slice = unit(&[0x26, 0x01, 9]);
        let hevc_filler = unit(&[0x4c, 0x01, 0xff, 0xff]);
        let hevc = [hevc_slice.clone(), hevc_filler].concat();
        assert_eq!(&*without_filler(&hevc, VideoCodec::Hevc), hevc_slice.as_slice());
        // H.264 filler read as HEVC is a slice, and kept.
        assert_eq!(&*without_filler(&filler, VideoCodec::Hevc), filler.as_slice());

        let cut = [idr.clone(), vec![0, 0, 0, 9, 1]].concat();
        assert_eq!(&*without_filler(&cut, VideoCodec::H264), cut.as_slice(), "not read whole: as it is");
    }

    /// The tags in an FLV file after its header: (type, time, body).
    fn tags(flv: &[u8]) -> Vec<(u8, u32, Vec<u8>)> {
        assert_eq!(&flv[..13], &HEADER);
        let mut at = 13;
        let mut out = Vec::new();
        while at < flv.len() {
            let kind = flv[at];
            let size = u32::from_be_bytes([0, flv[at + 1], flv[at + 2], flv[at + 3]]) as usize;
            let ms = u32::from_be_bytes([flv[at + 7], flv[at + 4], flv[at + 5], flv[at + 6]]);
            let body = flv[at + 11..at + 11 + size].to_vec();
            let back = u32::from_be_bytes(flv[at + 11 + size..at + 15 + size].try_into().unwrap());
            assert_eq!(back as usize, size + 11, "each tag followed by its size");
            out.push((kind, ms, body));
            at += 15 + size;
        }
        out
    }

    /// The first frame brings the file's header and the configuration
    /// record; the frames after, only themselves; a new record, itself again.
    #[test]
    fn h264_frames_follow_their_configuration_record() {
        let mut flv = FlvWriter::new(VideoCodec::H264);
        let mut file = flv.frame(0, true, &[0, 0, 0, 1, 0x65], Some(&[1, 0x64]));
        file.extend(flv.frame(20, false, &[0, 0, 0, 1, 0x41], Some(&[1, 0x64])));
        file.extend(flv.frame(40, true, &[0, 0, 0, 1, 0x65], Some(&[1, 0x4d])));
        let tags = tags(&file);
        let bodies: Vec<(u32, &[u8])> = tags.iter().map(|(_, ms, b)| (*ms, b.as_slice())).collect();
        assert_eq!(
            bodies,
            [
                (0, &[0x17, 0, 0, 0, 0, 1, 0x64][..]),
                (0, &[0x17, 1, 0, 0, 0, 0, 0, 0, 1, 0x65][..]),
                (20, &[0x27, 1, 0, 0, 0, 0, 0, 0, 1, 0x41][..]),
                (40, &[0x17, 0, 0, 0, 0, 1, 0x4d][..]),
                (40, &[0x17, 1, 0, 0, 0, 0, 0, 0, 1, 0x65][..]),
            ]
        );
        assert!(tags.iter().all(|(kind, ..)| *kind == VIDEO));
    }

    /// HEVC goes in Enhanced FLV: the extended header and the codec's name.
    #[test]
    fn hevc_frames_go_in_enhanced_flv() {
        let mut flv = FlvWriter::new(VideoCodec::Hevc);
        let file = flv.frame(1000, true, &[0, 0, 0, 1, 0x26], Some(&[1, 2, 3]));
        let tags = tags(&file);
        assert_eq!(tags[0].2, [0x90, b'h', b'v', b'c', b'1', 1, 2, 3]);
        assert_eq!(tags[1].2, [0x93, b'h', b'v', b'c', b'1', 0, 0, 0, 1, 0x26]);
        assert_eq!(tags[1].1, 1000);
    }

    /// A frame before any configuration record is not written: nothing
    /// could decode it.
    #[test]
    fn nothing_before_the_configuration_record() {
        let mut flv = FlvWriter::new(VideoCodec::H264);
        assert!(flv.frame(0, false, &[0, 0, 0, 1, 0x41], None).is_empty());
        assert!(!flv.frame(20, true, &[0, 0, 0, 1, 0x65], Some(&[1])).is_empty());
    }

    /// Times past 2^24 ms use the extended byte.
    #[test]
    fn long_streams_keep_their_times() {
        let mut flv = FlvWriter::new(VideoCodec::H264);
        let ms = 0x0123_4567;
        let tags = tags(&flv.frame(ms, true, &[0, 0, 0, 1, 0x65], Some(&[1])));
        assert_eq!(tags[1].1, ms);
    }

    /// Slots fall at whole milliseconds, rounded: 60 a second is 0, 17, 33, 50.
    #[test]
    fn slots_are_rounded_to_milliseconds() {
        let at: Vec<u32> = (0..4).map(|s| slot_ms(s, 60)).collect();
        assert_eq!(at, [0, 17, 33, 50]);
        assert_eq!(slot_ms(50, 50), 1000);
    }
}
