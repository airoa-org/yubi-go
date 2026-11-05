//! Each camera on the rig per gripper pair: its lens, where it is mounted, and
//! how late its frames are. The viewer draws the rig into the wrist cameras
//! with it ([`Calib`]); a pair without a saved calibration in
//! `camera-calibration/` gets the built-in [`DEFAULT`]. The export places
//! frames by [`DEFAULT`]'s delays only, so every headset exports alike. (The gripper encoders' own calibration, `POST /calibrate`, is a
//! different thing.)

pub mod store;
mod takes;

use serde::{Deserialize, Serialize};

use crate::record::playback::Index;
pub(crate) use takes::frame_times;

/// The calibration the viewer draws with until a gripper pair has its own:
/// the laptop fit of 2026-10-30 on grippers PL028/PR028. Its delays are 0,
/// so a frame and the jaws sit at their stamps: the fit on that rig measured
/// 3-5 ms, under half a wrist frame, and another rig's are unknown.
pub const DEFAULT: &str = include_str!("../../../assets/calib/default.json");

/// The wrist and encoder delays of a calibration a pre-release build saved
/// before they were fitted: 0, as in [`DEFAULT`].
fn wrist_late_start() -> Sides<f64> {
    Sides { left: 0.0, right: 0.0 }
}
fn encoder_late_start() -> Sides<f64> {
    Sides { left: 0.0, right: 0.0 }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sides<T> {
    pub left: T,
    pub right: T,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Mount {
    pub t: [f64; 3],
    /// x, y, z, w.
    pub q: [f64; 4],
}

/// One camera: its lens, and where it sits (`X_cam = q·(X − t)` in the frame
/// it rides: the hmd for a head eye, `*_hand_cam_link` for a wrist).
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Camera {
    pub f: f64,
    pub k1: f64,
    pub k2: f64,
    pub c: [f64; 2],
    pub theta_max: f64,
    pub mount: Mount,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Fix {
    pub p: [f64; 3],
    pub q: [f64; 4],
}

/// The finger angle is `k·q + q0` for the encoder's angle q, in rad.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Encoder {
    pub k: f64,
    pub q0: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Calib {
    /// The gripper ids from `meta.json` the fit ran on, left then right.
    pub grippers: [String; 2],
    pub head: [Camera; 2],
    /// A head frame shows the rig this long before its stamp, and a wrist
    /// frame likewise; the encoder reads an instant this long after the
    /// poses do. In ms.
    pub head_late_ms: f64,
    #[serde(default = "wrist_late_start")]
    pub wrist_late_ms: Sides<f64>,
    #[serde(default = "encoder_late_start")]
    pub encoder_late_ms: Sides<f64>,
    pub wrist: Sides<Camera>,
    pub root_fix: Sides<Fix>,
    pub encoder: Sides<Encoder>,
    /// How the fit went: the takes, the error per kind, the time it took.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub fit: serde_json::Value,
}

pub fn default() -> Calib {
    serde_json::from_str(DEFAULT).expect("assets/calib/default.json parses")
}

/// When each frame of camera `slot` (`CAM_NAMES` order) was exposed, on the
/// file's clock: its time from [`frame_times`] less the camera's delay. A
/// frame without a presentation time keeps its stamp as its time.
pub fn exposures(index: &Index, slot: usize, calib: &Calib) -> Option<Vec<u64>> {
    let camera = index.cameras[slot].as_ref()?;
    let late =
        [calib.wrist_late_ms.left, calib.wrist_late_ms.right, calib.head_late_ms][slot] * 1e6;
    let times = frame_times(&index.uvc[slot], 0);
    Some(
        camera
            .frames
            .iter()
            .map(|f| {
                let t = times
                    .binary_search_by_key(&f.t_ns, |x| x.0)
                    .map_or(f.t_ns as f64, |i| times[i].1);
                (t - late).max(0.0) as u64
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests;
