//! Just enough MPEG-TS for a destination that fell too far behind to come
//! back cleanly. Thrown away piece by piece, its stream broke up into gray
//! blocks until the next keyframe; skipping instead to where a keyframe
//! starts, after the tables that say what the stream holds, the picture is
//! whole again at once — a jump, not damage.
//!
//! The encoder's MPEG-TS is ffmpeg's: one program, its tables in single
//! packets, a keyframe's first packet flagged as a random access point.

/// A transport stream packet's size.
pub const PACKET: usize = 188;

/// What the stream's tables say, and the last of each, to send again ahead
/// of a keyframe a destination resumes at.
#[derive(Debug, Default)]
pub struct TsScan {
    /// The program association table's last packet.
    pat: Option<Vec<u8>>,
    /// The program map table's last packet, and the PID it comes on.
    pmt: Option<Vec<u8>>,
    pmt_pid: Option<u16>,
    /// The PID the video comes on.
    video_pid: Option<u16>,
    /// Every stream's PID — the picture's and the sound's.
    streams: Vec<u16>,
}

/// A packet's PID.
fn pid(packet: &[u8]) -> u16 {
    u16::from(packet[1] & 0x1F) << 8 | u16::from(packet[2])
}

/// Whether a packet starts a payload unit (a table, a frame).
fn starts_unit(packet: &[u8]) -> bool {
    packet[1] & 0x40 != 0
}

/// The packet's payload, after any adaptation field.
fn payload(packet: &[u8]) -> Option<&[u8]> {
    let control = packet[3] >> 4 & 3;
    match control {
        1 => Some(&packet[4..]),
        3 => {
            let skip = 5 + usize::from(packet[4]);
            packet.get(skip..)
        }
        _ => None,
    }
}

/// Whether the packet is flagged a random access point: where a keyframe
/// starts.
fn random_access(packet: &[u8]) -> bool {
    let has_field = packet[3] >> 4 & 2 != 0;
    has_field && packet[4] > 0 && packet[5] & 0x40 != 0
}

/// A table section's body, from a packet that starts it: after the pointer
/// field, `table_id` checked, up to its stated length.
fn section(packet: &[u8], table_id: u8) -> Option<&[u8]> {
    if !starts_unit(packet) {
        return None;
    }
    let data = payload(packet)?;
    let start = 1 + usize::from(*data.first()?);
    let section = data.get(start..)?;
    if *section.first()? != table_id {
        return None;
    }
    let length = usize::from(section.get(1)? & 0x0F) << 8 | usize::from(*section.get(2)?);
    section.get(..3 + length)
}

impl TsScan {
    /// Read a piece of whole packets: remember its tables, and say where the
    /// first packet of a video keyframe starts in it, in bytes.
    pub fn scan(&mut self, piece: &[u8]) -> Option<usize> {
        let mut keyframe = None;
        for (index, packet) in piece.as_chunks::<PACKET>().0.iter().enumerate() {
            if packet[0] != 0x47 {
                continue;
            }
            let pid = pid(packet);
            if pid == 0 {
                self.read_pat(packet);
            } else if Some(pid) == self.pmt_pid {
                self.read_pmt(packet);
            } else if keyframe.is_none()
                && Some(pid) == self.video_pid
                && starts_unit(packet)
                && random_access(packet)
            {
                keyframe = Some(index * PACKET);
            }
        }
        keyframe
    }

    /// Every stream's PID, as the tables say.
    pub fn streams(&self) -> &[u16] {
        &self.streams
    }

    /// The tables to send ahead of a keyframe a destination resumes at:
    /// without them a reader joining there could not place it. `None` until
    /// both have been seen.
    pub fn tables(&self) -> Option<Vec<u8>> {
        let (pat, pmt) = (self.pat.as_ref()?, self.pmt.as_ref()?);
        Some([pat.as_slice(), pmt.as_slice()].concat())
    }

    fn read_pat(&mut self, packet: &[u8]) {
        let Some(section) = section(packet, 0x00) else {
            return;
        };
        // Programs follow the 8-byte header, 4 bytes each, before the CRC.
        let programs = section.get(8..section.len().saturating_sub(4)).unwrap_or(&[]);
        for entry in programs.as_chunks::<4>().0 {
            let number = u16::from_be_bytes([entry[0], entry[1]]);
            if number != 0 {
                self.pmt_pid = Some(u16::from(entry[2] & 0x1F) << 8 | u16::from(entry[3]));
                self.pat = Some(packet.to_vec());
                return;
            }
        }
    }

    fn read_pmt(&mut self, packet: &[u8]) {
        let Some(section) = section(packet, 0x02) else {
            return;
        };
        let Some(info) = section.get(10..12) else {
            return;
        };
        let info_len = usize::from(info[0] & 0x0F) << 8 | usize::from(info[1]);
        let end = section.len().saturating_sub(4);
        let mut at = 12 + info_len;
        let (mut video, mut streams) = (None, Vec::new());
        while at + 5 <= end {
            let kind = section[at];
            let es_pid = u16::from(section[at + 1] & 0x1F) << 8 | u16::from(section[at + 2]);
            let es_info = usize::from(section[at + 3] & 0x0F) << 8 | usize::from(section[at + 4]);
            streams.push(es_pid);
            // H.264, HEVC, MPEG-2 video.
            if video.is_none() && matches!(kind, 0x1B | 0x24 | 0x02) {
                video = Some(es_pid);
            }
            at += 5 + es_info;
        }
        if video.is_some() {
            self.video_pid = video;
            self.streams = streams;
            self.pmt = Some(packet.to_vec());
        }
    }
}

/// The packets of `piece` with each PID's continuity count carried on from
/// the last a destination got (`last`, kept up to date): a copy where any
/// changes, `None` where none does. After a skip each PID's count jumps, and
/// a reader takes the packet before the jump for broken.
pub fn renumber(piece: &[u8], last: &mut std::collections::HashMap<u16, u8>) -> Option<Vec<u8>> {
    let mut copy: Option<Vec<u8>> = None;
    for (index, packet) in piece.as_chunks::<PACKET>().0.iter().enumerate() {
        let pid = pid(packet);
        // Null packets count nothing.
        if packet[0] != 0x47 || pid == 0x1FFF {
            continue;
        }
        let count = packet[3] & 0x0F;
        let has_payload = packet[3] & 0x10 != 0;
        let want = match last.get(&pid) {
            Some(&before) if has_payload => (before + 1) & 0x0F,
            Some(&before) => before,
            None => count,
        };
        if want != count {
            let bytes = copy.get_or_insert_with(|| piece.to_vec());
            bytes[index * PACKET + 3] = packet[3] & 0xF0 | want;
        }
        last.insert(pid, want);
    }
    copy
}

/// The packets of `piece` that finish a unit under way — a frame, a piece of
/// sound — on each PID still `open`: those up to its next unit's start,
/// which closes it. What a destination falling too far behind still sends,
/// so it does not stop halfway through a frame: cut off there, the frame
/// came out broken.
pub fn finish_units(piece: &[u8], open: &mut Vec<u16>) -> Vec<u8> {
    let mut kept = Vec::new();
    for packet in piece.as_chunks::<PACKET>().0 {
        if packet[0] != 0x47 {
            continue;
        }
        let pid = pid(packet);
        let Some(at) = open.iter().position(|&p| p == pid) else {
            continue;
        };
        if starts_unit(packet) {
            open.swap_remove(at);
        } else {
            kept.extend_from_slice(packet);
        }
    }
    kept
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A packet on `pid` carrying `body`, starting a unit or not, with an
    /// adaptation field flagged a random access point when `key`.
    pub(crate) fn packet(pid: u16, start: bool, key: bool, body: &[u8]) -> Vec<u8> {
        let mut p = vec![0x47, (u8::from(start) << 6) | (pid >> 8) as u8, pid as u8];
        if key {
            p.push(0x30);
            p.extend_from_slice(&[1, 0x40]);
        } else {
            p.push(0x10);
        }
        p.extend_from_slice(body);
        p.resize(PACKET, 0xFF);
        p
    }

    /// A PAT naming program 1 on `pmt_pid`.
    pub(crate) fn pat(pmt_pid: u16) -> Vec<u8> {
        let mut body = vec![0, 0x00, 0xB0, 13, 0, 1, 0xC1, 0, 0, 0, 1];
        body.extend_from_slice(&[0xE0 | (pmt_pid >> 8) as u8, pmt_pid as u8]);
        body.extend_from_slice(&[0, 0, 0, 0]);
        packet(0, true, false, &body)
    }

    /// A PMT with audio on `audio` and video (H.264) on `video`.
    pub(crate) fn pmt(pmt_pid: u16, audio: u16, video: u16) -> Vec<u8> {
        let mut body = vec![0, 0x02, 0xB0, 23, 0, 1, 0xC1, 0, 0, 0xE1, 0x00, 0xF0, 0];
        body.extend_from_slice(&[0x0F, 0xE0 | (audio >> 8) as u8, audio as u8, 0xF0, 0]);
        body.extend_from_slice(&[0x1B, 0xE0 | (video >> 8) as u8, video as u8, 0xF0, 0]);
        body.extend_from_slice(&[0, 0, 0, 0]);
        packet(pmt_pid, true, false, &body)
    }

    /// The tables found, a keyframe on the video PID is where the piece
    /// resumes; one on the audio PID, or a frame not flagged, is not.
    #[test]
    fn a_keyframe_is_found_after_the_tables() {
        let mut scan = TsScan::default();
        let first = [pat(0x1000), pmt(0x1000, 0x101, 0x100)].concat();
        assert_eq!(scan.scan(&first), None);
        let piece = [
            packet(0x101, true, true, &[1]),
            packet(0x100, true, false, &[2]),
            packet(0x100, false, false, &[3]),
            packet(0x100, true, true, &[4]),
        ]
        .concat();
        assert_eq!(scan.scan(&piece), Some(3 * PACKET));
        assert_eq!(scan.tables(), Some(first));
    }

    /// What finishes the units under way goes; each PID's next start
    /// closes it, and nothing of it after.
    #[test]
    fn the_units_under_way_are_finished() {
        let mut open = vec![0x100, 0x101];
        let piece = [
            packet(0x100, false, false, &[1]),
            packet(0x101, true, true, &[2]),
            packet(0x101, false, false, &[3]),
            packet(0x100, false, false, &[4]),
            packet(0x100, true, false, &[5]),
            packet(0x100, false, false, &[6]),
        ]
        .concat();
        let kept = finish_units(&piece, &mut open);
        assert_eq!(kept, [packet(0x100, false, false, &[1]), packet(0x100, false, false, &[4])].concat());
        assert!(open.is_empty(), "both closed");
    }

    /// Counts carry on per PID from what was sent last, through a jump; an
    /// adaptation-only packet repeats its PID's count; a PID first seen
    /// keeps its own.
    #[test]
    fn counts_carry_on_through_a_jump() {
        let with_count = |mut p: Vec<u8>, count: u8| {
            p[3] = p[3] & 0xF0 | count;
            p
        };
        let mut last = std::collections::HashMap::from([(0x100, 2)]);
        let mut adaptation_only = packet(0x100, false, false, &[]);
        adaptation_only[3] = 0x20 | 9;
        let piece = [
            with_count(packet(0x100, false, false, &[1]), 9),
            with_count(packet(0x100, false, false, &[2]), 10),
            adaptation_only,
            with_count(packet(0x101, true, false, &[3]), 7),
        ]
        .concat();
        let out = renumber(&piece, &mut last).expect("changed");
        let counts: Vec<u8> = out.chunks(PACKET).map(|p| p[3] & 0x0F).collect();
        assert_eq!(counts, [3, 4, 4, 7]);
        assert_eq!(last.get(&0x100), Some(&4));
        // Carried on as they come, nothing changes.
        let next = [with_count(packet(0x100, false, false, &[4]), 5)].concat();
        assert_eq!(renumber(&next, &mut last), None);
    }

    /// The tables say which PIDs carry the streams.
    #[test]
    fn the_streams_are_read_from_the_tables() {
        let mut scan = TsScan::default();
        scan.scan(&[pat(0x1000), pmt(0x1000, 0x101, 0x100)].concat());
        assert_eq!(scan.streams(), [0x101, 0x100]);
    }

    /// Before the tables, nothing is known to be video.
    #[test]
    fn nothing_before_the_tables() {
        let mut scan = TsScan::default();
        assert_eq!(scan.scan(&packet(0x100, true, true, &[1])), None);
        assert_eq!(scan.tables(), None);
    }
}
