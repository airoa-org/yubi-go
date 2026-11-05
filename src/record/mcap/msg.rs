//! The ROS 2 message definitions `signals.mcap` declares, and the CDR structs
//! that carry them. rosbag2 stores each schema as `ros2msg` text followed by
//! the definitions it depends on.

use std::io;

pub const BOOL: &str = include_str!("../../../assets/msg/std_msgs/Bool.msg");
pub const STRING: (&str, &str) =
    ("std_msgs/msg/String", include_str!("../../../assets/msg/std_msgs/String.msg"));
pub const FLOAT64: (&str, &str) =
    ("std_msgs/msg/Float64", include_str!("../../../assets/msg/std_msgs/Float64.msg"));
/// rosbag2's QoS YAML in its integer form, which Humble's `ros2 bag play`
/// requires and Jazzy's detects by the integer `history`. Durability 1 is
/// transient_local, so a subscriber that starts after the first message still
/// receives the calibration and the fixed mounts; zero durations mean "unspecified".
pub const LATCHED_QOS: &str = "- history: 1
  depth: 1
  reliability: 1
  durability: 1
  deadline:
    sec: 0
    nsec: 0
  lifespan:
    sec: 0
    nsec: 0
  liveliness: 1
  liveliness_lease_duration:
    sec: 0
    nsec: 0
  avoid_ros_namespace_conventions: false
";
pub const HEADER: &str = include_str!("../../../assets/msg/std_msgs/Header.msg");
pub const TIME: &str = include_str!("../../../assets/msg/builtin_interfaces/Time.msg");
pub const JOINT_STATE: &str = include_str!("../../../assets/msg/sensor_msgs/JointState.msg");
pub const COMPRESSED_IMAGE: &str =
    include_str!("../../../assets/msg/sensor_msgs/CompressedImage.msg");
pub const COMPRESSED_VIDEO: &str =
    include_str!("../../../assets/msg/foxglove_msgs/CompressedVideo.msg");
pub const TF_MESSAGE: &str = include_str!("../../../assets/msg/tf2_msgs/TFMessage.msg");
pub const TRANSFORM_STAMPED: &str =
    include_str!("../../../assets/msg/geometry_msgs/TransformStamped.msg");
pub const TRANSFORM: &str = include_str!("../../../assets/msg/geometry_msgs/Transform.msg");
pub const QUATERNION: &str = include_str!("../../../assets/msg/geometry_msgs/Quaternion.msg");
pub const VECTOR3: &str = include_str!("../../../assets/msg/geometry_msgs/Vector3.msg");
pub const POSE_STAMPED: &str = include_str!("../../../assets/msg/geometry_msgs/PoseStamped.msg");
pub const POSE: &str = include_str!("../../../assets/msg/geometry_msgs/Pose.msg");
pub const POINT: &str = include_str!("../../../assets/msg/geometry_msgs/Point.msg");
pub const TWIST_STAMPED: &str = include_str!("../../../assets/msg/geometry_msgs/TwistStamped.msg");
pub const TWIST: &str = include_str!("../../../assets/msg/geometry_msgs/Twist.msg");
pub const JOY: &str = include_str!("../../../assets/msg/sensor_msgs/Joy.msg");
pub const DIAGNOSTIC_ARRAY: &str =
    include_str!("../../../assets/msg/diagnostic_msgs/DiagnosticArray.msg");
pub const DIAGNOSTIC_STATUS: &str =
    include_str!("../../../assets/msg/diagnostic_msgs/DiagnosticStatus.msg");
pub const KEY_VALUE: &str = include_str!("../../../assets/msg/diagnostic_msgs/KeyValue.msg");
pub const LOG: &str = include_str!("../../../assets/msg/rcl_interfaces/Log.msg");
pub const UVC_FRAME: &str = include_str!("../../../assets/msg/yubi_go_msgs/UvcFrame.msg");
pub const INPUT_CHANGES: &str = include_str!("../../../assets/msg/yubi_go_msgs/InputChanges.msg");

pub const STAMPED: [(&str, &str); 2] =
    [("std_msgs/Header", HEADER), ("builtin_interfaces/Time", TIME)];
pub const TF_DEPS: [(&str, &str); 6] = [
    ("geometry_msgs/TransformStamped", TRANSFORM_STAMPED),
    ("geometry_msgs/Transform", TRANSFORM),
    ("geometry_msgs/Quaternion", QUATERNION),
    ("geometry_msgs/Vector3", VECTOR3),
    ("std_msgs/Header", HEADER),
    ("builtin_interfaces/Time", TIME),
];

pub const POSE_DEPS: [(&str, &str); 5] = [
    ("geometry_msgs/Pose", POSE),
    ("geometry_msgs/Point", POINT),
    ("geometry_msgs/Quaternion", QUATERNION),
    ("std_msgs/Header", HEADER),
    ("builtin_interfaces/Time", TIME),
];
pub const TWIST_DEPS: [(&str, &str); 4] = [
    ("geometry_msgs/Twist", TWIST),
    ("geometry_msgs/Vector3", VECTOR3),
    ("std_msgs/Header", HEADER),
    ("builtin_interfaces/Time", TIME),
];
pub const DIAGNOSTIC_DEPS: [(&str, &str); 4] = [
    ("diagnostic_msgs/DiagnosticStatus", DIAGNOSTIC_STATUS),
    ("diagnostic_msgs/KeyValue", KEY_VALUE),
    ("std_msgs/Header", HEADER),
    ("builtin_interfaces/Time", TIME),
];

/// A definition followed by its dependencies, in rosbag2's order.
pub fn schema(root: &str, deps: &[(&str, &str)]) -> String {
    let bar = "=".repeat(80);
    deps.iter().fold(root.to_string(), |s, (name, text)| format!("{s}\n{bar}\nMSG: {name}\n{text}"))
}

#[derive(serde::Serialize)]
pub struct Stamp {
    pub sec: i32,
    pub nanosec: u32,
}

#[derive(serde::Serialize)]
pub struct MsgHeader {
    pub stamp: Stamp,
    pub frame_id: String,
}

pub fn stamp(t_ns: u64) -> Stamp {
    Stamp { sec: (t_ns / 1_000_000_000) as i32, nanosec: (t_ns % 1_000_000_000) as u32 }
}

pub fn header(t_ns: u64, frame_id: &str) -> MsgHeader {
    MsgHeader { stamp: stamp(t_ns), frame_id: frame_id.to_string() }
}

#[derive(serde::Serialize)]
pub struct Vector3 {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

#[derive(serde::Serialize)]
pub struct Quaternion {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub w: f64,
}

#[derive(serde::Serialize)]
pub struct Transform {
    pub translation: Vector3,
    pub rotation: Quaternion,
}

pub fn tf_message(stamp: u64, edges: &[crate::misc::mount::Edge]) -> TfMessage {
    let transforms = edges
        .iter()
        .map(|e| TransformStamped {
            header: header(stamp, &e.parent),
            child_frame_id: e.child.clone(),
            transform: transform(e.pose),
        })
        .collect();
    TfMessage { transforms }
}

pub fn transform(p: [f64; 7]) -> Transform {
    Transform {
        translation: Vector3 { x: p[0], y: p[1], z: p[2] },
        rotation: Quaternion { x: p[3], y: p[4], z: p[5], w: p[6] },
    }
}

#[derive(serde::Serialize)]
pub struct TransformStamped {
    pub header: MsgHeader,
    pub child_frame_id: String,
    pub transform: Transform,
}

#[derive(serde::Serialize)]
pub struct TfMessage {
    pub transforms: Vec<TransformStamped>,
}

#[derive(serde::Serialize)]
pub struct CompressedImage<'a> {
    pub header: MsgHeader,
    pub format: &'a str,
    pub data: &'a [u8],
}

#[derive(serde::Serialize)]
pub struct CompressedVideo<'a> {
    pub timestamp: Stamp,
    pub frame_id: &'a str,
    pub data: &'a [u8],
    pub format: &'a str,
}

#[derive(serde::Serialize)]
pub struct JointState {
    pub header: MsgHeader,
    pub name: Vec<String>,
    pub position: Vec<f64>,
    pub velocity: Vec<f64>,
    pub effort: Vec<f64>,
}

#[derive(serde::Serialize)]
pub struct PoseStamped {
    pub header: MsgHeader,
    pub position: Vector3,
    pub orientation: Quaternion,
}

#[derive(serde::Serialize)]
pub struct TwistStamped {
    pub header: MsgHeader,
    pub linear: Vector3,
    pub angular: Vector3,
}

#[derive(serde::Serialize)]
pub struct Joy<'a> {
    pub header: MsgHeader,
    pub axes: &'a [f32],
    pub buttons: &'a [i32],
}

#[derive(serde::Serialize)]
pub struct InputChangesMsg<'a> {
    pub header: MsgHeader,
    pub name: Vec<&'a str>,
    pub value: Vec<f32>,
    pub changed: Vec<Stamp>,
}

#[derive(serde::Serialize)]
pub struct KeyValue<'a> {
    pub key: &'a str,
    pub value: &'a str,
}

#[derive(serde::Serialize)]
pub struct DiagnosticStatus<'a> {
    pub level: u8,
    pub name: &'a str,
    pub message: &'a str,
    pub hardware_id: &'a str,
    pub values: Vec<KeyValue<'a>>,
}

#[derive(serde::Serialize)]
pub struct DiagnosticArray<'a> {
    pub header: MsgHeader,
    pub status: Vec<DiagnosticStatus<'a>>,
}

#[derive(serde::Serialize)]
pub struct LogMsg<'a> {
    pub stamp: Stamp,
    pub level: u8,
    pub name: &'a str,
    pub msg: &'a str,
    pub file: &'a str,
    pub function: &'a str,
    pub line: u32,
}

#[derive(serde::Serialize)]
pub struct UvcFrameMsg {
    pub header: MsgHeader,
    pub frame: u64,
    pub has_pts: bool,
    pub pts: u32,
    pub has_scr: bool,
    pub scr: u64,
    pub first_reaped: Stamp,
    pub first_packet: u16,
    pub last_reaped: Stamp,
    pub last_packet: u16,
    pub bytes: u32,
    pub errors: u64,
    pub bad_headers: u64,
    pub damaged_frames: u64,
    pub lost_packets: u64,
    pub queue_drops: u64,
}

pub fn vector(v: Option<[f64; 3]>) -> Vector3 {
    let [x, y, z] = v.unwrap_or([f64::NAN; 3]);
    Vector3 { x, y, z }
}

#[derive(serde::Serialize)]
pub struct BoolMsg {
    pub data: bool,
}

#[derive(serde::Serialize)]
pub struct StringMsg {
    pub data: String,
}

#[derive(serde::Serialize)]
pub struct Float64Msg {
    pub data: f64,
}

pub fn encode<T: serde::Serialize>(msg: &T) -> io::Result<Vec<u8>> {
    cdr::serialize::<_, _, cdr::CdrLe>(msg, cdr::Infinite)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
