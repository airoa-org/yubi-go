//! The messages the writer thread takes, and the controller input changes
//! some of them carry.

use std::sync::Arc;

use super::{click, Fault};
use crate::misc::capture::Capture;
use crate::misc::pose::Located;

/// `Start` carries the whole `Config`, which makes every message at least 608
/// bytes by clippy's count. The writer's 512-message queue is then ~300 KB,
/// too little to be worth a box.
#[allow(clippy::large_enum_variant)]
pub(crate) enum Rec {
    Arm,
    Disarm,
    Pause,
    Start {
        gesture: Option<click::Gesture>,
        config: crate::misc::config::Config,
    },
    Stop {
        success: bool,
        gesture: Option<click::Gesture>,
    },
    Advance {
        gesture: click::Gesture,
    },
    Fail {
        at_ns: u64,
    },
    Resume {
        gesture: click::Gesture,
    },
    Frame {
        at_ns: u64,
        capture: Capture,
        slot: usize,
        jpeg: Arc<[u8]>,
    },
    /// One H.264 frame in Annex B, from the camera's encoder.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    Video {
        at_ns: u64,
        capture: Capture,
        slot: usize,
        data: Vec<u8>,
        key_frame: bool,
    },
    Pose {
        at_ns: u64,
        capture: Capture,
        part: usize,
        located: Located,
    },
    Gripper {
        at_ns: u64,
        seq: Option<u64>,
        side: usize,
        raw: f64,
        calib: Option<f64>,
    },
    Joy {
        at_ns: u64,
        side: usize,
        axes: [f32; 4],
        buttons: [i32; 9],
    },
    InputChanges {
        at_ns: u64,
        side: usize,
        changes: Vec<InputChange>,
    },
    /// `/diagnostics` when `event` is false, `/yubi_go/events` when true.
    Status {
        at_ns: u64,
        event: bool,
        statuses: Vec<crate::misc::status::Status>,
    },
    Fault {
        at_ns: u64,
        source: &'static str,
        fault: Fault,
    },
    Cancel,
    Shutdown,
    Uuid(String),
    Call {
        command: Box<Rec>,
        reply: std::sync::mpsc::Sender<Result<serde_json::Value, String>>,
    },
    FaultOver {
        at_ns: u64,
        source: &'static str,
        fault: Fault,
    },
}

/// One controller input that changed, with the boottime OpenXR says it did.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputChange {
    pub name: &'static str,
    pub value: f32,
    pub at_ns: u64,
}
