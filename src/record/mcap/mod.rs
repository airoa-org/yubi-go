//! `signals.mcap`, the one file a take records: every topic with its schema
//! and QoS, and the close check that no source's stamps went backwards.
//! `msg` holds the message definitions and the CDR structs that carry them.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;

use crate::misc::capture::Capture;
use crate::misc::pose::Located;

use msg::*;

mod msg;
#[cfg(test)]
mod tests;

pub const CAM_NAMES: [&str; 3] = ["left", "right", "head"];

pub(crate) const CAM_TOPICS: [&str; 3] = [
    "/left_camera/image_raw/compressed",
    "/right_camera/image_raw/compressed",
    "/head_camera/image_raw/compressed",
];

/// The `h264` setting's topics, in place of [`CAM_TOPICS`].
pub(crate) const VIDEO_TOPICS: [&str; 3] =
    ["/left_camera/video", "/right_camera/video", "/head_camera/video"];

const CAM_FRAMES: [&str; 3] = ["left_hand_cam_link", "right_hand_cam_link", "quest_hmd"];

pub(crate) const UVC_TOPICS: [&str; 3] =
    ["/left_camera/uvc_frame", "/right_camera/uvc_frame", "/head_camera/uvc_frame"];

/// The raw OpenXR poses and velocities, per part (left, right, hmd), in
/// `quest_origin`: `/tf` carries the controllers only as the mounted hand root.
pub(crate) const POSE_TOPICS: [&str; 3] =
    ["/quest/controller/left/pose", "/quest/controller/right/pose", "/quest/hmd/pose"];
const VELOCITY_TOPICS: [&str; 3] =
    ["/quest/controller/left/velocity", "/quest/controller/right/velocity", "/quest/hmd/velocity"];
const JOY_TOPICS: [&str; 2] = ["/quest/controller/left/joy", "/quest/controller/right/joy"];
const INPUT_CHANGE_TOPICS: [&str; 2] =
    ["/quest/controller/left/input_changes", "/quest/controller/right/input_changes"];
const QUEST_ORIGIN: &str = "quest_origin";

/// What each `Joy` index means, latched once so the file says it itself.
pub const JOY_AXES: [&str; 4] = ["trigger", "squeeze", "thumbstick_x", "thumbstick_y"];
pub const JOY_BUTTONS: [&str; 9] = [
    "lower_click",
    "upper_click",
    "thumbstick_click",
    "menu_click",
    "lower_touch",
    "upper_touch",
    "trigger_touch",
    "thumbstick_touch",
    "thumbrest_touch",
];

const TIMELINES: [&str; 8] = [
    "left camera",
    "right camera",
    "head camera",
    "left controller",
    "right controller",
    "hmd",
    "left gripper",
    "right gripper",
];

/// The topics and joint names of the YUBI ROS stack (yubi-sw), from
/// yubi_bringup/config/common
/// (`record_topics`, and `yubi_devices.yaml`'s encoder joint names). A rig that
/// spells these differently is a second dialect for everything downstream.
pub(crate) const GRIPPER_TOPICS: [&str; 2] =
    ["/yubi/gripper/left/joint_state", "/yubi/gripper/right/joint_state"];
pub(crate) const GRIPPER_JOINTS: [&str; 2] =
    ["left_right_finger_joint", "right_right_finger_joint"];
/// What the encoder said, every line: the YUBI ROS stack's topics carry the
/// calibrated angle once there is one, clamped to the joint's range, and the
/// raw reading cannot be recovered from it.
const RAW_GRIPPER_TOPICS: [&str; 2] = ["/yubi_go/gripper/left/raw", "/yubi_go/gripper/right/raw"];

const CAM_TIMELINE: usize = 0;
const POSE_TIMELINE: usize = 3;
const GRIPPER_TIMELINE: usize = 6;

#[derive(Default)]
struct Order {
    last_ns: [Option<u64>; TIMELINES.len()],
    rows: [u64; TIMELINES.len()],
    backwards: [u64; TIMELINES.len()],
}

impl Order {
    fn note(&mut self, source: usize, order_ns: u64) {
        let Some(last) = self.last_ns.get_mut(source) else { return };
        if last.is_some_and(|last| order_ns < last) {
            self.backwards[source] += 1;
        }
        *last = Some(order_ns);
        self.rows[source] += 1;
    }
}

/// Reads one Bool flag off a pose.
type Flag = fn(Located) -> bool;

/// Every Bool topic a pose tick writes, per part (left, right, hmd), with the
/// value each one carries. yubi has `tracked` on all three and `valid` on the
/// controllers; the position / orientation pairs are YUBI Go's own, and the
/// hmd gets them without a combined `valid`, which yubi never had.
fn flag_topics() -> [Vec<(String, Flag)>; 3] {
    let controller = |side: &str| -> Vec<(String, Flag)> {
        let topic = |leaf: &str| format!("/quest/controller/{side}/{leaf}");
        vec![
            (topic("tracked"), Located::is_tracked),
            (topic("valid"), Located::is_valid),
            (topic("position/valid"), |l| l.position.is_valid()),
            (topic("position/tracked"), |l| l.position.is_tracked()),
            (topic("orientation/valid"), |l| l.orientation.is_valid()),
            (topic("orientation/tracked"), |l| l.orientation.is_tracked()),
        ]
    };
    [
        controller("left"),
        controller("right"),
        vec![
            ("/quest/hmd/tracked".to_string(), Located::is_tracked),
            ("/quest/hmd/position/valid".to_string(), |l| l.position.is_valid()),
            ("/quest/hmd/position/tracked".to_string(), |l| l.position.is_tracked()),
            ("/quest/hmd/orientation/valid".to_string(), |l| l.orientation.is_valid()),
            ("/quest/hmd/orientation/tracked".to_string(), |l| l.orientation.is_tracked()),
        ],
    ]
}

pub struct SignalsMcap {
    writer: mcap::Writer<BufWriter<File>>,
    order: Order,
    tf: u16,
    tf_static: u16,
    joints: u16,
    raw: [u16; 2],
    encoder: [u16; 2],
    flags: [Vec<(u16, Flag)>; 3],
    /// Created on a camera's first frame, so a take holds only the format
    /// it recorded.
    cams: [Option<u16>; 3],
    videos: [Option<u16>; 3],
    uvc: [u16; 3],
    poses: [u16; 3],
    velocities: [u16; 3],
    joys: [u16; 2],
    input_changes: [u16; 2],
    diagnostics: u16,
    events: u16,
    rosout: u16,
    expected: [bool; 3],
    angle: [Option<f64>; 2],
    /// Written ahead of the first message and stamped like it: `/tf_static`
    /// is latched, so a reader sees it whenever it joins. Anything else that
    /// is true before the take begins goes out the same way.
    statics: Option<Vec<crate::misc::mount::Edge>>,
}

impl SignalsMcap {
    pub fn create(path: &Path, expected: [bool; 3]) -> io::Result<SignalsMcap> {
        let options = mcap::WriteOptions::new().compression(None).profile("ros2");
        let mut writer = mcap(options.create(BufWriter::new(File::create(path)?)))?;
        let mut channel_with =
            |topic: &str, schema_name: &str, schema: &str, latched: bool| -> io::Result<u16> {
                let id = mcap(writer.add_schema(schema_name, "ros2msg", schema.as_bytes()))?;
                let metadata = match latched {
                    true => [("offered_qos_profiles".to_string(), LATCHED_QOS.to_string())].into(),
                    false => Default::default(),
                };
                mcap(writer.add_channel(id, topic, "cdr", &metadata))
            };
        let tf_schema = schema(TF_MESSAGE, &TF_DEPS);
        let tf = channel_with("/tf", "tf2_msgs/msg/TFMessage", &tf_schema, false)?;
        let tf_static = channel_with("/tf_static", "tf2_msgs/msg/TFMessage", &tf_schema, true)?;
        let mut channel = |topic: &str, schema_name: &str, schema: &str| {
            channel_with(topic, schema_name, schema, false)
        };
        let joints =
            channel("/joint_states", "sensor_msgs/msg/JointState", &schema(JOINT_STATE, &STAMPED))?;
        let (mut raw, mut encoder) = ([0u16; 2], [0u16; 2]);
        for side in 0..2 {
            let joint_state = schema(JOINT_STATE, &STAMPED);
            raw[side] = channel(GRIPPER_TOPICS[side], "sensor_msgs/msg/JointState", &joint_state)?;
            encoder[side] =
                channel(RAW_GRIPPER_TOPICS[side], "sensor_msgs/msg/JointState", &joint_state)?;
        }
        let mut flags: [Vec<(u16, Flag)>; 3] = Default::default();
        for (part, topics) in flag_topics().into_iter().enumerate() {
            for (topic, value_of) in topics {
                flags[part].push((channel(&topic, "std_msgs/msg/Bool", BOOL)?, value_of));
            }
        }
        let mut uvc = [0u16; 3];
        for (slot, topic) in UVC_TOPICS.iter().enumerate() {
            uvc[slot] = channel(topic, "yubi_go_msgs/msg/UvcFrame", &schema(UVC_FRAME, &STAMPED))?;
        }
        let (mut poses, mut velocities) = ([0u16; 3], [0u16; 3]);
        for part in 0..3 {
            poses[part] = channel(
                POSE_TOPICS[part],
                "geometry_msgs/msg/PoseStamped",
                &schema(POSE_STAMPED, &POSE_DEPS),
            )?;
            velocities[part] = channel(
                VELOCITY_TOPICS[part],
                "geometry_msgs/msg/TwistStamped",
                &schema(TWIST_STAMPED, &TWIST_DEPS),
            )?;
        }
        let (mut joys, mut input_changes) = ([0u16; 2], [0u16; 2]);
        for side in 0..2 {
            joys[side] = channel(JOY_TOPICS[side], "sensor_msgs/msg/Joy", &schema(JOY, &STAMPED))?;
            input_changes[side] = channel(
                INPUT_CHANGE_TOPICS[side],
                "yubi_go_msgs/msg/InputChanges",
                &schema(INPUT_CHANGES, &STAMPED),
            )?;
        }
        let diagnostic = schema(DIAGNOSTIC_ARRAY, &DIAGNOSTIC_DEPS);
        let array = "diagnostic_msgs/msg/DiagnosticArray";
        let diagnostics = channel("/diagnostics", array, &diagnostic)?;
        let events = channel("/yubi_go/events", array, &diagnostic)?;
        let rosout = channel(
            "/rosout",
            "rcl_interfaces/msg/Log",
            &schema(LOG, &[("builtin_interfaces/Time", TIME)]),
        )?;
        Ok(SignalsMcap {
            writer,
            order: Order::default(),
            tf,
            tf_static,
            joints,
            raw,
            encoder,
            flags,
            cams: [None; 3],
            videos: [None; 3],
            uvc,
            poses,
            velocities,
            joys,
            input_changes,
            diagnostics,
            events,
            rosout,
            expected,
            angle: [None; 2],
            statics: Some(crate::misc::mount::static_edges()),
        })
    }

    fn publish(
        &mut self,
        channel: u16,
        at_ns: u64,
        c: Capture,
        payload: Vec<u8>,
    ) -> io::Result<()> {
        if let Some(edges) = self.statics.take() {
            let latched = encode(&tf_message(c.at_ns.unwrap_or(at_ns), &edges))?;
            self.publish(self.tf_static, at_ns, c, latched)?;
            let layout = serde_json::json!({"axes": JOY_AXES, "buttons": JOY_BUTTONS}).to_string();
            let layout = encode(&StringMsg { data: layout })?;
            self.latch("/quest/controller/joy_layout", STRING, layout, at_ns)?;
        }
        mcap(self.writer.write_to_known_channel(
            &mcap::records::MessageHeader {
                channel_id: channel,
                sequence: c.seq.unwrap_or(0) as u32,
                log_time: at_ns,
                publish_time: c.at_ns.unwrap_or(at_ns),
            },
            &payload,
        ))
    }

    /// The topics the YUBI ROS stack's encoder_node latches: each calibrated encoder's
    /// zero and id, and the pair, with `null` for a side not calibrated.
    pub fn calibration(
        &mut self,
        at_ns: u64,
        calibration: &crate::misc::calib::Calibration,
    ) -> io::Result<()> {
        if calibration.sides.iter().all(Option::is_none) {
            return Ok(());
        }
        let topics = [
            ("/yubi/gripper/left/encoder_offset", "/yubi/gripper/left/id"),
            ("/yubi/gripper/right/encoder_offset", "/yubi/gripper/right/id"),
        ];
        for (side, (offset, id)) in calibration.sides.iter().zip(topics) {
            let Some(side) = side else { continue };
            self.latch(offset, FLOAT64, encode(&Float64Msg { data: side.closed })?, at_ns)?;
            self.latch(id, STRING, encode(&StringMsg { data: side.device_id.clone() })?, at_ns)?;
        }
        let id = |s| calibration.side(s).map(|s| s.device_id.as_str());
        let pair = serde_json::json!({"left": id(0), "right": id(1)}).to_string();
        self.latch("/device_ids", STRING, encode(&StringMsg { data: pair })?, at_ns)?;
        // yubi latches the closed reading alone; the open one is what says
        // which way the jaw turns, so the angle can be recomputed from raw.
        let side = |s: usize| {
            calibration.side(s).map(|s| {
                serde_json::json!({
                    "device_id": s.device_id, "closed_rad": s.closed, "open_rad": s.open,
                    "captured_realtime_ns": s.captured_realtime_ns,
                })
            })
        };
        let full = serde_json::json!({"left": side(0), "right": side(1)}).to_string();
        self.latch("/yubi_go/calibration", STRING, encode(&StringMsg { data: full })?, at_ns)
    }

    /// Every stamp in this file is the wall clock read once at the take's
    /// start, plus boottime elapsed since. This record is that pair, so a stamp
    /// converts back to boottime as `stamp - realtime_ns + boottime_ns`.
    pub fn clock(&mut self, boottime_ns: u64, realtime_ns: u64) -> io::Result<()> {
        let metadata = [
            ("stamps".to_string(), "realtime_ns + (boottime - boottime_ns)".to_string()),
            ("boottime_ns".to_string(), boottime_ns.to_string()),
            ("realtime_ns".to_string(), realtime_ns.to_string()),
        ];
        let record = mcap::records::Metadata { name: "clock".into(), metadata: metadata.into() };
        mcap(self.writer.write_metadata(&record))
    }

    fn latch(
        &mut self,
        topic: &str,
        msg: (&str, &str),
        payload: Vec<u8>,
        at_ns: u64,
    ) -> io::Result<()> {
        let (name, definition) = msg;
        let schema = mcap(self.writer.add_schema(name, "ros2msg", definition.as_bytes()))?;
        let qos = [("offered_qos_profiles".to_string(), LATCHED_QOS.to_string())].into();
        let channel = mcap(self.writer.add_channel(schema, topic, "cdr", &qos))?;
        self.publish(channel, at_ns, Capture::default(), payload)
    }

    pub fn frame(
        &mut self,
        at_ns: u64,
        capture: Capture,
        slot: usize,
        jpeg: &[u8],
    ) -> io::Result<()> {
        let Some(&frame_id) = CAM_FRAMES.get(slot) else { return Ok(()) };
        let channel = match self.cams[slot] {
            Some(channel) => channel,
            None => {
                let schema = schema(COMPRESSED_IMAGE, &STAMPED);
                let channel =
                    self.channel(CAM_TOPICS[slot], "sensor_msgs/msg/CompressedImage", &schema)?;
                *self.cams[slot].insert(channel)
            }
        };
        let stamp = capture.at_ns.unwrap_or(at_ns);
        self.order.note(CAM_TIMELINE + slot, stamp);
        let msg = CompressedImage { header: header(stamp, frame_id), format: "jpeg", data: jpeg };
        self.publish(channel, at_ns, capture, encode(&msg)?)?;
        self.uvc_frame(at_ns, capture, slot, stamp)
    }

    /// One H.264 frame in Annex B, as `CompressedVideo`.
    pub fn video(
        &mut self,
        at_ns: u64,
        capture: Capture,
        slot: usize,
        data: &[u8],
    ) -> io::Result<()> {
        let Some(&frame_id) = CAM_FRAMES.get(slot) else { return Ok(()) };
        let channel = match self.videos[slot] {
            Some(channel) => channel,
            None => {
                let schema = schema(COMPRESSED_VIDEO, &[("builtin_interfaces/Time", TIME)]);
                let channel =
                    self.channel(VIDEO_TOPICS[slot], "foxglove_msgs/msg/CompressedVideo", &schema)?;
                *self.videos[slot].insert(channel)
            }
        };
        let stamp = capture.at_ns.unwrap_or(at_ns);
        self.order.note(CAM_TIMELINE + slot, stamp);
        let msg = CompressedVideo { timestamp: self::stamp(stamp), frame_id, data, format: "h264" };
        self.publish(channel, at_ns, capture, encode(&msg)?)?;
        self.uvc_frame(at_ns, capture, slot, stamp)
    }

    fn channel(&mut self, topic: &str, schema_name: &str, schema: &str) -> io::Result<u16> {
        let id = mcap(self.writer.add_schema(schema_name, "ros2msg", schema.as_bytes()))?;
        mcap(self.writer.add_channel(id, topic, "cdr", &Default::default()))
    }

    fn uvc_frame(
        &mut self,
        at_ns: u64,
        capture: Capture,
        slot: usize,
        stamp: u64,
    ) -> io::Result<()> {
        let frame_id = CAM_FRAMES[slot];
        let u = capture.uvc.unwrap_or_default();
        let msg = UvcFrameMsg {
            header: header(stamp, frame_id),
            frame: capture.seq.unwrap_or(0),
            has_pts: capture.uvc_pts.is_some(),
            pts: capture.uvc_pts.unwrap_or(0),
            has_scr: capture.uvc_scr.is_some(),
            scr: capture.uvc_scr.unwrap_or(0),
            first_reaped: self::stamp(u.first.reaped_ns),
            first_packet: u.first.packet,
            last_reaped: self::stamp(u.last.reaped_ns),
            last_packet: u.last.packet,
            bytes: u.bytes,
            errors: u.errors,
            bad_headers: u.bad_headers,
            damaged_frames: u.damaged_frames,
            lost_packets: u.lost_packets,
            queue_drops: u.queue_drops,
        };
        self.publish(self.uvc[slot], at_ns, capture, encode(&msg)?)
    }

    /// A `Lost` tick writes no tf, but its flags still go out, so a reader
    /// can tell "not tracked" from "not recorded". The raw pose goes out
    /// whenever either half is valid, with NaN for the half that is not, and
    /// the velocity likewise.
    pub fn pose(
        &mut self,
        at_ns: u64,
        capture: Capture,
        part: usize,
        located: Located,
    ) -> io::Result<()> {
        if part >= 3 {
            return Ok(());
        }
        let stamp = capture.at_ns.unwrap_or(at_ns);
        self.order.note(POSE_TIMELINE + part, stamp);
        if let Some(p) = located.partial_pose() {
            let msg = PoseStamped {
                header: header(stamp, QUEST_ORIGIN),
                position: Vector3 { x: p[0], y: p[1], z: p[2] },
                orientation: Quaternion { x: p[3], y: p[4], z: p[5], w: p[6] },
            };
            self.publish(self.poses[part], at_ns, capture, encode(&msg)?)?;
        }
        let v = located.velocity;
        if v.linear.is_some() || v.angular.is_some() {
            let msg = TwistStamped {
                header: header(stamp, QUEST_ORIGIN),
                linear: vector(v.linear),
                angular: vector(v.angular),
            };
            self.publish(self.velocities[part], at_ns, capture, encode(&msg)?)?;
        }
        let Some(p) = located.pose() else {
            return self.publish_flags(at_ns, capture, part, located);
        };

        let angle = self.angle.get(part).copied().flatten();
        let edges = crate::misc::mount::moving_edges(part, p, angle);
        self.publish(self.tf, at_ns, capture, encode(&tf_message(stamp, &edges))?)?;
        self.publish_flags(at_ns, capture, part, located)
    }

    fn publish_flags(
        &mut self,
        at_ns: u64,
        capture: Capture,
        part: usize,
        located: Located,
    ) -> io::Result<()> {
        for i in 0..self.flags[part].len() {
            let (channel, value_of) = self.flags[part][i];
            self.publish(channel, at_ns, capture, encode(&BoolMsg { data: value_of(located) })?)?;
        }
        Ok(())
    }

    pub fn gripper(
        &mut self,
        at_ns: u64,
        seq: Option<u64>,
        side: usize,
        raw: f64,
        calib: Option<f64>,
    ) -> io::Result<()> {
        if side >= 2 {
            return Ok(());
        }
        let capture = Capture { seq, ..Capture::default() };
        self.order.note(GRIPPER_TIMELINE + side, at_ns);
        if let Some(calib) = calib {
            self.angle[side] = Some(calib);
        }

        let msg = JointState {
            header: header(at_ns, ""),
            name: vec![GRIPPER_JOINTS[side].to_string()],
            position: vec![calib.unwrap_or(raw)],
            velocity: Vec::new(),
            effort: Vec::new(),
        };
        self.publish(self.raw[side], at_ns, capture, encode(&msg)?)?;
        let msg = JointState { position: vec![raw], ..msg };
        self.publish(self.encoder[side], at_ns, capture, encode(&msg)?)?;

        let joints: Vec<(String, f64)> =
            (0..2).filter_map(|s| Some((GRIPPER_JOINTS[s].to_string(), self.angle[s]?))).collect();
        if joints.is_empty() {
            return Ok(());
        }
        let msg = JointState {
            header: header(at_ns, ""),
            name: joints.iter().map(|(n, _)| n.clone()).collect(),
            position: joints.iter().map(|(_, a)| *a).collect(),
            velocity: Vec::new(),
            effort: Vec::new(),
        };
        self.publish(self.joints, at_ns, capture, encode(&msg)?)
    }

    /// One controller's buttons and axes, in [`JOY_AXES`] / [`JOY_BUTTONS`] order.
    pub fn joy(
        &mut self,
        at_ns: u64,
        side: usize,
        axes: &[f32],
        buttons: &[i32],
    ) -> io::Result<()> {
        let Some(&channel) = self.joys.get(side) else { return Ok(()) };
        let msg = Joy { header: header(at_ns, ""), axes, buttons };
        self.publish(channel, at_ns, Capture::default(), encode(&msg)?)
    }

    pub fn input_changes(
        &mut self,
        at_ns: u64,
        side: usize,
        changes: &[crate::hub::InputChange],
    ) -> io::Result<()> {
        let Some(&channel) = self.input_changes.get(side) else { return Ok(()) };
        let msg = InputChangesMsg {
            header: header(at_ns, ""),
            name: changes.iter().map(|c| c.name).collect(),
            value: changes.iter().map(|c| c.value).collect(),
            changed: changes.iter().map(|c| stamp(c.at_ns)).collect(),
        };
        self.publish(channel, at_ns, Capture::default(), encode(&msg)?)
    }

    /// `/diagnostics` when `event` is false, `/yubi_go/events` when true.
    pub fn status(
        &mut self,
        at_ns: u64,
        event: bool,
        statuses: &[crate::misc::status::Status],
    ) -> io::Result<()> {
        let status = statuses
            .iter()
            .map(|s| DiagnosticStatus {
                level: s.level,
                name: &s.name,
                message: &s.message,
                hardware_id: &s.hardware_id,
                values: s.values.iter().map(|(k, v)| KeyValue { key: k, value: v }).collect(),
            })
            .collect();
        let msg = DiagnosticArray { header: header(at_ns, ""), status };
        let channel = if event { self.events } else { self.diagnostics };
        self.publish(channel, at_ns, Capture::default(), encode(&msg)?)
    }

    pub fn log(&mut self, at_ns: u64, line: &crate::misc::journal::Line) -> io::Result<()> {
        let msg = LogMsg {
            stamp: stamp(at_ns),
            level: line.level,
            name: &line.target,
            msg: &line.msg,
            file: &line.file,
            function: "",
            line: line.line,
        };
        self.publish(self.rosout, at_ns, Capture::default(), encode(&msg)?)
    }

    /// The close check's lines for the timelines that went backwards, which
    /// `keep_faulted_episodes` keeps a take over.
    pub fn backwards(&self) -> Vec<String> {
        TIMELINES
            .iter()
            .enumerate()
            .filter(|&(i, _)| self.order.backwards[i] > 0)
            .map(|(i, name)| {
                format!(
                    "{name}: {} of {} samples went backwards against their own source",
                    self.order.backwards[i], self.order.rows[i]
                )
            })
            .collect()
    }

    pub fn close(mut self) -> io::Result<Vec<String>> {
        mcap(self.writer.finish().map(|_| ()))?;
        let mut bad = self.backwards();
        // On disk before `meta.json` says the take is complete: a power cut
        // after the marker must not leave a marked take with a short mcap.
        self.writer.into_inner().into_inner().map_err(|e| e.into_error())?.sync_all()?;
        for (slot, expected) in self.expected.iter().enumerate() {
            if *expected && self.order.rows[CAM_TIMELINE + slot] == 0 {
                bad.push(format!(
                    "{}: was live at the start and recorded nothing",
                    CAM_NAMES[slot]
                ));
            }
        }
        Ok(bad)
    }
}

fn mcap<T>(r: mcap::McapResult<T>) -> io::Result<T> {
    r.map_err(io::Error::other)
}

#[cfg(test)]
pub fn read_all(path: &Path) -> anyhow::Result<Vec<(String, u64, Vec<u8>)>> {
    let data = std::fs::read(path)?;
    let mut out = Vec::new();
    for message in mcap::MessageStream::new(&data)? {
        let m = message?;
        out.push((m.channel.topic.clone(), m.log_time, m.data.into_owned()));
    }
    Ok(out)
}
