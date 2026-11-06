//! A finished take, read back for the `:8080` viewer: when each camera frame
//! was taken and where its bytes are, and what the grippers did.
//!
//! One linear pass over `signals.mcap`, reading record headers and seeking past
//! payloads it has no use for. A JPEG take is ~15 MB/s of images, and indexing
//! one must not mean reading them. Only H.264 frames are read whole, because a
//! frame's mp4 size and whether it is a key frame are inside it, and they are
//! a few kB each. This leans on `SignalsMcap` writing its chunks uncompressed;
//! a compressed chunk is skipped.
//!
//! The poses the 3D view draws come out too: the raw XR poses of the
//! controllers and the headset, which the shells are placed on, and from `/tf`
//! the hand roots and the finger links, which the jaws are placed on.

mod decode;

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use decode::{
    joint_state, joint_states, jpeg_frame, transforms, uvc_frame, video_frame, Cdr, Cursor,
    JPEG_HEAD, MAGIC, OP_CHANNEL, OP_CHUNK, OP_DATA_END, OP_FOOTER, OP_MESSAGE,
};

use crate::record::mcap::{
    CAM_NAMES, CAM_TOPICS, GRIPPER_TOPICS, POSE_TOPICS, UVC_TOPICS, VIDEO_TOPICS,
};
use crate::record::mp4;

/// The pose tracks the 3D view draws: the raw XR poses first, in
/// [`POSE_TOPICS`] order, then the `/tf` children in [`TF_CHILDREN`] order.
pub const TRACKS: [&str; 9] = [
    "left_controller",
    "right_controller",
    "hmd",
    "left_hand_root",
    "right_hand_root",
    "left_left_finger_link",
    "left_right_finger_link",
    "right_left_finger_link",
    "right_right_finger_link",
];
const TF_CHILDREN: std::ops::Range<usize> = 3..9;

/// The `tracked` flag of each raw XR pose, in [`POSE_TOPICS`] order. It is
/// the stricter of the two: `valid` stays true while the runtime only
/// extrapolates, and `tracked` does not.
const TRACKED_TOPICS: [&str; 3] =
    ["/quest/controller/left/tracked", "/quest/controller/right/tracked", "/quest/hmd/tracked"];

/// The least time between two kept pose samples. The view interpolates
/// between them, and 30 Hz keeps a five-minute take's poses near 5 MB of JSON
/// where the recorded rate (the XR frame rate, 72-120 Hz) would be 20 MB.
const POSE_STEP_NS: u64 = 33_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Codec {
    Jpeg,
    H264,
}

/// Where one frame's image bytes are in the file.
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    /// The header stamp: when the camera took it, on the file's clock.
    pub t_ns: u64,
    pub offset: u64,
    pub len: u32,
    /// H.264 only: the frame's size as an mp4 sample.
    pub sample: u32,
    pub key: bool,
}

/// An H.264 stream's SPS and PPS.
type ParameterSets = (Vec<u8>, Vec<u8>);

pub struct Camera {
    pub codec: Codec,
    pub frames: Vec<Frame>,
    /// H.264 only: the first key frame's parameter sets.
    sets: Option<ParameterSets>,
}

pub struct Index {
    path: PathBuf,
    pub cameras: [Option<Camera>; 3],
    /// `(stamp, position)` per side, on the gripper's own topic.
    pub grippers: [Vec<(u64, f64)>; 2],
    /// `(stamp, [x, y, z, qx, qy, qz, qw])` per [`TRACKS`] entry. The `/tf`
    /// children are relative to their parent as recorded: the finger links to
    /// their hand root, the hand roots to `quest_origin`.
    pub poses: [Vec<(u64, [f64; 7])>; 9],
    /// `/joint_states` while both grippers are calibrated, in
    /// [`GRIPPER_JOINTS`] order.
    pub joints: Vec<(u64, [f64; 2])>,
    /// `(from, to)` stamps of each span a raw XR pose was not tracked, in
    /// [`POSE_TOPICS`] order. A span still open at the end stops at its last
    /// sample.
    pub untracked: [Vec<(u64, u64)>; 3],
    /// `(stamp, pts, first packet reaped)` of each frame whose camera sent a
    /// presentation time, per camera slot. The pts is the camera's own clock
    /// at exposure.
    pub uvc: [Vec<(u64, u32, u64)>; 3],
    /// The span each part is in now: its start and its latest sample.
    open: [Option<(u64, u64)>; 3],
    /// The least time between two kept pose samples.
    step_ns: u64,
}

#[derive(Clone, Copy)]
enum Role {
    Camera(usize, Codec),
    Uvc(usize),
    Gripper(usize),
    Pose(usize),
    Tracked(usize),
    Tf,
    Joints,
}

fn role(topic: &str) -> Option<Role> {
    let at = |topics: &[&str]| topics.iter().position(|t| *t == topic);
    if let Some(slot) = at(&CAM_TOPICS) {
        return Some(Role::Camera(slot, Codec::Jpeg));
    }
    if let Some(slot) = at(&VIDEO_TOPICS) {
        return Some(Role::Camera(slot, Codec::H264));
    }
    if let Some(slot) = at(&UVC_TOPICS) {
        return Some(Role::Uvc(slot));
    }
    if let Some(part) = at(&POSE_TOPICS) {
        return Some(Role::Pose(part));
    }
    if let Some(part) = at(&TRACKED_TOPICS) {
        return Some(Role::Tracked(part));
    }
    if topic == "/tf" {
        return Some(Role::Tf);
    }
    if topic == "/joint_states" {
        return Some(Role::Joints);
    }
    at(&GRIPPER_TOPICS).map(Role::Gripper)
}

impl Index {
    pub fn read(path: &Path) -> io::Result<Index> {
        Index::read_with(path, POSE_STEP_NS)
    }

    /// Every pose sample, for the export, which resamples them itself.
    pub fn read_all(path: &Path) -> io::Result<Index> {
        Index::read_with(path, 0)
    }

    fn read_with(path: &Path, step_ns: u64) -> io::Result<Index> {
        let mut c = Cursor { r: BufReader::with_capacity(64 * 1024, File::open(path)?), pos: 0 };
        if c.bytes(8)? != MAGIC {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "not an mcap file"));
        }
        let mut index = Index {
            path: path.to_path_buf(),
            cameras: Default::default(),
            grippers: Default::default(),
            poses: Default::default(),
            joints: Vec::new(),
            untracked: Default::default(),
            uvc: Default::default(),
            open: Default::default(),
            step_ns,
        };
        let mut roles = std::collections::HashMap::new();
        while let Ok(op) = c.u8() {
            let len = c.u64()?;
            match op {
                OP_CHUNK => {
                    c.skip(8 + 8 + 8 + 4)?;
                    let n = u32::from_le_bytes(c.bytes(4)?.try_into().unwrap()) as usize;
                    let compressed = n > 0;
                    c.skip(n as u64)?;
                    let records = c.u64()?;
                    if compressed {
                        c.skip(records)?;
                        continue;
                    }
                    let end = c.pos + records;
                    while c.pos < end {
                        let op = c.u8()?;
                        let len = c.u64()?;
                        index.record(&mut c, &mut roles, op, len)?;
                    }
                }
                OP_DATA_END | OP_FOOTER => break,
                _ => index.record(&mut c, &mut roles, op, len)?,
            }
        }
        for (spans, open) in index.untracked.iter_mut().zip(&mut index.open) {
            spans.extend(open.take());
        }
        for camera in index.cameras.iter_mut().flatten() {
            // A player starts at a key frame, so whatever came before the first
            // one cannot be shown.
            if camera.codec == Codec::H264 {
                let first = camera.frames.iter().position(|f| f.key).unwrap_or(camera.frames.len());
                camera.frames.drain(..first);
            }
        }
        Ok(index)
    }

    fn record(
        &mut self,
        c: &mut Cursor,
        roles: &mut std::collections::HashMap<u16, Role>,
        op: u8,
        len: u64,
    ) -> io::Result<()> {
        match op {
            OP_CHANNEL => {
                let body = c.bytes(len as usize)?;
                let topic = body
                    .get(4..8)
                    .and_then(|n| body.get(8..8 + u32::from_le_bytes(n.try_into().ok()?) as usize))
                    .and_then(|t| std::str::from_utf8(t).ok());
                if let Some(r) = topic.and_then(role) {
                    roles.insert(u16::from_le_bytes([body[0], body[1]]), r);
                }
                Ok(())
            }
            // channel u16, sequence u32, log time u64, publish time u64
            OP_MESSAGE if len >= 22 => {
                let head = c.bytes(22)?;
                let role = roles.get(&u16::from_le_bytes([head[0], head[1]])).copied();
                self.message(c, role, &head, len - 22)
            }
            _ => c.skip(len),
        }
    }

    /// One message's payload, `payload` bytes from the cursor, read as its
    /// channel's `role` asks.
    fn message(
        &mut self,
        c: &mut Cursor,
        role: Option<Role>,
        head: &[u8],
        payload: u64,
    ) -> io::Result<()> {
        let at = c.pos;
        match role {
            Some(Role::Camera(slot, Codec::Jpeg)) => {
                let read = payload.min(JPEG_HEAD);
                let fields = c.bytes(read as usize)?;
                c.skip(payload - read)?;
                if let Some(frame) = jpeg_frame(&fields, at) {
                    self.camera(slot, Codec::Jpeg).frames.push(frame);
                }
            }
            Some(Role::Camera(slot, Codec::H264)) => {
                let body = c.bytes(payload as usize)?;
                let camera = self.camera(slot, Codec::H264);
                if let Some((frame, sets)) = video_frame(&body, at) {
                    if camera.sets.is_none() && frame.key {
                        camera.sets = sets;
                    }
                    camera.frames.push(frame);
                }
            }
            Some(Role::Uvc(slot)) => {
                let body = c.bytes(payload as usize)?;
                if let Some(point) = uvc_frame(&body) {
                    self.uvc[slot].push(point);
                }
            }
            Some(Role::Gripper(side)) => {
                let body = c.bytes(payload as usize)?;
                if let Some(point) = joint_state(&body) {
                    self.grippers[side].push(point);
                }
            }
            Some(Role::Pose(part)) => {
                let body = c.bytes(payload as usize)?;
                let mut r = Cdr::new(&body);
                let sample = (|| {
                    let t_ns = r.stamp()?;
                    r.text()?;
                    Some((t_ns, r.pose()?))
                })();
                if let Some((t_ns, pose)) = sample {
                    self.keep(part, t_ns, pose);
                }
            }
            // A Bool has no header, so its stamp is the publish time,
            // which `SignalsMcap::pose` sets to the pose's own stamp.
            Some(Role::Tracked(part)) => {
                let body = c.bytes(payload as usize)?;
                let t_ns = u64::from_le_bytes(head[14..22].try_into().unwrap());
                if let Some(&tracked) = body.get(4) {
                    match (tracked != 0, self.open[part]) {
                        (false, None) => self.open[part] = Some((t_ns, t_ns)),
                        (false, Some((from, _))) => self.open[part] = Some((from, t_ns)),
                        (true, Some((from, _))) => {
                            self.untracked[part].push((from, t_ns));
                            self.open[part] = None;
                        }
                        (true, None) => {}
                    }
                }
            }
            Some(Role::Joints) => {
                let body = c.bytes(payload as usize)?;
                if let Some(point) = joint_states(&body) {
                    self.joints.push(point);
                }
            }
            Some(Role::Tf) => {
                let body = c.bytes(payload as usize)?;
                for (child, t_ns, pose) in transforms(&body) {
                    if let Some(k) = TRACKS[TF_CHILDREN].iter().position(|n| *n == child) {
                        self.keep(TF_CHILDREN.start + k, t_ns, pose);
                    }
                }
            }
            None => c.skip(payload)?,
        }
        Ok(())
    }

    /// One pose sample, unless it is under the step after the last
    /// kept, or half lost: `SignalsMcap::pose` writes NaN for the half that
    /// is not valid, and JSON has no NaN.
    fn keep(&mut self, track: usize, t_ns: u64, pose: [f64; 7]) {
        let track = &mut self.poses[track];
        if pose.iter().any(|v| !v.is_finite()) {
            return;
        }
        if track.last().is_some_and(|&(last, _)| t_ns < last + self.step_ns) {
            return;
        }
        track.push((t_ns, pose));
    }

    fn camera(&mut self, slot: usize, codec: Codec) -> &mut Camera {
        self.cameras[slot].get_or_insert_with(|| Camera { codec, frames: Vec::new(), sets: None })
    }

    /// The first gripper or pose stamp, which the view's times count from.
    /// A camera only when there is neither: a video opens on the key frame
    /// before the take's start (`writer::PreRoll`), up to a key interval
    /// early, and the view would start on that much of nothing else.
    fn origin(&self) -> u64 {
        let cams = self.cameras.iter().flatten().filter_map(|c| c.frames.first()).map(|f| f.t_ns);
        let grips = self.grippers.iter().filter_map(|g| g.first()).map(|p| p.0);
        let poses = self.poses.iter().filter_map(|p| p.first()).map(|p| p.0);
        grips.chain(poses).min().or_else(|| cams.min()).unwrap_or(0)
    }

    /// What the page plays from: times in milliseconds from the first stamp,
    /// and the wall-clock origin, so `meta.json`'s segments can be placed.
    pub fn view(&self) -> serde_json::Value {
        let origin = self.origin();
        let ms = |t_ns: u64| ((t_ns as i64 - origin as i64) as f64 / 1e5).round() / 10.0;
        let cameras: Vec<serde_json::Value> = self
            .cameras
            .iter()
            .enumerate()
            .filter_map(|(slot, c)| Some((slot, c.as_ref().filter(|c| !c.frames.is_empty())?)))
            .map(|(slot, c)| {
                let codec = match c.codec {
                    Codec::Jpeg => "jpeg",
                    Codec::H264 => "h264",
                };
                let mut cam = serde_json::json!({
                    "name": CAM_NAMES[slot],
                    "codec": codec,
                    "t_ms": c.frames.iter().map(|f| ms(f.t_ns)).collect::<Vec<_>>(),
                });
                // So the page can size the tile before the video loads.
                if let Some((w, h)) = c.sets.as_ref().and_then(|(sps, _)| mp4::dimensions(sps)) {
                    cam["width"] = w.into();
                    cam["height"] = h.into();
                }
                cam
            })
            .collect();
        let grippers: Vec<serde_json::Value> = self
            .grippers
            .iter()
            .enumerate()
            .filter(|(_, g)| !g.is_empty())
            // Every sample, because the page reads out the value at the
            // playhead: 200 lines/s is 12,000 points a minute.
            .map(|(side, g)| {
                let name = ["left", "right"][side];
                serde_json::json!({
                    "name": name,
                    "t_ms": g.iter().map(|p| ms(p.0)).collect::<Vec<_>>(),
                    "position": g.iter().map(|p| (p.1 * 1e4).round() / 1e4).collect::<Vec<_>>(),
                })
            })
            .collect();
        let round = |v: f64| (v * 1e4).round() / 1e4;
        let poses: serde_json::Map<String, serde_json::Value> = TRACKS
            .iter()
            .zip(&self.poses)
            .filter(|(_, track)| !track.is_empty())
            .map(|(name, track)| {
                let t: Vec<f64> = track.iter().map(|p| ms(p.0)).collect();
                let pose: Vec<f64> = track.iter().flat_map(|p| p.1.map(round)).collect();
                (name.to_string(), serde_json::json!({"t_ms": t, "pose": pose}))
            })
            .collect();
        let untracked: serde_json::Map<String, serde_json::Value> = TRACKS[..3]
            .iter()
            .zip(&self.untracked)
            .map(|(name, spans)| {
                let spans: Vec<[f64; 2]> = spans.iter().map(|&(a, b)| [ms(a), ms(b)]).collect();
                (name.to_string(), serde_json::json!(spans))
            })
            .collect();
        serde_json::json!({
            "origin_ns": origin.to_string(),
            "cameras": cameras,
            "grippers": grippers,
            "poses": poses,
            "untracked": untracked,
        })
    }

    /// One frame's bytes as recorded: a JPEG, or an H.264 access unit.
    pub fn bytes(&self, slot: usize, i: usize) -> Option<io::Result<Vec<u8>>> {
        let frame = self.cameras.get(slot)?.as_ref()?.frames.get(i)?;
        Some(read_at(&self.path, frame.offset, frame.len as usize))
    }

    /// One JPEG frame's bytes.
    pub fn jpeg(&self, slot: usize, i: usize) -> Option<io::Result<Vec<u8>>> {
        let camera = self.cameras.get(slot)?.as_ref().filter(|c| c.codec == Codec::Jpeg)?;
        let frame = camera.frames.get(i)?;
        Some(read_at(&self.path, frame.offset, frame.len as usize))
    }

    /// One H.264 camera as an mp4 the page can range over.
    pub fn video(&self, slot: usize) -> Option<Video> {
        let camera = self.cameras.get(slot)?.as_ref().filter(|c| c.codec == Codec::H264)?;
        let (sps, pps) = camera.sets.as_ref()?;
        let samples: Vec<mp4::Sample> = camera
            .frames
            .iter()
            .map(|f| mp4::Sample { t_ns: f.t_ns, size: f.sample, key: f.key })
            .collect();
        let head = mp4::header(&samples, sps, pps)?;
        let mut ends = Vec::with_capacity(camera.frames.len());
        let mut end = head.len() as u64;
        for f in &camera.frames {
            end += f.sample as u64;
            ends.push(end);
        }
        Some(Video { path: self.path.clone(), head, frames: camera.frames.clone(), ends })
    }
}

fn read_at(path: &Path, offset: u64, len: usize) -> io::Result<Vec<u8>> {
    let mut f = File::open(path)?;
    f.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0; len];
    f.read_exact(&mut buf)?;
    Ok(buf)
}

/// A camera's mp4, cut to byte ranges on request.
pub struct Video {
    path: PathBuf,
    head: Vec<u8>,
    frames: Vec<Frame>,
    /// Where each sample ends in the mp4.
    ends: Vec<u64>,
}

impl Video {
    // An mp4 always holds its header, so it is never empty.
    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u64 {
        self.ends.last().copied().unwrap_or(self.head.len() as u64)
    }

    /// Bytes `start..end` of the mp4, written to `out`.
    pub fn write(&self, start: u64, end: u64, out: &mut impl io::Write) -> io::Result<()> {
        let head = self.head.len() as u64;
        if start < head {
            out.write_all(&self.head[start as usize..end.min(head) as usize])?;
        }
        let mut file = File::open(&self.path)?;
        let first = self.ends.partition_point(|&e| e <= start.max(head));
        for (i, frame) in self.frames.iter().enumerate().skip(first) {
            let begin = self.ends[i] - frame.sample as u64;
            if begin >= end {
                break;
            }
            file.seek(SeekFrom::Start(frame.offset))?;
            let mut annex_b = vec![0; frame.len as usize];
            file.read_exact(&mut annex_b)?;
            let sample = mp4::avcc(&annex_b);
            if sample.len() != frame.sample as usize {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "frame changed on disk"));
            }
            let from = start.saturating_sub(begin) as usize;
            let to = (end.min(self.ends[i]) - begin) as usize;
            out.write_all(&sample[from..to])?;
        }
        Ok(())
    }
}

/// The last index read. The page asks for the view and then each camera's
/// frames in parallel, all of one take, so one entry saves every reread. The
/// lock is held while reading, so those parallel requests wait for one pass
/// rather than each making their own.
static LAST: Mutex<Option<(PathBuf, u64, Arc<Index>)>> = Mutex::new(None);

/// The index of one `signals.mcap`, reread when the file's length changes.
pub fn index(path: &Path) -> io::Result<Arc<Index>> {
    let len = std::fs::metadata(path)?.len();
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((p, l, index)) = last.as_ref() {
        if p == path && *l == len {
            return Ok(index.clone());
        }
    }
    let index = Arc::new(Index::read(path)?);
    *last = Some((path.to_path_buf(), len, index.clone()));
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::capture::Capture;
    use crate::record::mcap::SignalsMcap;

    fn path(name: &str) -> PathBuf {
        let path = std::env::temp_dir()
            .join(format!("yubigo_playback_{name}-{}.mcap", std::process::id()));
        let _ = std::fs::remove_file(&path);
        path
    }

    fn at(t_ns: u64) -> Capture {
        Capture { at_ns: Some(t_ns), ..Capture::default() }
    }

    #[test]
    fn jpeg_frames_and_grippers_index_without_reading_the_images() {
        let path = path("jpeg");
        let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
        out.gripper(1_000_000, None, 0, 1.4, Some(0.5)).unwrap();
        out.frame(2_000_000, at(1_500_000), 0, b"\xff\xd8one\xff\xd9").unwrap();
        out.frame(3_000_000, at(2_500_000), 2, b"\xff\xd8head\xff\xd9").unwrap();
        out.frame(4_000_000, at(3_500_000), 0, b"\xff\xd8two\xff\xd9").unwrap();
        out.gripper(5_000_000, None, 1, 1.0, None).unwrap();
        out.close().unwrap();

        let index = Index::read(&path).unwrap();
        let left = index.cameras[0].as_ref().unwrap();
        assert_eq!(left.codec, Codec::Jpeg);
        assert_eq!(left.frames.iter().map(|f| f.t_ns).collect::<Vec<_>>(), [1_500_000, 3_500_000]);
        assert!(index.cameras[1].is_none());
        assert_eq!(index.jpeg(0, 1).unwrap().unwrap(), b"\xff\xd8two\xff\xd9");
        assert_eq!(index.jpeg(2, 0).unwrap().unwrap(), b"\xff\xd8head\xff\xd9");
        assert!(index.jpeg(0, 2).is_none());
        assert_eq!(index.grippers[0], [(1_000_000, 0.5)]);
        assert_eq!(index.grippers[1], [(5_000_000, 1.0)], "uncalibrated: the reading itself");

        let view = index.view();
        assert_eq!(view["origin_ns"], "1000000");
        assert_eq!(view["cameras"][0]["t_ms"], serde_json::json!([0.5, 2.5]));
        assert_eq!(view["cameras"][1]["name"], "head");
    }

    #[test]
    fn untracked_spans_run_from_the_first_untracked_tick_to_the_next_tracked_one() {
        use crate::misc::pose::Located;
        let path = path("untracked");
        let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
        out.gripper(0, None, 0, 1.4, Some(0.3)).unwrap();
        let pose = [0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
        let ticks = [
            (Located::tracked(pose), Located::tracked(pose)),
            (Located::inferred(pose), Located::tracked(pose)),
            (Located::LOST, Located::tracked(pose)),
            (Located::tracked(pose), Located::inferred(pose)),
            (Located::tracked(pose), Located::inferred(pose)),
        ];
        for (k, (left, hmd)) in ticks.into_iter().enumerate() {
            let t = k as u64 * 10_000_000;
            out.pose(t, at(t), 0, left).unwrap();
            out.pose(t, at(t), 2, hmd).unwrap();
        }
        out.close().unwrap();

        let index = Index::read(&path).unwrap();
        assert_eq!(index.untracked[0], [(10_000_000, 30_000_000)], "inferred and lost alike");
        assert_eq!(index.untracked[2], [(30_000_000, 40_000_000)], "open at the end");
        assert!(index.untracked[1].is_empty());
        assert_eq!(index.view()["untracked"]["hmd"], serde_json::json!([[30.0, 40.0]]));
    }

    #[test]
    fn a_video_opening_before_the_signals_counts_from_before_zero() {
        let path = path("lead_in");
        let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
        out.frame(1_000_000, at(1_000_000), 2, b"\xff\xd8key\xff\xd9").unwrap();
        out.gripper(3_000_000, None, 0, 1.4, Some(0.5)).unwrap();
        out.close().unwrap();
        let view = Index::read(&path).unwrap().view();
        assert_eq!(view["origin_ns"], "3000000");
        assert_eq!(view["cameras"][0]["t_ms"], serde_json::json!([-2.0]));
    }

    #[test]
    fn poses_come_from_the_raw_topics_and_tf_thinned_to_the_view_rate() {
        use crate::misc::pose::Located;
        let path = path("poses");
        let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
        out.gripper(0, None, 0, 1.4, Some(0.3)).unwrap();
        let pose = [0.3, -0.2, 1.1, 0.0, 0.3826834, 0.0, 0.9238795];
        for (k, t) in [0u64, 10_000_000, 40_000_000].into_iter().enumerate() {
            let mut p = pose;
            p[0] += k as f64;
            out.pose(t, at(t), 0, Located::tracked(p)).unwrap();
            out.pose(t, at(t), 2, Located::tracked(p)).unwrap();
        }
        out.close().unwrap();

        let index = Index::read(&path).unwrap();
        let track = |name: &str| &index.poses[TRACKS.iter().position(|n| *n == name).unwrap()];
        let xs = |name: &str| track(name).iter().map(|p| (p.0, p.1[0])).collect::<Vec<_>>();
        assert_eq!(xs("left_controller"), [(0, 0.3), (40_000_000, 2.3)], "10 ms is under the step");
        assert_eq!(xs("hmd").len(), 2);
        let root = crate::misc::mount::hand_root_pose(0, pose);
        assert!((track("left_hand_root")[0].1[0] - root[0]).abs() < 1e-12);
        assert_eq!(track("left_left_finger_link").len(), 2, "calibrated, so the jaws are in /tf");
        assert!(track("right_hand_root").is_empty());
        assert_eq!(index.view()["poses"]["hmd"]["pose"].as_array().unwrap().len(), 14);
    }

    #[test]
    fn the_export_reads_every_pose_and_both_calibrated_joints() {
        use crate::misc::pose::Located;
        let path = path("export");
        let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
        out.gripper(1_000, None, 0, 1.4, Some(0.3)).unwrap();
        out.gripper(2_000, None, 1, 1.0, Some(0.6)).unwrap();
        out.gripper(3_000, None, 0, 1.4, Some(0.4)).unwrap();
        let pose = [0.3, -0.2, 1.1, 0.0, 0.0, 0.0, 1.0];
        for t in [0u64, 10_000_000, 20_000_000] {
            out.pose(t, at(t), 0, Located::tracked(pose)).unwrap();
        }
        out.close().unwrap();

        let index = Index::read_all(&path).unwrap();
        assert_eq!(index.poses[0].len(), 3, "nothing thinned");
        let pairs = [(2_000, [0.3, 0.6]), (3_000, [0.4, 0.6])];
        assert_eq!(index.joints, pairs, "one side alone is not a pair");
        assert_eq!(Index::read(&path).unwrap().poses[0].len(), 1);
    }

    fn annex_b(units: &[&[u8]]) -> Vec<u8> {
        units.iter().flat_map(|u| [&[0, 0, 0, 1][..], u].concat()).collect()
    }

    #[test]
    fn h264_frames_play_from_the_first_key_frame_as_one_mp4() {
        let sps = crate::record::mp4::tests::SPS_640X480;
        let pps: &[u8] = &[0x68, 0xee, 0x3c, 0x80];
        let path = path("h264");
        let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
        out.video(1_000, at(1_000), 1, &annex_b(&[&[0x41, 9, 9]])).unwrap();
        out.video(2_000, at(2_000), 1, &annex_b(&[sps, pps, &[0x65, 1, 2, 3]])).unwrap();
        out.video(3_000, at(3_000), 1, &annex_b(&[&[0x41, 4, 5]])).unwrap();
        out.close().unwrap();

        let index = Index::read(&path).unwrap();
        let right = index.cameras[1].as_ref().unwrap();
        assert_eq!(right.frames.len(), 2, "the frame before the key frame is unplayable");
        assert!(right.frames[0].key && !right.frames[1].key);
        let view = index.view();
        assert_eq!(
            (view["cameras"][0]["width"].clone(), view["cameras"][0]["height"].clone()),
            (640.into(), 480.into())
        );

        let video = index.video(1).unwrap();
        let mut whole = Vec::new();
        video.write(0, video.len(), &mut whole).unwrap();
        assert_eq!(whole.len() as u64, video.len());
        assert_eq!(
            &whole[video.head.len()..],
            &[0, 0, 0, 4, 0x65, 1, 2, 3, 0, 0, 0, 3, 0x41, 4, 5]
        );
        // Any range is the same bytes as the whole, cut.
        for (a, b) in [
            (0, 10),
            (video.head.len() as u64 - 2, video.len() - 3),
            (video.len() - 1, video.len()),
        ] {
            let mut part = Vec::new();
            video.write(a, b, &mut part).unwrap();
            assert_eq!(part, whole[a as usize..b as usize], "{a}..{b}");
        }
        assert!(index.jpeg(1, 0).is_none());
    }
}
