//! A take resampled onto the 30 Hz grid a LeRobot dataset is indexed by, with
//! every pose feature rebake derives. Each is computed the way rebake's
//! `mr_yubi_interpolated.yaml` pipeline computes it, so the two datasets
//! train alike.
//!
//! Three choices differ from that pipeline:
//! - A camera shows its frame **nearest** each tick, not the last one before
//!   it, which halves the worst-case image age (33 ms → 17 ms at 30 Hz).
//! - The hand chain is rebuilt from the raw controller poses with this
//!   build's CAD mount, so a take exports the same whichever build recorded
//!   its `/tf`.
//! - A frame is placed at its exposure, and the jaws at the instant the
//!   encoder read them, by the camera calibration's delays, so a pose and the
//!   picture beside it are of one instant.

use glam::{DQuat, DVec3};

use crate::misc::mount::{self, Link};
use crate::record::playback::Index;

pub const FPS: u64 = 30;

/// Past this gap the earlier sample is held, not blended toward the later one:
/// rebake's `max_gap_ns`, five missed samples on a 200 Hz source.
const MAX_GAP_NS: u64 = 25_000_000;

/// `DW_POSE_JUMP_MIN_CM` of AIRoA's internal data pipeline (DW): hand motion between two samples
/// stays within 0.79 cm of a constant-velocity guess, and the teleports it
/// found on YUBI Go takes measured 1.21–28 cm.
const POSE_JUMP_M: f64 = 0.07;

/// The tick times, `ceil(k / 30 s)` after `start`. Rounding up keeps
/// `ticks * 90 kHz` whole, so an mp4 built on them sits exactly on the grid.
pub fn tick(start: u64, k: usize) -> u64 {
    start + (k as u64 * 1_000_000_000).div_ceil(FPS)
}

#[derive(Clone, Copy)]
enum Source {
    Hmd,
    /// A frame below one hand root, which moves with that hand and, for the
    /// fingertips, with the gripper too.
    Hand(usize, &'static str),
}

/// The pose features, in the data pipeline's names, and where each comes from.
const FRAMES: [(&str, Source); 11] = [
    ("hmd", Source::Hmd),
    ("left_controller", Source::Hand(0, "quest_left_controller")),
    ("right_controller", Source::Hand(1, "quest_right_controller")),
    ("left_hand_root", Source::Hand(0, "left_hand_root")),
    ("right_hand_root", Source::Hand(1, "right_hand_root")),
    ("left_hand_cam", Source::Hand(0, "left_hand_cam_link")),
    ("right_hand_cam", Source::Hand(1, "right_hand_cam_link")),
    ("left_left_finger_tip", Source::Hand(0, "left_left_finger_tip_link")),
    ("left_right_finger_tip", Source::Hand(0, "left_right_finger_tip_link")),
    ("right_left_finger_tip", Source::Hand(1, "right_left_finger_tip_link")),
    ("right_right_finger_tip", Source::Hand(1, "right_right_finger_tip_link")),
];

/// One pose feature on the grid.
pub struct Track {
    pub name: &'static str,
    /// `[x, y, z, qx, qy, qz, qw]` in `quest_origin`.
    pub absolute: Vec<[f64; 7]>,
    /// The step from the previous tick, in the previous tick's frame.
    pub relative: Vec<[f64; 7]>,
    /// A source of this pose took a new sample since the previous tick.
    pub fresh: Vec<bool>,
}

pub struct Grid {
    pub t_ns: Vec<u64>,
    /// [`FRAMES`] in order, then `left_hand_root_to_right_hand_root`.
    pub tracks: Vec<Track>,
    /// Both grippers, held from the last `/joint_states`.
    pub joints: Vec<[f64; 2]>,
    pub joints_fresh: Vec<bool>,
    /// Per camera slot, the frame shown at each tick.
    pub frames: [Option<Vec<usize>>; 3],
    pub warnings: Vec<String>,
}

impl Grid {
    pub fn track(&self, name: &str) -> &Track {
        self.tracks.iter().find(|t| t.name == name).expect("a known feature")
    }
}

fn link(p: [f64; 7]) -> Link {
    Link { pos: DVec3::new(p[0], p[1], p[2]), rot: DQuat::from_xyzw(p[3], p[4], p[5], p[6]) }
}

/// The sample at `t` and the index of the one it is held from: blended
/// between the two either side, unless they are over [`MAX_GAP_NS`] apart.
fn pose_at(track: &[(u64, [f64; 7])], t: u64) -> (Link, usize) {
    let i = track.partition_point(|s| s.0 <= t).max(1) - 1;
    let a = track[i];
    let Some(&b) = track.get(i + 1).filter(|b| a.0 < t && b.0 - a.0 <= MAX_GAP_NS) else {
        return (link(a.1), i);
    };
    let u = (t - a.0) as f64 / (b.0 - a.0) as f64;
    let (a, b) = (link(a.1), link(b.1));
    let qb = if a.rot.dot(b.rot) < 0.0 { -b.rot } else { b.rot };
    let rot = (a.rot * (1.0 - u) + qb * u).normalize();
    (Link { pos: a.pos.lerp(b.pos, u), rot }, i)
}

fn held<T: Copy>(track: &[(u64, T)], t: u64) -> (T, usize) {
    let i = track.partition_point(|s| s.0 <= t).max(1) - 1;
    (track[i].1, i)
}

fn nearest(stamps: &[u64], t: u64) -> usize {
    let i = stamps.partition_point(|&s| s < t);
    match (i.checked_sub(1), stamps.get(i)) {
        (Some(before), Some(&after)) if t - stamps[before] <= after - t => before,
        (_, Some(_)) => i,
        (Some(before), None) => before,
        (None, None) => unreachable!("a camera with no frames is not sampled"),
    }
}

/// rebake's `DeltaTransformEnricher` with `previous_target_frame`: the move
/// expressed in the previous pose's frame, and the turn as
/// `conj(q_prev) ⊗ q` after flipping `q` onto `q_prev`'s hemisphere.
fn delta(prev: [f64; 7], cur: [f64; 7]) -> [f64; 7] {
    let (a, b) = (link(prev), link(cur));
    let dp = a.rot.inverse() * (b.pos - a.pos);
    let q = if a.rot.dot(b.rot) < 0.0 { -b.rot } else { b.rot };
    let dq = a.rot.conjugate() * q;
    [dp.x, dp.y, dp.z, dq.x, dq.y, dq.z, dq.w]
}

fn relative_of(absolute: &[[f64; 7]]) -> Vec<[f64; 7]> {
    let identity = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
    std::iter::once(identity).chain(absolute.windows(2).map(|w| delta(w[0], w[1]))).collect()
}

/// Whether each tick's held sample is a different one from the previous tick's.
fn changes(held: impl Iterator<Item = usize>) -> Vec<bool> {
    let mut last = None;
    held.map(|i| last.replace(i) != Some(i)).collect()
}

/// Hand samples a constant-velocity guess from the two before misses by more
/// than [`POSE_JUMP_M`].
fn jumps(track: &[(u64, [f64; 7])]) -> usize {
    let p = |s: &(u64, [f64; 7])| DVec3::new(s.1[0], s.1[1], s.1[2]);
    track
        .windows(3)
        .filter(|w| {
            let (t0, t1, t2) = (w[0].0 as f64, w[1].0 as f64, w[2].0 as f64);
            if t1 <= t0 {
                return false;
            }
            let guess = p(&w[1]) + (p(&w[1]) - p(&w[0])) * ((t2 - t1) / (t1 - t0));
            (p(&w[2]) - guess).length() > POSE_JUMP_M
        })
        .count()
}

/// `cameras`: each frame's exposure, per slot. `encoder_late_ns`: how long
/// after an instant the encoder's reading of it is stamped.
pub fn resample(
    index: &Index,
    cameras: [Option<Vec<u64>>; 3],
    encoder_late_ns: u64,
) -> Result<Grid, String> {
    let hands: [Vec<(u64, [f64; 7])>; 2] = std::array::from_fn(|side| {
        index.poses[side].iter().map(|&(t, p)| (t, mount::hand_root_pose(side, p))).collect()
    });
    let hmd = &index.poses[2];
    let cameras = cameras.map(|c| c.filter(|c| !c.is_empty()));

    let (start, end) = span(index, &hands, &cameras)?;
    let t_ns: Vec<u64> = (0..).map(|k| tick(start, k)).take_while(|&t| t <= end).collect();

    let sampled: Vec<[(Link, usize); 3]> = t_ns
        .iter()
        .map(|&t| [pose_at(&hands[0], t), pose_at(&hands[1], t), pose_at(hmd, t)])
        .collect();
    let (joints, joint_held): (Vec<[f64; 2]>, Vec<usize>) =
        t_ns.iter().map(|&t| held(&index.joints, t + encoder_late_ns)).unzip();
    let fresh: [Vec<bool>; 3] =
        std::array::from_fn(|part| changes(sampled.iter().map(|s| s[part].1)));
    let joints_fresh = changes(joint_held.iter().copied());

    let mut tracks: Vec<Track> = FRAMES
        .iter()
        .map(|&(name, source)| {
            let absolute: Vec<[f64; 7]> = sampled
                .iter()
                .zip(&joints)
                .map(|(s, j)| match source {
                    Source::Hmd => s[2].0.pose(),
                    Source::Hand(side, frame) => {
                        mount::frame_below(side, s[side].0, frame, j[side]).pose()
                    }
                })
                .collect();
            let fresh = match source {
                Source::Hmd => fresh[2].clone(),
                Source::Hand(side, frame) if frame.contains("finger") => {
                    fresh[side].iter().zip(&joints_fresh).map(|(a, b)| *a || *b).collect()
                }
                Source::Hand(side, _) => fresh[side].clone(),
            };
            Track { name, relative: relative_of(&absolute), absolute, fresh }
        })
        .collect();

    let between: Vec<[f64; 7]> = sampled
        .iter()
        .map(|s| {
            let (l, r) = (s[0].0, s[1].0);
            let back = l.rot.inverse();
            Link { pos: back * (r.pos - l.pos), rot: back * r.rot }.pose()
        })
        .collect();
    tracks.push(Track {
        name: "left_hand_root_to_right_hand_root",
        relative: relative_of(&between),
        absolute: between,
        fresh: fresh[0].iter().zip(&fresh[1]).map(|(a, b)| *a || *b).collect(),
    });

    let frames = std::array::from_fn(|slot| {
        let stamps = cameras[slot].as_ref()?;
        Some(t_ns.iter().map(|&t| nearest(stamps, t)).collect())
    });

    let warnings = hand_warnings(&hands);
    Ok(Grid { t_ns, tracks, joints, joints_fresh, frames, warnings })
}

/// The grid's first and last tick: from the latest first sample of any
/// source to the latest last one. A source with no samples refuses the take.
fn span(
    index: &Index,
    hands: &[Vec<(u64, [f64; 7])>; 2],
    cameras: &[Option<Vec<u64>>; 3],
) -> Result<(u64, u64), String> {
    let hmd = &index.poses[2];
    let mut spans = vec![
        ("the left controller", hands[0].first().map(|s| s.0), hands[0].last().map(|s| s.0)),
        ("the right controller", hands[1].first().map(|s| s.0), hands[1].last().map(|s| s.0)),
        ("the headset", hmd.first().map(|s| s.0), hmd.last().map(|s| s.0)),
        (
            "calibrated grippers",
            index.joints.first().map(|s| s.0),
            index.joints.last().map(|s| s.0),
        ),
    ];
    for c in cameras.iter().flatten() {
        spans.push(("a camera", c.first().copied(), c.last().copied()));
    }
    let mut start = 0;
    let mut end = 0;
    for (what, first, last) in spans {
        let (Some(first), Some(last)) = (first, last) else {
            return Err(format!("the episode has no samples from {what}"));
        };
        start = start.max(first);
        end = end.max(last);
    }
    Ok((start, end))
}

/// What the export says about each hand's track: its jumps and its gaps.
fn hand_warnings(hands: &[Vec<(u64, [f64; 7])>; 2]) -> Vec<String> {
    let mut warnings = Vec::new();
    for (side, hand) in hands.iter().enumerate() {
        let n = jumps(hand);
        if n > 0 {
            let name = ["left", "right"][side];
            warnings.push(format!("{name} hand: {n} jumps over 7 cm"));
        }
        let gaps = hand.windows(2).filter(|w| w[1].0 - w[0].0 > MAX_GAP_NS).count();
        if gaps > 0 {
            let name = ["left", "right"][side];
            warnings.push(format!("{name} hand: {gaps} gaps over 25 ms"));
        }
    }
    warnings
}

#[cfg(test)]
mod tests {
    use super::*;

    const STILL: [f64; 7] = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];

    fn turned(x: f64, angle: f64) -> [f64; 7] {
        let q = DQuat::from_rotation_z(angle);
        [x, 0.0, 0.0, q.x, q.y, q.z, q.w]
    }

    #[test]
    fn ticks_land_on_whole_90_khz_steps() {
        for k in 0..10_000 {
            let ns = tick(0, k);
            assert_eq!(ns * 90_000 / 1_000_000_000, 3000 * k as u64, "tick {k}");
        }
    }

    #[test]
    fn a_pose_between_two_samples_is_blended() {
        let track = [(0, turned(0.0, 0.0)), (20_000_000, turned(1.0, std::f64::consts::FRAC_PI_2))];
        let (mid, held) = pose_at(&track, 10_000_000);
        assert_eq!(held, 0);
        assert!((mid.pos.x - 0.5).abs() < 1e-12);
        let angle = mid.rot.to_axis_angle().1;
        assert!((angle - std::f64::consts::FRAC_PI_4).abs() < 1e-12, "{angle}");
    }

    #[test]
    fn a_pose_across_a_gap_is_held() {
        let track = [(0, turned(0.0, 0.0)), (30_000_000, turned(1.0, 0.0))];
        assert_eq!(pose_at(&track, 10_000_000).0.pos.x, 0.0);
        assert_eq!(pose_at(&track, 30_000_000).0.pos.x, 1.0, "on a sample is that sample");
        assert_eq!(pose_at(&track, 90_000_000).0.pos.x, 1.0, "after the last is the last");
    }

    #[test]
    fn the_blend_takes_the_short_way_round() {
        let q = DQuat::from_rotation_z(0.2);
        let flipped = [0.0, 0.0, 0.0, -q.x, -q.y, -q.z, -q.w];
        let track = [(0, STILL), (20_000_000, flipped)];
        let angle = pose_at(&track, 10_000_000).0.rot.to_axis_angle().1;
        assert!((angle - 0.1).abs() < 1e-3, "{angle}");
    }

    #[test]
    fn the_nearest_frame_wins_and_a_tie_goes_earlier() {
        let stamps = [100, 200, 300];
        assert_eq!(nearest(&stamps, 0), 0);
        assert_eq!(nearest(&stamps, 149), 0);
        assert_eq!(nearest(&stamps, 150), 0);
        assert_eq!(nearest(&stamps, 151), 1);
        assert_eq!(nearest(&stamps, 300), 2);
        assert_eq!(nearest(&stamps, 999), 2);
    }

    #[test]
    fn a_delta_is_in_the_previous_frame() {
        let prev = turned(1.0, std::f64::consts::FRAC_PI_2);
        let cur = [1.0, 1.0, 0.0, prev[3], prev[4], prev[5], prev[6]];
        let d = delta(prev, cur);
        assert!(
            (d[0] - 1.0).abs() < 1e-12 && d[1].abs() < 1e-12,
            "+y in the world is +x after a quarter turn: {d:?}"
        );
        assert!((d[6] - 1.0).abs() < 1e-12, "no turn: {d:?}");
        let r = relative_of(&[prev, cur]);
        assert_eq!(r[0], STILL);
    }

    #[test]
    fn a_turn_through_the_sign_flip_is_still_small() {
        let q = DQuat::from_rotation_z(0.1);
        let prev = [0.0, 0.0, 0.0, q.x, q.y, q.z, q.w];
        let q2 = DQuat::from_rotation_z(0.2);
        let cur = [0.0, 0.0, 0.0, -q2.x, -q2.y, -q2.z, -q2.w];
        let d = delta(prev, cur);
        assert!(d[6] > 0.99, "{d:?}");
        assert!((DQuat::from_xyzw(d[3], d[4], d[5], d[6]).to_axis_angle().1 - 0.1).abs() < 1e-9);
    }

    #[test]
    fn freshness_is_a_new_sample_since_the_last_tick() {
        assert_eq!(changes([0, 0, 1, 1, 3].into_iter()), [true, false, true, false, true]);
    }

    #[test]
    fn a_teleport_counts_as_a_jump_and_steady_motion_does_not() {
        let at = |t: u64, x: f64| (t * 10_000_000, [x, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0]);
        let steady: Vec<_> = (0..10).map(|k| at(k, k as f64 * 0.01)).collect();
        assert_eq!(jumps(&steady), 0);
        let mut teleport = steady.clone();
        teleport[5].1[0] += 0.1;
        assert_eq!(jumps(&teleport), 3, "the jump, the way back, and the guess built on it");
    }
}
