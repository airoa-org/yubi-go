//! The eight sources the panel and the gate watch, and what each last said:
//! rates, poses and controller input.

use std::time::{Duration, Instant};

use super::{Hub, InputChange, Rec, SourceStats, State};
use crate::misc::capture::Capture;
use crate::misc::clock::boottime_ns;
use crate::misc::pose::Located;

pub const SOURCES: usize = 8;
pub const SRC_CAM: [usize; 3] = [0, 1, 2];
pub const SRC_ENCODER: [usize; 2] = [3, 4];
pub const SRC_CONTROLLER: [usize; 2] = [5, 6];
pub const SRC_HMD: usize = 7;

pub(crate) const SOURCE_NAMES: [&str; SOURCES] = [
    "left wrist cam",
    "right wrist cam",
    "head cam",
    "left gripper",
    "right gripper",
    "left controller",
    "right controller",
    "headset",
];

pub(crate) const GRIPPER_PAIR: &str = "grippers";

impl Hub {
    /// Every tick the runtime answers for a part, `Lost` included: the part
    /// is live — the runtime is answering — so its rate ticks and the panel
    /// says "not tracked" rather than "no data". Only the pose is gone.
    pub fn publish_pose(&self, capture: Capture, part: usize, located: Located) {
        let at_ns = boottime_ns();
        if part < 3 {
            let mut s = self.state.lock().unwrap();
            s.poses[part] = located.pose();
            s.tracked[part] = located.is_tracked();
            s.valid[part] = located.is_valid();
            s.untracked_since[part] = match located.is_tracked() {
                true => None,
                false => s.untracked_since[part].or(Some(Instant::now())),
            };
            let source = if part < 2 { SRC_CONTROLLER[part] } else { SRC_HMD };
            s.rates[source].tick(Instant::now());
        }
        if part < 2 {
            self.publish_hand(part, located.velocity, at_ns);
        }
        self.record(Rec::Pose { at_ns, capture, part, located });
    }

    pub fn publish_joy(&self, side: usize, axes: [f32; 4], buttons: [i32; 9]) {
        if self.capturing() {
            self.record(Rec::Joy { at_ns: boottime_ns(), side, axes, buttons });
        }
    }

    pub fn publish_input_changes(&self, side: usize, changes: Vec<InputChange>) {
        if self.capturing() {
            self.record(Rec::InputChanges { at_ns: boottime_ns(), side, changes });
        }
    }

    pub fn stats(&self) -> [SourceStats; SOURCES] {
        Self::collect_stats(&self.state.lock().unwrap())
    }

    pub(super) fn collect_stats(s: &State) -> [SourceStats; SOURCES] {
        std::array::from_fn(|i| {
            let cam = SRC_CAM
                .iter()
                .position(|&c| c == i)
                .and_then(|slot| s.cam_info.get(s.roles.rank(slot)?));
            let ((width, height), nominal) = cam.map_or(((0, 0), 0.0), |c| (c.size, c.expected()));
            let part = match i {
                _ if i == SRC_CONTROLLER[0] => Some(0),
                _ if i == SRC_CONTROLLER[1] => Some(1),
                _ if i == SRC_HMD => Some(2),
                _ => None,
            };
            SourceStats {
                hz: s.rates[i].hz(),
                live: s.rates[i].live(),
                width,
                height,
                nominal,
                extrapolated: part.is_some_and(|p| !s.tracked[p]),
                lost: part.is_some_and(|p| !s.valid[p]),
                untracked_sec: part
                    .and_then(|p| s.untracked_since[p])
                    .map_or(0.0, |t| t.elapsed().as_secs_f32()),
                bytes_mb_s: cam
                    .and_then(|c| c.bytes)
                    .filter(|(_, at)| at.elapsed() < Duration::from_secs(2))
                    .map_or(0.0, |(mb, _)| mb),
            }
        })
    }

    pub fn poses(&self) -> [Option<[f64; 7]>; 3] {
        self.state.lock().unwrap().poses
    }

    pub fn sides(&self) -> [Option<f32>; 2] {
        self.state.lock().unwrap().sides
    }
}
