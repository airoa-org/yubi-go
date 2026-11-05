use glam::{DQuat, DVec3};

macro_rules! mesh {
    ($n:literal) => {
        ($n, include_bytes!(concat!("../../assets/meshes/yubi/", $n)) as &[u8])
    };
}
pub const MESHES: &[(&str, &[u8])] = &[
    mesh!("hand_left_left_finger_link.STL"),
    mesh!("hand_left_right_finger_link.STL"),
    mesh!("hand_right_left_finger_link.STL"),
    mesh!("hand_right_right_finger_link.STL"),
];

pub const CONTROLLER_MESHES: &[(&str, &[u8])] = &[
    ("left.stl", include_bytes!("../../assets/meshes/quest/left.stl") as &[u8]),
    ("right.stl", include_bytes!("../../assets/meshes/quest/right.stl") as &[u8]),
];

/// `left_mount_joint` in `assets/yubi.urdf`; the right hand mirrors `y` and
/// yaw. `the_grip_mount_is_the_urdfs_mount_joint` keeps the two equal.
const MOUNT_XYZ: [f64; 3] = [-0.063, 0.016, 0.0525];
const MOUNT_RPY: [f64; 3] = [0.0, -std::f64::consts::FRAC_PI_4, std::f64::consts::FRAC_PI_4];

/// The OpenXR grip origin in the URDF's `quest_*_controller` (OVR anchor)
/// frame, in metres: Meta's anchor-to-grip offset `(0, -0.03, -0.04)` from
/// `OVRInput.cs` (`_gripToVrapiOffsetInverse`), in ROS axes. It has no lateral
/// part, so both hands share it.
const ANCHOR_XYZ: [f64; 3] = [-0.04, 0.0, -0.03];

/// The rotation from the URDF controller frame into grip axes. The 60° pitch
/// is the rotation in the same `OVRInput.cs` constant as [`ANCHOR_XYZ`].
pub fn controller_correction() -> DQuat {
    (xr_to_ros() * DQuat::from_rotation_x(std::f64::consts::FRAC_PI_3)).inverse()
}

pub(crate) fn xr_to_ros() -> DQuat {
    DQuat::from_rotation_z(-std::f64::consts::FRAC_PI_2)
        * DQuat::from_rotation_x(std::f64::consts::FRAC_PI_2)
}

fn grip_to_hand_root(side: usize) -> (DVec3, DQuat) {
    let flip = if side == 0 { 1.0 } else { -1.0 };
    let translation = DVec3::new(MOUNT_XYZ[0], flip * MOUNT_XYZ[1], MOUNT_XYZ[2]);
    let inverse = (DQuat::from_rotation_z(flip * MOUNT_RPY[2])
        * DQuat::from_rotation_y(MOUNT_RPY[1]))
    .inverse();
    let w = xr_to_ros();
    let grip_to_hand_root = controller_correction() * inverse;
    let anchor = DVec3::from_array(ANCHOR_XYZ);
    (
        w * (grip_to_hand_root * -translation - controller_correction() * anchor),
        w * grip_to_hand_root,
    )
}

pub fn hand_root(side: usize, p: DVec3, q: DQuat) -> (DVec3, DQuat) {
    let (offset, rotation) = grip_to_hand_root(side);
    (p + q * offset, q * rotation)
}

pub const LINK_NAMES: [&str; 6] = [
    "hand_root",
    "hand_cam_link",
    "right_finger_link",
    "right_finger_tip_link",
    "left_finger_link",
    "left_finger_tip_link",
];

#[derive(Clone, Copy, Debug)]
pub struct Link {
    pub pos: DVec3,
    pub rot: DQuat,
}

impl Link {
    pub fn pose(self) -> [f64; 7] {
        let (p, q) = (self.pos, self.rot);
        [p.x, p.y, p.z, q.x, q.y, q.z, q.w]
    }
}

fn chain(parent: Link, local: Link) -> Link {
    Link { pos: parent.pos + parent.rot * local.pos, rot: parent.rot * local.rot }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JointKind {
    Fixed,
    Floating,
    Revolute,
}

#[derive(Clone, Copy, Debug)]
pub struct Joint {
    pub kind: JointKind,
    pub parent: &'static str,
    pub child: &'static str,
    pub xyz: [f64; 3],
    pub rpy: [f64; 3],
    pub axis: [f64; 3],
    pub limits: [f64; 2],
}

// The URDF writes pi/4 as 0.785398.
#[allow(clippy::approx_constant)]
pub const JOINTS: &[Joint] = &include!(concat!(env!("OUT_DIR"), "/urdf_joints.rs"));

impl Joint {
    fn at(&self, q: f64) -> Link {
        let [roll, pitch, yaw] = self.rpy;
        let origin = DQuat::from_rotation_z(yaw)
            * DQuat::from_rotation_y(pitch)
            * DQuat::from_rotation_x(roll);
        let turn = DQuat::from_axis_angle(DVec3::from_array(self.axis).normalize(), q);
        Link { pos: DVec3::from_array(self.xyz), rot: origin * turn }
    }
}

const HAND_ROOTS: [&str; 2] = ["left_hand_root", "right_hand_root"];

fn finger_value(joint: &Joint, aperture: f64) -> f64 {
    if joint.limits[1] > 0.0 {
        aperture
    } else {
        -aperture
    }
}

/// The URDF hangs the OVR anchor here; we record the raw OpenXR grip.
fn fixed(joint: &Joint) -> Link {
    match ["quest_left_controller", "quest_right_controller"].iter().position(|c| *c == joint.child)
    {
        Some(side) => {
            let (offset, rotation) = grip_to_hand_root(side);
            let back = rotation.inverse();
            Link { pos: back * -offset, rot: back }
        }
        None => joint.at(0.0),
    }
}

/// One edge of the tf tree, named as the URDF of the YUBI ROS stack (yubi-sw)
/// names it. The recorder and
/// rosbridge both serialize these and neither decides a name or a parent.
#[derive(Clone, Debug, PartialEq)]
pub struct Edge {
    pub parent: String,
    pub child: String,
    pub pose: [f64; 7],
}

impl Edge {
    fn new(parent: &str, child: &str, link: Link) -> Edge {
        Edge { parent: parent.into(), child: child.into(), pose: link.pose() }
    }
}

pub fn static_edges() -> Vec<Edge> {
    JOINTS
        .iter()
        .filter(|j| j.kind == JointKind::Fixed)
        .map(|j| Edge::new(j.parent, j.child, fixed(j)))
        .collect()
}

pub fn moving_edges(part: usize, pose: [f64; 7], aperture: Option<f64>) -> Vec<Edge> {
    let Some(&root) = HAND_ROOTS.get(part) else {
        return vec![Edge { parent: "quest_origin".into(), child: "quest_hmd".into(), pose }];
    };
    let hand = Edge {
        parent: "quest_origin".into(),
        child: root.into(),
        pose: hand_root_pose(part, pose),
    };
    let fingers = JOINTS.iter().filter(|j| j.kind == JointKind::Revolute && j.parent == root);
    let fingers = aperture.into_iter().flat_map(|a| fingers.clone().map(move |j| (j, a)));
    std::iter::once(hand)
        .chain(fingers.map(|(j, a)| Edge::new(j.parent, j.child, j.at(finger_value(j, a)))))
        .collect()
}

pub fn hand_root_pose(side: usize, pose: [f64; 7]) -> [f64; 7] {
    let (p, q) = hand_root(
        side,
        DVec3::new(pose[0], pose[1], pose[2]),
        DQuat::from_xyzw(pose[3], pose[4], pose[5], pose[6]),
    );
    Link { pos: p, rot: q }.pose()
}

pub fn hand_links(side: usize, controller: (DVec3, DQuat), aperture: f64) -> [Link; 6] {
    let (p, q) = hand_root(side, controller.0, controller.1);
    let root = Link { pos: p, rot: q };
    let prefix = ["left", "right"][side];
    LINK_NAMES.map(|name| below(root, HAND_ROOTS[side], &format!("{prefix}_{name}"), aperture))
}

/// A frame of one hand, with its hand root at `root` and the jaws at `aperture`.
pub fn frame_below(side: usize, root: Link, frame: &str, aperture: f64) -> Link {
    below(root, HAND_ROOTS[side], frame, aperture)
}

fn below(root: Link, root_name: &str, frame: &str, aperture: f64) -> Link {
    if frame == root_name {
        return root;
    }
    let joint = JOINTS.iter().find(|j| j.child == frame).unwrap_or_else(|| panic!("{frame}"));
    let local = match joint.kind {
        JointKind::Revolute => joint.at(finger_value(joint, aperture)),
        _ => fixed(joint),
    };
    chain(below(root, root_name, joint.parent, aperture), local)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joint(child: &str) -> &'static Joint {
        JOINTS.iter().find(|j| j.child == child).unwrap()
    }

    #[test]
    fn the_grip_mount_is_the_urdfs_mount_joint() {
        assert_eq!(JOINTS.len(), 14, "yubi's URDF has fourteen joints");
        for (side, flip) in [(0, 1.0), (1, -1.0)] {
            let mount = joint(["quest_left_controller", "quest_right_controller"][side]);
            assert_eq!(mount.xyz, [MOUNT_XYZ[0], flip * MOUNT_XYZ[1], MOUNT_XYZ[2]]);
            let rpy = [0.0, MOUNT_RPY[1], flip * MOUNT_RPY[2]];
            assert!(mount.rpy.iter().zip(rpy).all(|(a, b)| (a - b).abs() < 1e-12), "{side}");
        }
    }

    #[test]
    fn the_two_fingers_open_away_from_each_other() {
        let right = finger_value(joint("left_right_finger_link"), 0.5);
        let left = finger_value(joint("left_left_finger_link"), 0.5);
        assert_eq!((right, left), (0.5, -0.5), "each stays inside its own limits");
    }

    #[test]
    fn urdf_mount_matches_the_cad_corrected_values() {
        assert_eq!(MOUNT_XYZ, [-0.063, 0.016, 0.0525]);
        let magnitude = DVec3::from_array(MOUNT_XYZ).length();
        assert!((magnitude - 0.08355).abs() < 1e-4, "{magnitude} m, want 0.0836");
        let q = DQuat::from_rotation_z(MOUNT_RPY[2]) * DQuat::from_rotation_y(MOUNT_RPY[1]);
        let degrees = 2.0 * q.w.clamp(-1.0, 1.0).acos().to_degrees();
        assert!((degrees - 62.8).abs() < 0.2, "rotation {degrees} deg, want 62.8");
    }

    #[test]
    fn hand_root_keeps_the_mount_distance() {
        let urdf = DVec3::from_array(MOUNT_XYZ).length();
        assert!((urdf - 0.08355).abs() < 1e-4, "URDF mount moved: {urdf}");
        for side in 0..2 {
            let (offset, _) = grip_to_hand_root(side);
            let chain = xr_to_ros().inverse() * offset
                + controller_correction() * DVec3::from_array(ANCHOR_XYZ);
            assert!(
                (chain.length() - urdf).abs() < 1e-9,
                "side {side}: mount part {} m, want {urdf}",
                chain.length()
            );
            let (p, _) = hand_root(side, DVec3::ZERO, DQuat::IDENTITY);
            assert!(
                (p.length() - offset.length()).abs() < 1e-12,
                "side {side}: hand_root is not the rigid offset"
            );
        }
    }

    #[test]
    fn hand_root_is_a_rigid_offset_from_the_grip() {
        let p = DVec3::new(0.3, -0.2, 1.1);
        let q = DQuat::from_rotation_y(0.6) * DQuat::from_rotation_x(-0.2);
        for side in 0..2 {
            let (root_p, root_q) = hand_root(side, p, q);
            let (offset, rotation) = grip_to_hand_root(side);
            let back_p = root_p - q * offset;
            let back_q = root_q * rotation.inverse();
            assert!((back_p - p).length() < 1e-12, "side {side} translation");
            assert!(back_q.dot(q).abs() > 1.0 - 1e-12, "side {side} rotation");
        }
    }

    #[test]
    fn the_static_controller_link_undoes_the_mount() {
        let (p_xr, q_xr) = awkward_pose();
        for side in 0..2 {
            let (hp, hq) = hand_root(side, p_xr, q_xr);
            let child = ["quest_left_controller", "quest_right_controller"][side];
            let edge = static_edges().into_iter().find(|e| e.child == child).unwrap();
            let back_p = DVec3::new(edge.pose[0], edge.pose[1], edge.pose[2]);
            let back_q = DQuat::from_xyzw(edge.pose[3], edge.pose[4], edge.pose[5], edge.pose[6]);
            let p = hp + hq * back_p;
            let q = hq * back_q;
            assert!((p - p_xr).length() < 1e-12, "side {side}: {p} vs {p_xr}");
            assert!(q.dot(q_xr).abs() > 1.0 - 1e-12, "side {side}: {q} vs {q_xr}");
        }
    }

    #[test]
    fn controller_correction_is_a_proper_rotation() {
        let c = controller_correction();
        assert!((c.length() - 1.0).abs() < 1e-12, "not a unit quaternion");
        let m = glam::DMat3::from_quat(c);
        assert!((m.determinant() - 1.0).abs() < 1e-12, "det {}", m.determinant());
        let (s, c) = (0.5, 3f64.sqrt() / 2.0);
        assert!((m * DVec3::X - DVec3::new(0.0, -c, -s)).length() < 1e-12);
        assert!((m * DVec3::Y - DVec3::NEG_X).length() < 1e-12);
        assert!((m * DVec3::Z - DVec3::new(0.0, s, -c)).length() < 1e-12);
    }

    fn to_ros_p(v: DVec3) -> DVec3 {
        DVec3::new(-v.z, -v.x, v.y)
    }
    fn to_ros_q(q: DQuat) -> DQuat {
        DQuat::from_xyzw(-q.z, -q.x, q.y, q.w)
    }

    fn awkward_pose() -> (DVec3, DQuat) {
        (
            DVec3::new(0.12, 1.35, -0.4),
            DQuat::from_rotation_y(0.8)
                * DQuat::from_rotation_x(-0.35)
                * DQuat::from_rotation_z(0.5),
        )
    }

    #[test]
    fn mount_survives_the_ros_remap() {
        for side in 0..2 {
            let flip = if side == 0 { 1.0 } else { -1.0 };
            let t = DVec3::new(MOUNT_XYZ[0], flip * MOUNT_XYZ[1], MOUNT_XYZ[2]);
            let grip_to_hr = controller_correction()
                * (DQuat::from_rotation_z(flip * MOUNT_RPY[2])
                    * DQuat::from_rotation_y(MOUNT_RPY[1]))
                .inverse();
            let (p_xr, q_xr) = awkward_pose();

            let anchor = controller_correction() * DVec3::from_array(ANCHOR_XYZ);
            let want_q = xr_to_ros() * (q_xr * grip_to_hr);
            let want_p = to_ros_p(p_xr + q_xr * (grip_to_hr * -t - anchor));

            let (got_p, got_q) = hand_root(side, to_ros_p(p_xr), to_ros_q(q_xr));

            assert!((got_p - want_p).length() < 1e-12, "side {side}: {got_p} vs {want_p}");
            assert!(got_q.dot(want_q).abs() > 1.0 - 1e-12, "side {side}: {got_q} vs {want_q}");
        }
    }

    #[test]
    fn finger_chain_matches_the_xr_composition() {
        let angle = 0.5;
        let (p_xr, q_xr) = awkward_pose();
        for side in 0..2 {
            let (offset, rotation) = grip_to_hand_root(side);
            let w_inv = xr_to_ros().inverse();
            let hr_rot = q_xr * (w_inv * rotation);
            let hr_pos = p_xr + q_xr * (w_inv * offset);
            let sign = -1.0;
            let tip_rot = hr_rot * DQuat::from_rotation_z(sign * angle);
            let want = hr_pos
                + hr_rot * DVec3::new(-0.016, sign * 0.015, 0.0)
                + tip_rot * DVec3::new(0.10943, sign * 0.014, 0.0);

            let links = hand_links(side, (to_ros_p(p_xr), to_ros_q(q_xr)), angle);
            let tip = links[3];
            let got = w_inv * tip.pos;
            assert!(
                (got - want).length() < 1e-12,
                "side {side}: tip {got} vs {want} ({} mm off)",
                (got - want).length() * 1000.0
            );
            let got_rot = w_inv * tip.rot;
            assert!(got_rot.dot(tip_rot).abs() > 1.0 - 1e-12, "side {side}: tip frame");
        }
    }

    #[test]
    fn correction_pitches_the_pointing_axis_off_the_handle() {
        let m = glam::DMat3::from_quat(controller_correction());
        let handle = DVec3::NEG_Z;
        let pointing = m * DVec3::X;
        let degrees = pointing.dot(handle).clamp(-1.0, 1.0).acos().to_degrees();
        assert!((degrees - 60.0).abs() < 1e-9, "{degrees} deg off the handle, want 60");
    }

    #[test]
    fn jaws_pinch_rather_than_sweep() {
        let at = |angle| {
            let l = hand_links(0, (DVec3::ZERO, DQuat::IDENTITY), angle);
            (l[3].pos - l[5].pos).length()
        };
        let (closed, open) = (at(0.0), at(0.94));
        assert!(
            (open - closed).abs() > 0.02,
            "jaw separation barely moved: {closed:.4} -> {open:.4}"
        );

        let l = hand_links(0, (DVec3::ZERO, DQuat::IDENTITY), 0.5);
        let into_hand = |p: DVec3| l[0].rot.inverse() * (p - l[0].pos);
        let (right, left) = (into_hand(l[3].pos), into_hand(l[5].pos));
        assert!(
            (right.y + left.y).abs() < 1e-9 && (right.x - left.x).abs() < 1e-9,
            "tips are not mirrored in the hand frame: {right:?} vs {left:?}"
        );
        assert!(into_hand(l[2].pos).y < 0.0, "right finger is on the wrong side");
    }

    #[test]
    fn every_link_inherits_the_mount_offset() {
        let controller = (DVec3::new(0.3, -0.2, 1.1), DQuat::from_rotation_y(0.6));
        for side in 0..2 {
            let links = hand_links(side, controller, 0.3);
            let (root, _) = hand_root(side, controller.0, controller.1);
            assert!((links[0].pos - root).length() < 1e-12, "link 0 is hand_root");
            for (i, l) in links.iter().enumerate() {
                assert!(
                    (l.pos - controller.0).length() > 0.01,
                    "link {i} sits on the controller, not the hand"
                );
            }
        }
    }

    #[test]
    fn hands_are_mirrored() {
        let (left, _) = hand_root(0, DVec3::ZERO, DQuat::IDENTITY);
        let (right, _) = hand_root(1, DVec3::ZERO, DQuat::IDENTITY);
        assert!((left - right).length() > 0.01, "left {left:?} right {right:?}");
    }
}
