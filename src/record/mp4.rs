//! An mp4 view of one camera's `CompressedVideo` frames, so a browser's
//! `<video>` plays a take straight off the headset with nothing decoded here.
//!
//! The file is never written. Its header is built from the frame index, and
//! every byte range is cut from the mcap on request, which is what lets
//! `<video>` seek with HTTP ranges. WebCodecs would have spared the muxing, but
//! browsers offer it to secure contexts only, and the page is plain `http://`
//! on a LAN address.
//!
//! Frame times stay variable: each sample lasts until the next frame's stamp,
//! so a dropped frame is a longer sample rather than a shifted timeline.

/// The mp4 sample clock: 90 kHz, the usual video timescale, so a 30 fps
/// interval is a whole 3000 ticks.
const TIMESCALE: u64 = 90_000;

const SPS: u8 = 7;
const PPS: u8 = 8;
const AUD: u8 = 9;
const IDR: u8 = 5;

/// One encoded frame, as the header needs it.
pub struct Sample {
    pub t_ns: u64,
    /// Bytes in the mp4, once the start codes are lengths and the parameter
    /// sets are out: what [`avcc`] makes of the frame.
    pub size: u32,
    pub key: bool,
}

/// The NAL units of an Annex B access unit, without their start codes.
pub fn nal_units(annex_b: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= annex_b.len() {
        if annex_b[i] == 0 && annex_b[i + 1] == 0 && annex_b[i + 2] == 1 {
            starts.push((i, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut units = Vec::with_capacity(starts.len());
    for (k, &(_, body)) in starts.iter().enumerate() {
        let mut end = starts.get(k + 1).map_or(annex_b.len(), |&(code, _)| code);
        // The zero before a 4-byte start code belongs to the code, not to the
        // unit before it.
        while end > body && annex_b[end - 1] == 0 && k + 1 < starts.len() {
            end -= 1;
        }
        if end > body {
            units.push(&annex_b[body..end]);
        }
    }
    units
}

fn nal_type(unit: &[u8]) -> u8 {
    unit[0] & 0x1f
}

/// One frame as an mp4 sample: each unit prefixed with its 4-byte length, and
/// the parameter sets and delimiters dropped, because `avcC` carries the
/// parameter sets once for the whole track.
pub fn avcc(annex_b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(annex_b.len());
    for unit in nal_units(annex_b) {
        if matches!(nal_type(unit), SPS | PPS | AUD) {
            continue;
        }
        out.extend_from_slice(&(unit.len() as u32).to_be_bytes());
        out.extend_from_slice(unit);
    }
    out
}

pub fn is_key(annex_b: &[u8]) -> bool {
    nal_units(annex_b).iter().any(|u| nal_type(u) == IDR)
}

/// The first SPS and PPS in a frame, which every key frame carries (see
/// `video::with_parameter_sets`).
pub fn parameter_sets(annex_b: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let units = nal_units(annex_b);
    let sps = units.iter().find(|u| nal_type(u) == SPS)?;
    let pps = units.iter().find(|u| nal_type(u) == PPS)?;
    Some((sps.to_vec(), pps.to_vec()))
}

/// Exp-Golomb reads over an SPS with its emulation-prevention bytes removed.
struct Bits {
    data: Vec<u8>,
    pos: usize,
}

impl Bits {
    fn new(unit: &[u8]) -> Bits {
        let mut data = Vec::with_capacity(unit.len());
        let mut zeros = 0;
        for &b in unit {
            if zeros >= 2 && b == 3 {
                zeros = 0;
                continue;
            }
            zeros = if b == 0 { zeros + 1 } else { 0 };
            data.push(b);
        }
        Bits { data, pos: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.pos / 8)?;
        let bit = (byte >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        Some(bit as u32)
    }

    fn bits(&mut self, n: u32) -> Option<u32> {
        (0..n).try_fold(0, |acc, _| Some(acc << 1 | self.bit()?))
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        Some((1u32 << zeros) - 1 + self.bits(zeros)?)
    }

    fn se(&mut self) -> Option<i32> {
        let k = self.ue()? as i64;
        Some(if k % 2 == 1 { (k + 1) / 2 } else { -(k / 2) } as i32)
    }
}

/// The picture size an SPS declares, after cropping (H.264 §7.3.2.1.1).
pub fn dimensions(sps: &[u8]) -> Option<(u32, u32)> {
    let mut b = Bits::new(sps);
    b.bits(8)?;
    let profile = b.bits(8)?;
    b.bits(16)?;
    b.ue()?;
    let mut chroma = 1;
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        chroma = b.ue()?;
        if chroma == 3 {
            b.bit()?;
        }
        b.ue()?;
        b.ue()?;
        b.bit()?;
        if b.bit()? == 1 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if b.bit()? == 1 {
                    let size = if i < 6 { 16 } else { 64 };
                    let (mut last, mut next) = (8i32, 8i32);
                    for _ in 0..size {
                        if next != 0 {
                            next = (last + b.se()? + 256) % 256;
                        }
                        if next != 0 {
                            last = next;
                        }
                    }
                }
            }
        }
    }
    b.ue()?;
    match b.ue()? {
        0 => {
            b.ue()?;
        }
        1 => {
            b.bit()?;
            b.se()?;
            b.se()?;
            for _ in 0..b.ue()? {
                b.se()?;
            }
        }
        _ => {}
    }
    b.ue()?;
    b.bit()?;
    let width_mbs = b.ue()? + 1;
    let height_units = b.ue()? + 1;
    let frame_mbs_only = b.bit()?;
    if frame_mbs_only == 0 {
        b.bit()?;
    }
    b.bit()?;
    let (mut left, mut right, mut top, mut bottom) = (0, 0, 0, 0);
    if b.bit()? == 1 {
        (left, right, top, bottom) = (b.ue()?, b.ue()?, b.ue()?, b.ue()?);
    }
    let (sub_w, sub_h) = match chroma {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    let crop_x = if chroma == 0 { 1 } else { sub_w };
    let crop_y = (2 - frame_mbs_only) * if chroma == 0 { 1 } else { sub_h };
    let width = (width_mbs * 16).checked_sub(crop_x * (left + right))?;
    let height = ((2 - frame_mbs_only) * height_units * 16).checked_sub(crop_y * (top + bottom))?;
    Some((width, height))
}

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    out.extend_from_slice(&(body.len() as u32 + 8).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out
}

fn full(kind: &[u8; 4], version: u8, flags: u32, body: &[u8]) -> Vec<u8> {
    let mut head = (flags & 0xff_ffff).to_be_bytes();
    head[0] = version;
    bx(kind, &[&head[..], body].concat())
}

/// The identity transform every `tkhd` and `mvhd` carries.
const MATRIX: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];

fn be32(values: &[u32]) -> Vec<u8> {
    values.iter().flat_map(|v| v.to_be_bytes()).collect()
}

/// Everything before the first sample's first byte: `ftyp`, `moov` and the
/// `mdat` header. The samples follow it back to back, in order.
pub fn header(samples: &[Sample], sps: &[u8], pps: &[u8]) -> Option<Vec<u8>> {
    let first = samples.first()?;
    let (width, height) = dimensions(sps)?;
    let ticks: Vec<u64> = samples
        .iter()
        .map(|s| s.t_ns.saturating_sub(first.t_ns) * TIMESCALE / 1_000_000_000)
        .collect();
    let mut deltas: Vec<u32> =
        ticks.windows(2).map(|w| w[1].saturating_sub(w[0]).max(1) as u32).collect();
    // The last frame has no successor to end it, so it lasts as long as the one
    // before; a single frame lasts one 30 fps interval.
    deltas.push(deltas.last().copied().unwrap_or(3000));
    let duration: u64 = deltas.iter().map(|&d| d as u64).sum();
    let millis = (duration * 1000 / TIMESCALE) as u32;

    let mut stts = Vec::new();
    for &d in &deltas {
        match stts.last_mut() {
            Some((count, delta)) if *delta == d => *count += 1,
            _ => stts.push((1u32, d)),
        }
    }
    let stts: Vec<u32> = std::iter::once(stts.len() as u32)
        .chain(stts.into_iter().flat_map(|(c, d)| [c, d]))
        .collect();
    let keys: Vec<u32> = (1..).zip(samples).filter(|(_, s)| s.key).map(|(n, _)| n).collect();
    let stss: Vec<u32> = std::iter::once(keys.len() as u32).chain(keys).collect();
    let n = samples.len() as u32;
    let sizes: Vec<u32> = [0, n].into_iter().chain(samples.iter().map(|s| s.size)).collect();
    let data: u64 = samples.iter().map(|s| s.size as u64).sum();

    let avcc_box = bx(
        b"avcC",
        &[
            &[1, sps[1], sps[2], sps[3], 0xff, 0xe1][..],
            &(sps.len() as u16).to_be_bytes(),
            sps,
            &[1],
            &(pps.len() as u16).to_be_bytes(),
            pps,
        ]
        .concat(),
    );
    let mut avc1 = vec![0u8; 6];
    avc1.extend_from_slice(&1u16.to_be_bytes());
    avc1.extend_from_slice(&[0u8; 16]);
    avc1.extend_from_slice(&(width as u16).to_be_bytes());
    avc1.extend_from_slice(&(height as u16).to_be_bytes());
    avc1.extend_from_slice(&be32(&[0x0048_0000, 0x0048_0000, 0]));
    avc1.extend_from_slice(&1u16.to_be_bytes());
    avc1.extend_from_slice(&[0u8; 32]);
    avc1.extend_from_slice(&[0x00, 0x18, 0xff, 0xff]);
    avc1.extend_from_slice(&avcc_box);
    let stsd = full(b"stsd", 0, 0, &[&1u32.to_be_bytes()[..], &bx(b"avc1", &avc1)].concat());

    // Built twice: the chunk offset is the header's own length, which the
    // first pass measures with a placeholder of the same width.
    let build = |data_at: u64| -> Vec<u8> {
        let stbl = bx(
            b"stbl",
            &[
                stsd.clone(),
                full(b"stts", 0, 0, &be32(&stts)),
                full(b"stss", 0, 0, &be32(&stss)),
                full(b"stsc", 0, 0, &be32(&[1, 1, n, 1])),
                full(b"stsz", 0, 0, &be32(&sizes)),
                full(b"co64", 0, 0, &[&1u32.to_be_bytes()[..], &data_at.to_be_bytes()].concat()),
            ]
            .concat(),
        );
        let dinf = bx(
            b"dinf",
            &full(b"dref", 0, 0, &[&1u32.to_be_bytes()[..], &full(b"url ", 0, 1, &[])].concat()),
        );
        let minf = bx(b"minf", &[full(b"vmhd", 0, 1, &[0; 8]), dinf, stbl].concat());
        let mdhd = full(
            b"mdhd",
            0,
            0,
            &[&be32(&[0, 0, TIMESCALE as u32, duration as u32])[..], &[0x55, 0xc4, 0, 0]].concat(),
        );
        let hdlr = full(b"hdlr", 0, 0, &[&be32(&[0])[..], b"vide", &[0; 12], b"video\0"].concat());
        let mdia = bx(b"mdia", &[mdhd, hdlr, minf].concat());
        let tkhd = full(
            b"tkhd",
            0,
            3,
            &[
                &be32(&[0, 0, 1, 0, millis, 0, 0])[..],
                &[0; 8],
                &be32(&MATRIX),
                &be32(&[width << 16, height << 16]),
            ]
            .concat(),
        );
        let trak = bx(b"trak", &[tkhd, mdia].concat());
        let mvhd = full(
            b"mvhd",
            0,
            0,
            &[
                &be32(&[0, 0, 1000, millis, 0x0001_0000])[..],
                &[0x01, 0x00, 0, 0],
                &[0; 8],
                &be32(&MATRIX),
                &[0; 24],
                &be32(&[2]),
            ]
            .concat(),
        );
        let moov = bx(b"moov", &[mvhd, trak].concat());
        let ftyp = bx(b"ftyp", &[&b"isom"[..], &be32(&[0x200]), b"isomiso2avc1mp41"].concat());
        // A 64-bit `mdat` size, so the header has one shape whatever the length.
        let mdat = [&1u32.to_be_bytes()[..], b"mdat", &(data + 16).to_be_bytes()].concat();
        [ftyp, moov, mdat].concat()
    };
    let len = build(0).len() as u64;
    Some(build(len))
}

/// A whole mp4 of Annex B access units, frame `i` at exactly `i / fps` s:
/// `ceil(i / fps)` in nanoseconds is a whole number of 90 kHz ticks for a
/// rate that divides 90,000.
pub fn file(frames: &[Vec<u8>], fps: u64) -> Option<Vec<u8>> {
    let (sps, pps) = parameter_sets(frames.first()?)?;
    let samples: Vec<Vec<u8>> = frames.iter().map(|f| avcc(f)).collect();
    let index: Vec<Sample> = samples
        .iter()
        .zip(frames)
        .enumerate()
        .map(|(i, (s, f))| Sample {
            t_ns: (i as u64 * 1_000_000_000).div_ceil(fps),
            size: s.len() as u32,
            key: is_key(f),
        })
        .collect();
    let mut out = header(&index, &sps, &pps)?;
    for s in samples {
        out.extend_from_slice(&s);
    }
    Some(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The SPS and PPS of a real take's left wrist camera (2026-10-21,
    /// Quest 3 hardware encoder, High profile).
    pub(crate) const SPS_640X480: &[u8] = &[
        0x67, 0x64, 0x00, 0x20, 0xac, 0xb4, 0x05, 0x01, 0xed, 0x37, 0x06, 0x06, 0x05, 0x06, 0xd0,
        0xa1, 0x35,
    ];

    #[test]
    fn a_high_profile_sps_gives_its_picture_size() {
        assert_eq!(dimensions(SPS_640X480), Some((640, 480)));
    }

    #[test]
    fn units_split_on_either_start_code_and_lose_it() {
        let au = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 0, 5];
        let units = nal_units(&au);
        assert_eq!(units, vec![&[0x67, 1, 2][..], &[0x68, 3], &[0x65, 4, 0, 5]]);
        assert!(is_key(&au));
        assert_eq!(parameter_sets(&au), Some((vec![0x67, 1, 2], vec![0x68, 3])));
        assert_eq!(avcc(&au), vec![0, 0, 0, 4, 0x65, 4, 0, 5]);
    }

    #[test]
    fn a_file_puts_every_frame_one_thirtieth_of_a_second_apart() {
        let au = |unit: &[u8]| {
            let pps: &[u8] = &[0, 0, 0, 1, 0x68, 0xee];
            [&[0, 0, 0, 1][..], SPS_640X480, pps, &[0, 0, 0, 1], unit].concat()
        };
        let frames: Vec<Vec<u8>> = (0..31).map(|i| au(&[0x65, i])).collect();
        let mp4 = file(&frames, 30).unwrap();
        let stts = mp4.windows(4).position(|w| w == b"stts").unwrap();
        let entries: Vec<u32> = (0..3)
            .map(|k| u32::from_be_bytes(mp4[stts + 8 + 4 * k..][..4].try_into().unwrap()))
            .collect();
        assert_eq!(entries, [1, 31, 3000], "one run of 31 samples, 3000 ticks each");
        assert!(mp4.ends_with(&[0, 0, 0, 2, 0x65, 30]));
    }

    #[test]
    fn the_header_ends_where_the_first_sample_begins() {
        let samples: Vec<Sample> = (0..4)
            .map(|i| Sample { t_ns: 1_000_000_000 + i * 33_333_333, size: 100, key: i == 0 })
            .collect();
        let head = header(&samples, SPS_640X480, &[0x68, 0xee]).unwrap();
        let co64 = head.windows(4).position(|w| w == b"co64").unwrap();
        let at = u64::from_be_bytes(head[co64 + 12..co64 + 20].try_into().unwrap());
        assert_eq!(at, head.len() as u64);
        assert_eq!(&head[head.len() - 12..head.len() - 8], b"mdat");
        assert_eq!(u64::from_be_bytes(head[head.len() - 8..].try_into().unwrap()), 416);
    }
}
