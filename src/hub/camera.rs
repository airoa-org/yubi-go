//! The cameras: frames in, previews and live frames out, and which camera
//! fills which slot.
//!
//! A camera is known by its rank, the number its worker opened it under
//! (`camera_ports.json`). A slot is the role it plays: left wrist, right
//! wrist, head. `Roles` maps one to the other. Anything that describes the
//! camera itself is kept by rank, so it moves with the camera when the roles
//! change. Anything that describes the picture is kept by slot.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::{episodes, video, Hub, Ports, Rec, Roles, SourceStats, SOURCES, SRC_CAM};
use crate::misc::capture::Capture;
use crate::misc::clock::boottime_ns;

pub const SLOT_HEAD: usize = 2;
pub const USB_CAM_SLOTS: [usize; 3] = [0, 1, SLOT_HEAD];
pub const CAM_STRIP: [usize; USB_CAM_SLOTS.len()] = [0, SLOT_HEAD, 1];

pub type UiSnapshot = ([Option<(Arc<[u8]>, u64)>; 3], [Option<f32>; 2], [SourceStats; SOURCES]);

const PREVIEW_PERIOD: Duration = Duration::from_millis(100);

/// How long a stored preview stands for "this camera is delivering". Ten
/// preview periods: long enough that an ordinary hiccup is not a fault, short
/// enough that a stalled camera stops being offered before anyone watches it.
const PREVIEW_STALE: Duration = Duration::from_secs(1);

/// UVC pads a short payload with zeros, so the EOI marker is looked for
/// before them.
fn carries_eoi(jpeg: &[u8]) -> bool {
    let end = jpeg.iter().rposition(|&b| b != 0).unwrap_or(0);
    end > 0 && jpeg[end - 1] == 0xff && jpeg[end] == 0xd9
}

impl Hub {
    pub fn set_cam_name(&self, rank: usize, name: &str) {
        if let Some(cam) = self.state.lock().unwrap().cam_info.get_mut(rank) {
            name.clone_into(&mut cam.name);
        }
    }

    pub fn cam_names(&self) -> [String; 3] {
        let s = self.state.lock().unwrap();
        std::array::from_fn(|rank| match s.roles.slot(rank) {
            Some(_) => s.cam_info[rank].name.clone(),
            None => String::new(),
        })
    }

    pub fn on_frame(&self, rank: usize, capture: Capture, jpeg: &[u8]) {
        self.on_frame_at(rank, boottime_ns(), capture, Arc::from(jpeg))
    }

    pub fn on_frame_at(&self, rank: usize, at_ns: u64, capture: Capture, jpeg: Arc<[u8]>) {
        let whole = carries_eoi(&jpeg);
        let mut recorded = None;
        let mut recorded_hz = 0.0;
        let mut slots = [false; 3];
        {
            let mut s = self.state.lock().unwrap();
            for slot in 0..s.cams.len() {
                if s.roles.rank(slot) != Some(rank) {
                    continue;
                }
                slots[slot] = whole;
                recorded.get_or_insert(slot);
                if !whole {
                    continue;
                }
                let now = Instant::now();
                s.rates[SRC_CAM[slot]].tick(now);
                let hz = s.rates[SRC_CAM[slot]].hz();
                if let Some(info) = s.cam_info.get_mut(rank) {
                    info.peak = info.peak.max(hz);
                }
                recorded_hz = hz;
                if s.last_preview[slot].is_none_or(|t| t.elapsed() >= PREVIEW_PERIOD) {
                    s.last_preview[slot] = Some(now);
                    let cam = &mut s.cams[slot];
                    cam.frame = Some(jpeg.clone());
                    cam.seq += 1;
                }
            }
        }
        for (slot, _) in slots.iter().enumerate().filter(|(_, &whole)| whole) {
            let mut live = self.live[slot].lock().unwrap();
            *live = (Some(jpeg.clone()), live.1 + 1);
        }
        if let Some(slot) = recorded {
            if whole && self.capturing() {
                if self.h264.load(Ordering::Relaxed) {
                    let job = video::Job { at_ns, capture, jpeg: jpeg.clone(), hz: recorded_hz };
                    match self.video.push(slot, job) {
                        video::Push::Queued => return,
                        video::Push::Busy => {
                            self.state.lock().unwrap().rec.dropped += 1;
                            return;
                        }
                        video::Push::Unavailable => {}
                    }
                }
                self.record(Rec::Frame { at_ns, capture, slot, jpeg });
            }
        }
    }

    /// A camera's status, named by the slot it fills now rather than by rank;
    /// `suffix` tells the stream's from the controls'.
    pub fn set_cam_status(
        &self,
        rank: usize,
        suffix: &str,
        mut status: crate::misc::status::Status,
    ) {
        let slot = self.state.lock().unwrap().roles.slot(rank);
        let name = slot.and_then(|s| episodes::CAM_NAMES.get(s)).copied();
        status.name = format!("camera/{}{suffix}", name.unwrap_or("unassigned"));
        status.values.insert(0, ("rank".to_string(), rank.to_string()));
        if let Some(port) = self.cam_ports().get(rank).cloned().flatten() {
            status.hardware_id = port;
        }
        self.set_status(status);
    }

    pub fn ui_snapshot(&self, seen: [u64; 3]) -> UiSnapshot {
        let s = self.state.lock().unwrap();
        let cam = |i: usize| {
            let slot = USB_CAM_SLOTS[i];
            (s.cams[slot].seq > seen[i])
                .then(|| Some((s.cams[slot].frame.clone()?, s.cams[slot].seq)))
                .flatten()
        };
        ([cam(0), cam(1), cam(2)], s.sides, Self::collect_stats(&s))
    }

    /// The newest whole frame for one slot, if it is newer than `seen`.
    pub fn live_frame(&self, slot: usize, seen: u64) -> Option<(Arc<[u8]>, u64)> {
        let live = self.live.get(slot)?.lock().unwrap();
        (live.1 > seen).then(|| Some((live.0.clone()?, live.1))).flatten()
    }

    /// Whether a whole JPEG has arrived for this slot recently enough to send.
    ///
    /// A live rate is not the same question. The stalled wrist keeps ticking
    /// while its last whole frame goes stale after a hub replug, so a
    /// surface that trusted the rate would open an MJPEG connection that never
    /// delivers and draw a black rectangle.
    pub fn has_preview(&self, slot: usize) -> bool {
        let s = self.state.lock().unwrap();
        s.last_preview.get(slot).copied().flatten().is_some_and(|t| t.elapsed() < PREVIEW_STALE)
    }

    pub fn set_byte_rate(&self, rank: usize, mb_per_s: f32) {
        if let Some(cam) = self.state.lock().unwrap().cam_info.get_mut(rank) {
            cam.bytes = Some((mb_per_s, Instant::now()));
        }
    }

    pub fn set_cam_format(&self, rank: usize, width: u16, height: u16, nominal: f32, stereo: bool) {
        if let Some(cam) = self.state.lock().unwrap().cam_info.get_mut(rank) {
            cam.size = (width, height);
            cam.nominal = nominal;
            cam.peak = 0.0;
            cam.stereo = stereo;
        }
    }

    /// Whether the head slot holds the side-by-side stereo module.
    pub fn head_stereo(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.roles.rank(SLOT_HEAD).and_then(|rank| s.cam_info.get(rank)).is_some_and(|c| c.stereo)
    }

    pub fn set_video(&self, video: crate::misc::config::Video) {
        self.h264.store(video == crate::misc::config::Video::H264, Ordering::Relaxed);
    }

    pub fn video(&self) -> crate::misc::config::Video {
        match self.h264.load(Ordering::Relaxed) {
            true => crate::misc::config::Video::H264,
            false => crate::misc::config::Video::Jpeg,
        }
    }

    /// Only the Android encoder calls this.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub(crate) fn record_video(
        &self,
        slot: usize,
        at_ns: u64,
        capture: Capture,
        data: Vec<u8>,
        key_frame: bool,
    ) {
        self.record(Rec::Video { at_ns, capture, slot, data, key_frame });
    }

    pub(crate) fn request_key_frames(&self) {
        self.video.request_key_frames();
    }

    pub(crate) fn flush_video(&self) -> Vec<std::sync::mpsc::Receiver<()>> {
        self.video.flush()
    }

    pub fn roles(&self) -> Roles {
        self.state.lock().unwrap().roles
    }

    pub fn set_roles(&self, roles: Roles) {
        self.state.lock().unwrap().roles = roles;
    }

    /// A camera opened (`Some`) or closed (`None`) at `rank`.
    pub fn set_cam_port(&self, rank: usize, port: Option<String>) {
        let mut s = self.state.lock().unwrap();
        if let Some(p) = s.cam_ports.get_mut(rank) {
            *p = port;
        }
        if let Some(saved) = &s.saved_ports {
            s.roles = Roles::from_ports(saved, &s.cam_ports, !self.head_absent());
        }
    }

    pub fn cam_ports(&self) -> Ports {
        self.state.lock().unwrap().cam_ports.clone()
    }

    /// Follow the camera assignment `config.json` holds: by port when it has
    /// one, else the older rank list, which only holds until cameras open in
    /// another order.
    pub fn adopt_cameras(&self, config: &crate::misc::config::Config) {
        let mut s = self.state.lock().unwrap();
        s.saved_ports = config.camera_ports.clone();
        match (&s.saved_ports, config.camera_roles) {
            (Some(saved), _) => {
                s.roles = Roles::from_ports(saved, &s.cam_ports, config.head_camera)
            }
            (None, Some(roles)) => s.roles = roles,
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::config::Config;

    /// The stereo module opened at rank 0, which the first launch puts in the
    /// left slot, and was then assigned to the head: its details go with it.
    #[test]
    fn a_cameras_details_follow_it_to_its_new_slot() {
        let hub = Hub::default();
        hub.set_cam_port(0, Some("1-1.2".into()));
        hub.set_cam_port(1, Some("1-1.3".into()));
        hub.set_cam_port(2, Some("1-1.4".into()));
        hub.set_cam_format(0, 2560, 720, 30.0, true);
        hub.set_cam_format(2, 640, 480, 30.0, false);
        hub.set_cam_name(0, "stereo");
        assert!(!hub.head_stereo());
        assert_eq!(hub.stats()[SRC_CAM[0]].width, 2560);

        hub.set_roles(Roles::new([2, 1, 0]));
        assert!(hub.head_stereo(), "set_roles");
        assert_eq!(hub.stats()[SRC_CAM[SLOT_HEAD]].width, 2560);
        assert_eq!(hub.stats()[SRC_CAM[0]].width, 640);
        assert_eq!(hub.cam_names()[0], "stereo", "by rank, as before");

        let config = Config {
            camera_ports: Some([Some("1-1.4".into()), Some("1-1.3".into()), None]),
            ..Config::default()
        };
        hub.set_roles(Roles::default());
        hub.adopt_cameras(&config);
        assert_eq!(hub.roles().ranks(), [2, 1, 0]);
        assert!(hub.head_stereo(), "Roles::from_ports");
        hub.set_cam_port(2, None);
        hub.set_cam_port(2, Some("1-1.4".into()));
        assert!(hub.head_stereo(), "and again after a replug");
    }

    #[test]
    fn only_whole_frames_count_toward_a_cameras_rate() {
        let hub = Hub::default();
        let slot = hub.state.lock().unwrap().roles.slot(0).unwrap();
        hub.on_frame(0, Capture::default(), b"\xff\xd8\x00\x01");
        assert!(!hub.stats()[SRC_CAM[slot]].live, "a cut-short frame");
        hub.on_frame(0, Capture::default(), b"\xff\xd8\xff\xd9");
        assert!(hub.stats()[SRC_CAM[slot]].live, "a whole frame");
    }
}
