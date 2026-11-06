#[cfg(target_os = "android")]
use anyhow::Context as _;
use nusb::descriptors::{ConfigurationDescriptor, TransferType};
use std::time::{Duration, Instant};

use crate::misc::capture::UrbAt;

const CS_INTERFACE: u8 = 0x24;
const VS_FORMAT_MJPEG: u8 = 0x06;
const VS_FRAME_MJPEG: u8 = 0x07;

#[cfg(target_os = "android")]
const SET_CUR: u8 = 0x01;
#[cfg(target_os = "android")]
const GET_CUR: u8 = 0x81;
#[cfg(target_os = "android")]
const VS_PROBE_CONTROL: u16 = 0x01 << 8;
#[cfg(target_os = "android")]
const VS_COMMIT_CONTROL: u16 = 0x02 << 8;

const CC_VIDEO: u8 = 0x0e;
const SC_VIDEOSTREAMING: u8 = 0x02;

/// The wrists' preferred frame interval, in UVC's 100 ns units: 120 fps, the
/// rate of the pose sampler (`view::xr::POSE_HZ`). Downstream can drop to
/// any lower rate, but frames never captured cannot be recovered.
pub const TARGET_INTERVAL: u32 = 83_333;

/// The smallest iso endpoint a wrist may be given, in bytes per microframe.
/// The wrists deliver 4.6-5.9 MB/s at ~98 fps, 575-740 bytes per microframe,
/// so 1200 is about twice the peak. That selects alt 4 at 1600, and three
/// cameras still fit USB 2.0's ~6000-byte periodic budget.
pub const MIN_ISO_BYTES_PER_MICROFRAME: usize = 1200;

fn pick_interval(d: &[u8], target: u32) -> u32 {
    let at = |o: usize| d.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let default = at(21).unwrap_or(target);
    match d[25] as usize {
        0 => match (at(26), at(30)) {
            (Some(min), Some(max)) if min <= max => target.clamp(min, max),
            _ => default,
        },
        n => (0..n)
            .filter_map(|i| at(26 + 4 * i))
            .min_by_key(|v| v.abs_diff(target))
            .unwrap_or(default),
    }
}

#[derive(Debug, Clone)]
pub struct MjpegFrame {
    pub frame_index: u8,
    pub width: u16,
    pub height: u16,
    pub interval: u32,
}

#[derive(Debug, Clone)]
pub struct StreamInfo {
    pub vs_interface: u8,
    pub format_index: u8,
    pub frame: MjpegFrame,
    pub ep_address: u8,
    pub ep_transfer_type: TransferType,
    pub ep_max_packet: usize,
    pub ep_alt_setting: u8,
}

pub fn find_mjpeg_stream(
    config: &ConfigurationDescriptor,
    want_w: u16,
    want_h: u16,
    want_interval: u32,
    min_iso_bytes: usize,
) -> Option<StreamInfo> {
    #[derive(Default)]
    struct Vs {
        number: u8,
        format_index: Option<u8>,
        /// Off once a second MJPEG format starts: its frames are not the first's.
        taking: bool,
        frames: Vec<MjpegFrame>,
        endpoints: Vec<(u8, TransferType, usize, u8)>,
    }
    let mut interfaces: Vec<Vs> = Vec::new();

    for intf in config.interface_alt_settings() {
        if intf.class() != CC_VIDEO || intf.subclass() != SC_VIDEOSTREAMING {
            continue;
        }
        let vs = match interfaces.iter().position(|v| v.number == intf.interface_number()) {
            Some(i) => &mut interfaces[i],
            None => {
                interfaces.push(Vs { number: intf.interface_number(), ..Vs::default() });
                interfaces.last_mut().expect("just pushed")
            }
        };

        for d in intf.descriptors() {
            if d.descriptor_type() != CS_INTERFACE || d.len() < 3 {
                continue;
            }
            match d[2] {
                VS_FORMAT_MJPEG if d.len() > 3 => {
                    vs.taking = vs.format_index.is_none();
                    vs.format_index.get_or_insert(d[3]);
                }
                VS_FRAME_MJPEG if d.len() >= 26 && vs.taking => vs.frames.push(MjpegFrame {
                    frame_index: d[3],
                    width: u16::from_le_bytes([d[5], d[6]]),
                    height: u16::from_le_bytes([d[7], d[8]]),
                    interval: pick_interval(&d, want_interval),
                }),
                _ => {}
            }
        }

        for ep in intf.endpoints().filter(|e| e.address() & 0x80 != 0) {
            let bytes = ep.max_packet_size() * ep.packets_per_microframe() as usize;
            vs.endpoints.push((ep.address(), ep.transfer_type(), bytes, intf.alternate_setting()));
        }
    }

    let Vs { number: vs_interface, format_index, frames, endpoints, .. } =
        interfaces.into_iter().find(|v| {
            v.format_index.is_some() && !v.frames.is_empty() && !v.endpoints.is_empty()
        })?;
    let widest = endpoints.iter().copied().max_by_key(|&(_, _, bytes, _)| bytes)?;
    let (ep_address, ep_transfer_type, ep_max_packet, ep_alt_setting) = endpoints
        .iter()
        .copied()
        .filter(|&(_, _, bytes, _)| bytes >= min_iso_bytes)
        .min_by_key(|&(_, _, bytes, _)| bytes)
        .unwrap_or(widest);
    log::info!(
        "  iso endpoints {:?} -> alt {ep_alt_setting} ({ep_max_packet} bytes/microframe)",
        endpoints.iter().map(|&(_, _, b, a)| (a, b)).collect::<Vec<_>>(),
    );
    for f in &frames {
        log::info!(
            "  offers {}x{} interval={} ({:.1} fps)",
            f.width,
            f.height,
            f.interval,
            1e7 / f.interval as f32
        );
    }
    // Closest in lines first, then in width. The stereo module packs two eyes
    // into one frame, so its 480-line mode is 1280 wide, and summing |dw|+|dh|
    // would answer a 640x480 request with 640x240: two 320x240 eyes.
    let frame = frames.into_iter().min_by_key(|f| {
        ((f.height as i32 - want_h as i32).abs(), (f.width as i32 - want_w as i32).abs())
    })?;

    Some(StreamInfo {
        vs_interface,
        format_index: format_index?,
        frame,
        ep_address,
        ep_transfer_type,
        ep_max_packet,
        ep_alt_setting,
    })
}

#[cfg(any(target_os = "android", test))]
#[derive(Debug, Clone)]
pub struct Negotiated {
    pub max_payload_transfer_size: u32,
    #[allow(dead_code)]
    pub max_video_frame_size: u32,
    pub frame_interval: u32,
}

#[cfg(any(target_os = "android", test))]
pub fn build_probe(info: &StreamInfo) -> [u8; 26] {
    let mut p = [0u8; 26];
    p[0] = 0x01;
    p[2] = info.format_index;
    p[3] = info.frame.frame_index;
    p[4..8].copy_from_slice(&info.frame.interval.to_le_bytes());
    p
}

#[cfg(any(target_os = "android", test))]
pub fn parse_probe_response(data: &[u8]) -> Option<Negotiated> {
    if data.len() < 26 {
        return None;
    }
    Some(Negotiated {
        frame_interval: u32::from_le_bytes(data[4..8].try_into().ok()?),
        max_video_frame_size: u32::from_le_bytes(data[18..22].try_into().ok()?),
        max_payload_transfer_size: u32::from_le_bytes(data[22..26].try_into().ok()?),
    })
}

#[cfg(target_os = "android")]
pub fn probe_commit(dev: &super::usbfs::Device, info: &StreamInfo) -> anyhow::Result<Negotiated> {
    const CLASS_IFACE_OUT: u8 = 0x21;
    const CLASS_IFACE_IN: u8 = 0xa1;
    let index = info.vs_interface as u16;

    let mut probe = build_probe(info);
    dev.control(CLASS_IFACE_OUT, SET_CUR, VS_PROBE_CONTROL, index, &mut probe)
        .context("probe SET_CUR")?;

    let mut buf = [0u8; 48];
    let len = [48usize, 34, 26]
        .iter()
        .find_map(|&l| {
            dev.control(CLASS_IFACE_IN, GET_CUR, VS_PROBE_CONTROL, index, &mut buf[..l]).ok()
        })
        .context("probe GET_CUR failed at 48/34/26 bytes")?;

    let negotiated = parse_probe_response(&buf[..len])
        .ok_or_else(|| anyhow::anyhow!("short probe response: {len} bytes"))?;

    const MAX_PAYLOAD_OFFSET: usize = 22;
    let cap = info.ep_max_packet.min(u32::MAX as usize) as u32;
    if len >= MAX_PAYLOAD_OFFSET + 4 && negotiated.max_payload_transfer_size > cap {
        buf[MAX_PAYLOAD_OFFSET..MAX_PAYLOAD_OFFSET + 4].copy_from_slice(&cap.to_le_bytes());
        log::info!(
            "  clamping payload {} -> {cap} to match the chosen endpoint",
            negotiated.max_payload_transfer_size,
        );
    }
    dev.control(CLASS_IFACE_OUT, SET_CUR, VS_COMMIT_CONTROL, index, &mut buf[..len])
        .context("COMMIT")?;
    Ok(negotiated)
}

const SC_VIDEOCONTROL: u8 = 0x01;
const VC_INPUT_TERMINAL: u8 = 0x02;
const VC_PROCESSING_UNIT: u8 = 0x05;
const ITT_CAMERA: u16 = 0x0201;

/// `(bmControls bit, selector, bytes, signed, name)` for the camera terminal
/// and the processing unit, from UVC 1.5 tables 4-3/4-6 and A.9.4/A.9.5. Only
/// the ones that change what a frame looks like or when it was exposed.
type ControlSpec = (u8, u8, u8, bool, &'static str);
const CT_CONTROLS: [ControlSpec; 3] = [
    (1, 0x02, 1, false, "ae_mode"),
    (2, 0x03, 1, false, "ae_priority"),
    (3, 0x04, 4, false, "exposure_time_absolute_100us"),
];
const PU_CONTROLS: [ControlSpec; 10] = [
    (0, 0x02, 2, true, "brightness"),
    (1, 0x03, 2, false, "contrast"),
    (3, 0x07, 2, false, "saturation"),
    (4, 0x08, 2, false, "sharpness"),
    (5, 0x09, 2, false, "gamma"),
    (6, 0x0a, 2, false, "white_balance_temperature"),
    (8, 0x01, 2, false, "backlight_compensation"),
    (9, 0x04, 2, false, "gain"),
    (10, 0x05, 1, false, "power_line_frequency"),
    (12, 0x0b, 1, false, "white_balance_temperature_auto"),
];

/// One control the camera says it has, addressed for GET_CUR.
#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    pub name: &'static str,
    pub selector: u8,
    pub len: u8,
    pub signed: bool,
    /// wIndex: entity id in the high byte, VideoControl interface in the low.
    pub index: u16,
}

/// The camera-terminal and processing-unit controls this camera advertises.
pub fn find_controls(config: &ConfigurationDescriptor) -> Vec<Control> {
    let mut out = Vec::new();
    for intf in config.interface_alt_settings() {
        if intf.class() != CC_VIDEO || intf.subclass() != SC_VIDEOCONTROL {
            continue;
        }
        let vc = intf.interface_number() as u16;
        for d in intf.descriptors() {
            if d.descriptor_type() != CS_INTERFACE || d.len() < 8 {
                continue;
            }
            let (specs, size_at): (&[ControlSpec], usize) = match d[2] {
                VC_INPUT_TERMINAL
                    if d.len() >= 15 && u16::from_le_bytes([d[4], d[5]]) == ITT_CAMERA =>
                {
                    (&CT_CONTROLS, 14)
                }
                VC_PROCESSING_UNIT => (&PU_CONTROLS, 7),
                _ => continue,
            };
            let end = (size_at + 1 + d[size_at] as usize).min(d.len());
            let bits = &d[size_at + 1..end];
            let has =
                |bit: u8| bits.get(bit as usize / 8).is_some_and(|b| b & (1 << (bit % 8)) != 0);
            for &(_, selector, len, signed, name) in specs.iter().filter(|c| has(c.0)) {
                out.push(Control { name, selector, len, signed, index: (d[3] as u16) << 8 | vc });
            }
        }
    }
    out
}

/// The control's current value, little-endian as UVC sends it.
#[cfg(target_os = "android")]
pub fn read_control(dev: &super::usbfs::ControlHandle, c: &Control) -> std::io::Result<i64> {
    const CLASS_IFACE_IN: u8 = 0xa1;
    let mut buf = [0u8; 4];
    let value = (c.selector as u16) << 8;
    let n = dev.control(CLASS_IFACE_IN, GET_CUR, value, c.index, &mut buf[..c.len as usize])?;
    Ok(control_value(&buf[..n.min(c.len as usize)], c.signed))
}

#[cfg(target_os = "android")]
pub fn write_control(
    dev: &super::usbfs::ControlHandle,
    c: &Control,
    v: i64,
) -> std::io::Result<()> {
    const CLASS_IFACE_OUT: u8 = 0x21;
    let mut buf = v.to_le_bytes();
    let value = (c.selector as u16) << 8;
    dev.control(CLASS_IFACE_OUT, SET_CUR, value, c.index, &mut buf[..c.len as usize])?;
    Ok(())
}

#[cfg(any(target_os = "android", test))]
fn control_value(bytes: &[u8], signed: bool) -> i64 {
    let mut raw = [0u8; 8];
    raw[..bytes.len()].copy_from_slice(bytes);
    let v = u64::from_le_bytes(raw);
    match (signed, bytes.len()) {
        (true, 1) => v as u8 as i8 as i64,
        (true, 2) => v as u16 as i16 as i64,
        (true, 4) => v as u32 as i32 as i64,
        _ => v as i64,
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Timing {
    pub pts: Option<u32>,
    pub scr: Option<u64>,
    /// Where the frame's first and last payloads sat on the bus.
    pub first: Option<UrbAt>,
    pub last: UrbAt,
    /// The stream's counters as this frame closed.
    pub counts: Counts,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Counts {
    pub errors: u64,
    pub bad_headers: u64,
    pub damaged_frames: u64,
    pub lost_packets: u64,
}

#[derive(Debug, Default)]
pub struct FrameCounter {
    pub frames_eof: u64,
    pub fid_toggles: u64,
    pub payload_bytes: u64,
    pub errors: u64,
    pub bad_headers: u64,
    pub damaged_frames: u64,
    pub lost_packets: u64,
    last_fid: Option<bool>,
    damaged: bool,
    frame: Vec<u8>,
    timing: Timing,
    at: UrbAt,
    /// The FID of the frame an EOF just closed. Until the next frame starts, the
    /// camera keeps sending header-only payloads with that FID, still carrying
    /// the closed frame's PTS; taken as the next frame's, they put PTS one frame
    /// late (10 frames in every 27 on the wrist cameras, a Quest 3S, 2026-10-21). Only
    /// header-only ones are skipped: a camera that never toggles FID still
    /// starts its next frame with data.
    ended_fid: Option<bool>,
}

impl FrameCounter {
    #[cfg(test)]
    pub fn push_payload(&mut self, payload: &[u8]) {
        self.push_payload_with(payload, |_, _, _| {})
    }

    pub fn lost_packet(&mut self) {
        self.lost_packets += 1;
        self.damaged = true;
    }

    /// Where the next payload sits on the bus; see [`UrbAt`].
    pub fn set_at(&mut self, at: UrbAt) {
        self.at = at;
    }

    pub fn push_payload_with(
        &mut self,
        payload: &[u8],
        mut on_frame: impl FnMut(Timing, u64, &[u8]),
    ) {
        let fid = match payload {
            [hlen, info, ..] if *hlen >= 2 && *hlen as usize <= payload.len() => {
                Some(info & 0x01 != 0)
            }
            _ => None,
        };
        let header_only = payload.first().is_some_and(|&h| h as usize == payload.len());
        if header_only && fid.is_some() && fid == self.ended_fid {
            return;
        }
        self.ended_fid = None;
        // Until a camera has sent an EOF, a FID toggle is its only frame
        // boundary, and the payload carrying the new FID opens the next frame.
        if self.frames_eof == 0 && self.last_fid.is_some() && fid.is_some() && fid != self.last_fid
        {
            self.close(self.fid_toggles, &mut on_frame);
        }
        let eofs = self.frames_eof;
        self.timing.first.get_or_insert(self.at);
        self.timing.last = self.at;
        self.count(payload);
        if let Some(hlen) = payload.first().map(|&h| h as usize) {
            if hlen >= 2 && hlen <= payload.len() {
                self.frame.extend_from_slice(&payload[hlen..]);
            }
        }
        if self.frames_eof != eofs {
            self.ended_fid = fid;
            self.close(self.frames_eof - 1, &mut on_frame);
        }
    }

    fn close(&mut self, ordinal: u64, on_frame: &mut impl FnMut(Timing, u64, &[u8])) {
        if self.damaged {
            self.damaged_frames += 1;
        } else if self.frame.starts_with(&[0xff, 0xd8]) {
            self.timing.counts = Counts {
                errors: self.errors,
                bad_headers: self.bad_headers,
                damaged_frames: self.damaged_frames,
                lost_packets: self.lost_packets,
            };
            on_frame(self.timing, ordinal, &self.frame);
        }
        self.frame.clear();
        self.timing = Timing::default();
        self.damaged = false;
    }

    fn count(&mut self, payload: &[u8]) {
        if payload.is_empty() {
            return;
        }
        let hlen = payload[0] as usize;
        if hlen < 2 || hlen > payload.len() {
            self.bad_headers += 1;
            self.damaged = true;
            return;
        }
        let info = payload[1];
        let fid = info & 0x01 != 0;
        if info & 0x40 != 0 {
            self.errors += 1;
            self.damaged = true;
        }
        if info & 0x02 != 0 {
            self.frames_eof += 1;
        }
        if self.last_fid != Some(fid) {
            if self.last_fid.is_some() {
                self.fid_toggles += 1;
                if self.frames_eof > 0 && !self.frame.is_empty() {
                    self.damaged = true;
                }
            }
            self.last_fid = Some(fid);
        }
        self.read_timing(payload, hlen, info);
        self.payload_bytes += (payload.len() - hlen) as u64;
    }

    fn read_timing(&mut self, payload: &[u8], hlen: usize, info: u8) {
        let mut at = 2;
        if info & 0x04 != 0 {
            if at + 4 > hlen {
                return;
            }
            let pts = u32::from_le_bytes(payload[at..at + 4].try_into().expect("4 bytes"));
            self.timing.pts.get_or_insert(pts);
            at += 4;
        }
        if info & 0x08 != 0 && at + 6 <= hlen {
            let mut scr = [0u8; 8];
            scr[..6].copy_from_slice(&payload[at..at + 6]);
            self.timing.scr.get_or_insert(u64::from_le_bytes(scr));
        }
    }

    pub fn frames(&self) -> u64 {
        if self.frames_eof > 0 {
            self.frames_eof
        } else {
            self.fid_toggles
        }
    }
}

/// How long a camera may deliver bytes and complete no frame before its
/// worker gives up and the scan reopens it. Three seconds is ~300 frames at a
/// healthy wrist's rate, and past the one second after which the gate already
/// reports the camera as stalled.
pub const STALL_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug)]
pub struct StallWatch {
    since: Instant,
    frames: u64,
    bytes: u64,
}

impl StallWatch {
    pub fn new(now: Instant) -> Self {
        Self { since: now, frames: 0, bytes: 0 }
    }

    pub fn poll(&mut self, now: Instant, frames: u64, bytes: u64) -> bool {
        if frames != self.frames {
            self.since = now;
            self.frames = frames;
        }
        let flowing = bytes > self.bytes;
        self.bytes = bytes;
        flowing && now.duration_since(self.since) >= STALL_TIMEOUT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(fid: bool, eof: bool, data_len: usize) -> Vec<u8> {
        let mut p = vec![2, (fid as u8) | ((eof as u8) << 1)];
        p.extend(std::iter::repeat_n(0xab, data_len));
        p
    }

    #[test]
    fn counts_frames_by_eof_and_fid() {
        let mut c = FrameCounter::default();
        c.push_payload(&payload(false, false, 100));
        c.push_payload(&payload(false, true, 50));
        c.push_payload(&payload(true, false, 100));
        c.push_payload(&payload(true, true, 50));
        assert_eq!(c.frames_eof, 2);
        assert_eq!(c.fid_toggles, 1);
        assert_eq!(c.frames(), 2);
        assert_eq!(c.payload_bytes, 300);
        assert_eq!(c.errors, 0);
    }

    #[test]
    fn fid_fallback_and_bad_headers() {
        let mut c = FrameCounter::default();
        c.push_payload(&payload(false, false, 10));
        c.push_payload(&payload(true, false, 10));
        c.push_payload(&payload(false, false, 10));
        assert_eq!(c.frames_eof, 0);
        assert_eq!(c.frames(), 2);
        c.push_payload(&[99, 0]);
        c.push_payload(&[]);
        assert_eq!(c.bad_headers, 1);
    }

    fn timed(eof: bool, pts: Option<u32>, scr: Option<u64>, data_len: usize) -> Vec<u8> {
        let info = ((eof as u8) << 1) | ((pts.is_some() as u8) << 2) | ((scr.is_some() as u8) << 3);
        let mut p = vec![0, info];
        if let Some(pts) = pts {
            p.extend(pts.to_le_bytes());
        }
        if let Some(scr) = scr {
            p.extend(&scr.to_le_bytes()[..6]);
        }
        p[0] = p.len() as u8;
        p.extend(std::iter::repeat_n(0xab, data_len));
        p
    }

    #[test]
    fn reads_pts_and_scr_out_of_the_payload_header() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let scr = 0x0123_89ab_cdef;
        let mut jpeg = timed(false, Some(0xdead_beef), Some(scr), 0);
        jpeg.extend([0xff, 0xd8, 0x00]);
        c.push_payload_with(&jpeg, |t, _, _| got.push(t));
        c.push_payload_with(&timed(true, Some(0xdead_beef), None, 4), |t, _, _| got.push(t));
        let clocks = |t: &Timing| (t.pts, t.scr);
        assert_eq!(
            got.iter().map(clocks).collect::<Vec<_>>(),
            vec![(Some(0xdead_beef), Some(scr))]
        );

        let mut only_scr = timed(false, None, Some(7), 0);
        only_scr.extend([0xff, 0xd8, 0x00]);
        c.push_payload_with(&only_scr, |t, _, _| got.push(t));
        c.push_payload_with(&timed(true, None, None, 4), |t, _, _| got.push(t));
        assert_eq!(clocks(&got[1]), (None, Some(7)));
    }

    #[test]
    fn finds_the_controls_a_camera_terminal_and_processing_unit_advertise() {
        // config(9) + VC interface(9) + camera terminal(18, bmControls 0x0a)
        // + processing unit(11, bmControls 0x0201: brightness and gain).
        let mut d = vec![9, 2, 0, 0, 1, 1, 0, 0x80, 50];
        d.extend([9, 4, 0, 0, 0, CC_VIDEO, SC_VIDEOCONTROL, 0, 0]);
        d.extend([18, CS_INTERFACE, VC_INPUT_TERMINAL, 1, 0x01, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 3]);
        d.extend([0x0a, 0, 0]);
        d.extend([11, CS_INTERFACE, VC_PROCESSING_UNIT, 2, 1, 0, 0, 2, 0x01, 0x02, 0]);
        let total = d.len() as u16;
        d[2..4].copy_from_slice(&total.to_le_bytes());
        let config = ConfigurationDescriptor::new(&d).unwrap();
        let names: Vec<_> = find_controls(&config).iter().map(|c| (c.name, c.index)).collect();
        assert_eq!(
            names,
            [
                ("ae_mode", 0x0100),
                ("exposure_time_absolute_100us", 0x0100),
                ("brightness", 0x0200),
                ("gain", 0x0200),
            ]
        );
        assert_eq!(control_value(&[0xff, 0xff], true), -1);
        assert_eq!(control_value(&[0x10, 0x27, 0, 0], false), 10_000);
    }

    #[test]
    fn payloads_trailing_an_eof_do_not_lend_their_pts_to_the_next_frame() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let start = |fid: bool, pts: u32| {
            let mut p = timed(false, Some(pts), None, 0);
            p[1] |= fid as u8;
            p.extend([0xff, 0xd8, 0x00]);
            p
        };
        let with_fid = |mut p: Vec<u8>, fid: bool| {
            p[1] |= fid as u8;
            p
        };
        c.push_payload_with(&start(false, 100), |t, _, _| got.push(t.pts));
        c.push_payload_with(&timed(true, Some(100), None, 4), |t, _, _| got.push(t.pts));
        // Idle microframes before the next exposure: same FID, the old PTS.
        c.push_payload_with(&timed(false, Some(100), None, 0), |t, _, _| got.push(t.pts));
        c.push_payload_with(&timed(false, Some(100), None, 0), |t, _, _| got.push(t.pts));
        c.push_payload_with(&start(true, 200), |t, _, _| got.push(t.pts));
        c.push_payload_with(&with_fid(timed(true, Some(200), None, 4), true), |t, _, _| {
            got.push(t.pts)
        });
        assert_eq!(got, [Some(100), Some(200)]);
        assert_eq!(c.frames(), 2, "the trailing payloads are not frames");
        assert_eq!(c.damaged_frames, 0);
    }

    #[test]
    fn a_frame_carries_where_its_first_and_last_payloads_sat_on_the_bus() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let at = |reaped_ns, packet| UrbAt { reaped_ns, packet };
        let mut jpeg = payload(false, false, 0);
        jpeg.extend([0xff, 0xd8, 0x00]);
        c.set_at(at(100, 31));
        c.push_payload_with(&jpeg, |t, _, _| got.push(t));
        c.set_at(at(104, 2));
        c.push_payload_with(&payload(false, true, 4), |t, _, _| got.push(t));
        assert_eq!((got[0].first, got[0].last), (Some(at(100, 31)), at(104, 2)));

        c.lost_packet();
        c.set_at(at(108, 0));
        c.push_payload_with(&payload(true, true, 4), |t, _, _| got.push(t));
        c.push_payload_with(&jpeg, |t, _, _| got.push(t));
        c.push_payload_with(&payload(false, true, 4), |t, _, _| got.push(t));
        let counts = Counts { damaged_frames: 1, lost_packets: 1, ..Counts::default() };
        assert_eq!(got.last().unwrap().counts, counts, "the damaged frame is counted, not sent");
    }

    #[test]
    fn the_frame_ordinal_counts_past_a_frame_we_could_not_assemble() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let mut jpeg = vec![2u8, 0b10];
        jpeg.extend([0xff, 0xd8, 0x00]);

        c.push_payload_with(&jpeg, |_, n, _| got.push(n));
        c.push_payload_with(&payload(false, true, 4), |_, n, _| got.push(n));
        c.push_payload_with(&jpeg, |_, n, _| got.push(n));
        assert_eq!(got, vec![0, 2]);
    }

    fn whole_frame(fid: bool) -> [Vec<u8>; 2] {
        let mut first = payload(fid, false, 0);
        first.extend([0xff, 0xd8, 0x11, 0x22]);
        [first, payload(fid, true, 40)]
    }

    #[test]
    fn a_frame_that_lost_a_packet_is_refused() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let [a, b] = whole_frame(false);
        c.push_payload_with(&a, |_, n, _| got.push(n));
        c.lost_packet();
        c.push_payload_with(&b, |_, n, _| got.push(n));
        assert_eq!(got, Vec::<u64>::new(), "the holed frame must not be handed on");
        assert_eq!(c.damaged_frames, 1);

        let [a, b] = whole_frame(true);
        c.push_payload_with(&a, |_, n, _| got.push(n));
        c.push_payload_with(&b, |_, n, _| got.push(n));
        assert_eq!(got, vec![1], "the next whole frame arrives, ordinal intact");
        assert_eq!(c.damaged_frames, 1, "and is not counted as damaged");
    }

    #[test]
    fn a_frame_the_camera_marked_bad_is_refused() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let [a, b] = whole_frame(false);
        c.push_payload_with(&a, |_, n, _| got.push(n));
        c.push_payload_with(&[2, 0x40], |_, n, _| got.push(n));
        c.push_payload_with(&b, |_, n, _| got.push(n));
        assert_eq!(got, Vec::<u64>::new());
        assert_eq!((c.errors, c.damaged_frames), (1, 1));
    }

    #[test]
    fn a_frame_with_an_unreadable_header_in_it_is_refused() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let [a, b] = whole_frame(false);
        c.push_payload_with(&a, |_, n, _| got.push(n));
        c.push_payload_with(&[99, 0], |_, n, _| got.push(n));
        c.push_payload_with(&b, |_, n, _| got.push(n));
        assert_eq!(got, Vec::<u64>::new());
        assert_eq!((c.bad_headers, c.damaged_frames), (1, 1));
    }

    #[test]
    fn a_frame_whose_eof_went_missing_takes_the_next_one_with_it() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let [a, b] = whole_frame(false);
        c.push_payload_with(&a, |_, n, _| got.push(n));
        c.push_payload_with(&b, |_, n, _| got.push(n));
        assert_eq!(got, vec![0], "a whole frame first, so EOF is known to be this camera's mark");

        let [c2, _dropped_eof] = whole_frame(true);
        c.push_payload_with(&c2, |_, n, _| got.push(n));
        let [d, e] = whole_frame(false);
        c.push_payload_with(&d, |_, n, _| got.push(n));
        c.push_payload_with(&e, |_, n, _| got.push(n));
        assert_eq!(got, vec![0], "neither the frame missing its end nor the pair it joined");
        assert_eq!(c.damaged_frames, 1);
    }

    #[test]
    fn a_camera_without_eof_is_not_condemned_by_its_own_frame_boundary() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        for fid in [false, true, false] {
            let mut p = payload(fid, false, 0);
            p.extend([0xff, 0xd8, 0x11, 0x22]);
            c.push_payload_with(&p, |_, n, _| got.push(n));
        }
        assert_eq!(c.frames_eof, 0);
        assert_eq!(got, vec![0, 1], "two toggles, two frames, none damaged");
        assert_eq!(c.damaged_frames, 0);
    }

    /// One JPEG of `n` bytes cut into three payloads carrying `fid`, the last
    /// one marked EOF when `eof`.
    fn split_frame(fid: bool, eof: bool, n: u8) -> (Vec<u8>, [Vec<u8>; 3]) {
        let mut jpeg = vec![0xff, 0xd8];
        jpeg.extend(0..n);
        let cut = jpeg.len() / 3;
        let parts = [&jpeg[..cut], &jpeg[cut..2 * cut], &jpeg[2 * cut..]];
        let payloads = std::array::from_fn(|i| {
            let mut p = payload(fid, eof && i == 2, 0);
            p.extend(parts[i]);
            p
        });
        (jpeg, payloads)
    }

    #[test]
    fn a_camera_without_eof_delivers_every_frame_whole() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let mut sent = Vec::new();
        for (i, fid) in [false, true, false, true].into_iter().enumerate() {
            let (jpeg, payloads) = split_frame(fid, false, 10 + i as u8);
            sent.push(jpeg);
            for p in payloads {
                c.push_payload_with(&p, |_, n, f| got.push((n, f.to_vec())));
            }
        }
        let want: Vec<_> =
            sent.into_iter().take(3).enumerate().map(|(n, j)| (n as u64, j)).collect();
        assert_eq!(got, want, "each toggle hands on the frame before it, and only that frame");
        assert_eq!(c.damaged_frames, 0);
    }

    #[test]
    fn an_eof_camera_joined_mid_frame_loses_only_the_partial_one() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let (_, [_, tail_a, tail_b]) = split_frame(false, true, 10);
        c.push_payload_with(&tail_a, |_, n, f| got.push((n, f.to_vec())));
        c.push_payload_with(&tail_b, |_, n, f| got.push((n, f.to_vec())));
        let mut sent = Vec::new();
        for fid in [true, false] {
            let (jpeg, payloads) = split_frame(fid, true, 12);
            sent.push(jpeg);
            for p in payloads {
                c.push_payload_with(&p, |_, n, f| got.push((n, f.to_vec())));
            }
        }
        assert_eq!(got.iter().map(|g| &g.1).collect::<Vec<_>>(), sent.iter().collect::<Vec<_>>());
        assert_eq!(c.damaged_frames, 0);
    }

    #[test]
    fn a_header_that_claims_timing_it_did_not_send_is_ignored() {
        let mut c = FrameCounter::default();
        let mut got = Vec::new();
        let mut lying = vec![2u8, 0x0c];
        lying.extend([0xff, 0xd8, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        c.push_payload_with(&lying, |t, _, _| got.push(t));
        c.push_payload_with(&payload(false, true, 4), |t, _, _| got.push(t));
        assert_eq!(got.iter().map(|t| (t.pts, t.scr)).collect::<Vec<_>>(), vec![(None, None)]);
    }

    #[test]
    fn probe_roundtrip() {
        let info = StreamInfo {
            vs_interface: 1,
            format_index: 1,
            frame: MjpegFrame { frame_index: 2, width: 1280, height: 720, interval: 333_333 },
            ep_address: 0x81,
            ep_transfer_type: TransferType::Bulk,
            ep_max_packet: 512,
            ep_alt_setting: 0,
        };
        let p = build_probe(&info);
        assert_eq!(p[2], 1);
        assert_eq!(p[3], 2);
        let mut resp = p.to_vec();
        resp[22..26].copy_from_slice(&(16384u32).to_le_bytes());
        let n = parse_probe_response(&resp).unwrap();
        assert_eq!(n.max_payload_transfer_size, 16384);
        assert_eq!(n.frame_interval, 333_333);
        assert!(parse_probe_response(&resp[..20]).is_none());
    }

    #[test]
    fn bytes_without_frames_is_a_stall() {
        let t0 = Instant::now();
        let mut watch = StallWatch::new(t0);
        let mut bytes = 0;
        for f in 1..=3 {
            bytes += 100_000;
            assert!(!watch.poll(t0, f, bytes), "a framing camera is never stalled");
        }
        bytes += 100_000;
        assert!(!watch.poll(t0 + STALL_TIMEOUT / 2, 3, bytes), "too soon to give up");
        bytes += 100_000;
        assert!(watch.poll(t0 + STALL_TIMEOUT, 3, bytes), "bytes, no frames, long enough");
    }

    #[test]
    fn a_framing_camera_never_stalls() {
        let t0 = Instant::now();
        let mut watch = StallWatch::new(t0);
        for tick in 1..1_000u32 {
            let now = t0 + STALL_TIMEOUT * tick;
            assert!(!watch.poll(now, tick as u64, tick as u64 * 100_000), "tick {tick}");
        }
    }

    #[test]
    fn silence_is_not_a_stall() {
        let t0 = Instant::now();
        let mut watch = StallWatch::new(t0);
        watch.poll(t0, 3, 300_000);
        assert!(!watch.poll(t0 + STALL_TIMEOUT * 10, 3, 300_000), "no bytes either: not ours");
    }

    #[test]
    fn the_clock_restarts_on_every_frame() {
        let t0 = Instant::now();
        let mut watch = StallWatch::new(t0);
        let late = t0 + STALL_TIMEOUT * 100;
        assert!(!watch.poll(late, 5_000, 500_000), "framing right up to here");
        assert!(!watch.poll(late + STALL_TIMEOUT / 2, 5_000, 600_000));
        assert!(watch.poll(late + STALL_TIMEOUT, 5_000, 700_000));
    }
}

#[cfg(test)]
mod interval_tests {
    use super::*;

    fn frame_desc(intervals: &[u32], default: u32) -> Vec<u8> {
        let mut d = vec![0u8; 26];
        d[21..25].copy_from_slice(&default.to_le_bytes());
        d[25] = intervals.len() as u8;
        for i in intervals {
            d.extend_from_slice(&i.to_le_bytes());
        }
        d
    }

    #[test]
    fn picks_nearest_discrete_interval() {
        const FPS30: u32 = 333_333;
        let d = frame_desc(&[166_666, FPS30, 666_666], 166_666);
        assert_eq!(pick_interval(&d, FPS30), FPS30);
        let d = frame_desc(&[166_666, 666_666], 166_666);
        assert_eq!(pick_interval(&d, FPS30), 166_666);
    }

    #[test]
    fn prefers_high_rate_when_the_camera_offers_a_choice() {
        let only_fast = frame_desc(&[83_263], 83_263);
        assert_eq!(pick_interval(&only_fast, TARGET_INTERVAL), 83_263);

        let choice = frame_desc(&[83_263, 166_666, 333_333], 333_333);
        assert_eq!(pick_interval(&choice, TARGET_INTERVAL), 83_263, "must not settle for 30 fps");
    }

    #[test]
    fn clamps_continuous_range_and_falls_back() {
        let mut d = vec![0u8; 38];
        d[21..25].copy_from_slice(&166_666u32.to_le_bytes());
        d[25] = 0;
        d[26..30].copy_from_slice(&400_000u32.to_le_bytes());
        d[30..34].copy_from_slice(&800_000u32.to_le_bytes());
        assert_eq!(pick_interval(&d, TARGET_INTERVAL), 400_000);
        assert_eq!(pick_interval(&d[..27], TARGET_INTERVAL), 166_666);
        d[26..30].copy_from_slice(&900_000u32.to_le_bytes());
        assert_eq!(pick_interval(&d, TARGET_INTERVAL), 166_666, "min > max is the default");
    }

    /// A configuration with one VideoStreaming interface offering MJPEG at
    /// `sizes` (60 fps only), and one iso alt setting per `(mult, size)`.
    fn config(sizes: &[(u16, u16)], endpoints: &[(u16, u16)]) -> Vec<u8> {
        let mut d = vec![9, 2, 0, 0, 1, 1, 0, 0x80, 250];
        d.extend([9, 4, 1, 0, 0, CC_VIDEO, SC_VIDEOSTREAMING, 0, 0]);
        d.extend([11, CS_INTERFACE, VS_FORMAT_MJPEG, 1, sizes.len() as u8, 0, 1, 0, 0, 0, 0]);
        for (i, &(w, h)) in sizes.iter().enumerate() {
            let mut f = vec![0u8; 30];
            f[..4].copy_from_slice(&[30, CS_INTERFACE, VS_FRAME_MJPEG, i as u8 + 1]);
            f[5..7].copy_from_slice(&w.to_le_bytes());
            f[7..9].copy_from_slice(&h.to_le_bytes());
            f[21..25].copy_from_slice(&166_666u32.to_le_bytes());
            f[25] = 1;
            f[26..30].copy_from_slice(&166_666u32.to_le_bytes());
            d.extend(f);
        }
        for (alt, &(mult, size)) in endpoints.iter().enumerate() {
            d.extend([9, 4, 1, alt as u8 + 1, 1, CC_VIDEO, SC_VIDEOSTREAMING, 0, 0]);
            d.extend([7, 5, 0x81, 0x05]);
            d.extend((((mult - 1) << 11) | size).to_le_bytes());
            d.push(1);
        }
        let total = d.len() as u16;
        d[2..4].copy_from_slice(&total.to_le_bytes());
        d
    }

    /// The stereo module's MJPEG sizes, as its descriptors list them
    /// (2026-09-11).
    #[test]
    fn a_side_by_side_stereo_module_gets_480_lines_not_half_the_width() {
        let sizes = [(3840, 1080), (2560, 720), (1600, 600), (1280, 480), (640, 240)];
        let d = config(&sizes, &[(1, 640), (2, 800), (3, 1020)]);
        let config = nusb::descriptors::ConfigurationDescriptor::new(&d).unwrap();
        let info =
            find_mjpeg_stream(&config, 640, 480, TARGET_INTERVAL, MIN_ISO_BYTES_PER_MICROFRAME)
                .unwrap();
        assert_eq!((info.frame.width, info.frame.height), (1280, 480));
    }

    #[test]
    fn a_module_offering_the_exact_size_gets_it() {
        let d = config(&[(1280, 720), (800, 600), (640, 480), (320, 240)], &[(1, 1024), (2, 800)]);
        let config = nusb::descriptors::ConfigurationDescriptor::new(&d).unwrap();
        let info =
            find_mjpeg_stream(&config, 640, 480, TARGET_INTERVAL, MIN_ISO_BYTES_PER_MICROFRAME)
                .unwrap();
        assert_eq!((info.frame.width, info.frame.height), (640, 480));
    }

    /// `(subtype, index, sizes)` of one format descriptor and its frames.
    type Format<'a> = (u8, u8, &'a [(u16, u16)]);

    /// A VideoStreaming interface `number`: its alt 0 carries `formats`, and
    /// alt 1 one iso IN endpoint at `ep`.
    fn vs_interface(number: u8, formats: &[Format], ep: u8) -> Vec<u8> {
        let mut d = vec![9, 4, number, 0, 0, CC_VIDEO, SC_VIDEOSTREAMING, 0, 0];
        for &(subtype, index, sizes) in formats {
            d.extend([11, CS_INTERFACE, subtype, index, sizes.len() as u8, 0, 1, 0, 0, 0, 0]);
            for (i, &(w, h)) in sizes.iter().enumerate() {
                let mut f = vec![0u8; 30];
                f[..4].copy_from_slice(&[30, CS_INTERFACE, subtype + 1, i as u8 + 1]);
                f[5..7].copy_from_slice(&w.to_le_bytes());
                f[7..9].copy_from_slice(&h.to_le_bytes());
                f[21..25].copy_from_slice(&166_666u32.to_le_bytes());
                f[25] = 1;
                f[26..30].copy_from_slice(&166_666u32.to_le_bytes());
                d.extend(f);
            }
        }
        d.extend([9, 4, number, 1, 1, CC_VIDEO, SC_VIDEOSTREAMING, 0, 0]);
        d.extend([7, 5, ep, 0x05]);
        d.extend(((2u16 << 11) | 1024).to_le_bytes());
        d.push(1);
        d
    }

    fn configuration(interfaces: &[Vec<u8>]) -> Vec<u8> {
        let mut d = vec![9, 2, 0, 0, interfaces.len() as u8, 1, 0, 0x80, 250];
        interfaces.iter().for_each(|i| d.extend(i));
        let total = d.len() as u16;
        d[2..4].copy_from_slice(&total.to_le_bytes());
        d
    }

    const VS_FORMAT_UNCOMPRESSED: u8 = 0x04;

    #[test]
    fn the_stream_comes_from_the_interface_that_offers_mjpeg() {
        let d = configuration(&[
            vs_interface(1, &[(VS_FORMAT_UNCOMPRESSED, 1, &[(640, 480)])], 0x81),
            vs_interface(2, &[(VS_FORMAT_MJPEG, 3, &[(640, 480)])], 0x82),
        ]);
        let config = nusb::descriptors::ConfigurationDescriptor::new(&d).unwrap();
        let info =
            find_mjpeg_stream(&config, 640, 480, TARGET_INTERVAL, MIN_ISO_BYTES_PER_MICROFRAME)
                .unwrap();
        assert_eq!((info.vs_interface, info.format_index, info.ep_address), (2, 3, 0x82));
    }

    #[test]
    fn a_second_mjpeg_format_does_not_lend_its_frames_to_the_first() {
        let d = configuration(&[vs_interface(
            1,
            &[(VS_FORMAT_MJPEG, 1, &[(1280, 720)]), (VS_FORMAT_MJPEG, 2, &[(640, 480)])],
            0x81,
        )]);
        let config = nusb::descriptors::ConfigurationDescriptor::new(&d).unwrap();
        let info =
            find_mjpeg_stream(&config, 640, 480, TARGET_INTERVAL, MIN_ISO_BYTES_PER_MICROFRAME)
                .unwrap();
        assert_eq!((info.format_index, info.frame.width), (1, 1280));
    }
}
