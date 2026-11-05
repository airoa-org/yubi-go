const SLACK: f32 = 0.35;

pub fn band(
    head: glam::Vec3,
    facing: glam::Quat,
    distance: f32,
    drop: f32,
) -> Option<(glam::Vec3, glam::Quat)> {
    let up = glam::Vec3::Y;
    let back = facing * glam::Vec3::Z;
    let back = glam::Vec3::new(back.x, 0.0, back.z);
    if back.length() < 1e-3 {
        return None;
    }
    let back = back.normalize();
    let right = up.cross(back).normalize();
    let rot = glam::Quat::from_mat3(&glam::Mat3::from_cols(right, back.cross(right), back));

    let pitch = (facing * glam::Vec3::NEG_Z).y.clamp(-1.0, 1.0).asin();
    let follow = (pitch + SLACK).min(0.0);
    let elevation = (drop / distance).asin() + follow;
    Some((head - back * distance * elevation.cos() + up * distance * elevation.sin(), rot))
}

/// A status quad hung under a band `band_width` metres wide that shows the
/// texture's full width: its offset from the band's centre and its width, so
/// it sits under the band where it sits on the texture and keeps its gaps.
pub fn under_band(rect: (usize, usize, usize, usize), band_width: f32) -> (f32, f32) {
    let (x, _, w, _) = rect;
    let per_px = band_width / super::panel::W as f32;
    ((x as f32 + w as f32 / 2.0 - super::panel::W as f32 / 2.0) * per_px, w as f32 * per_px)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_quads_keep_the_textures_gaps() {
        use crate::view::panel::{PANEL_ID, PANEL_NET, PANEL_STORAGE};
        let span = |rect| {
            let (centre, width) = under_band(rect, 0.52);
            (centre - width / 2.0, centre + width / 2.0)
        };
        let quads = [span(PANEL_STORAGE), span(PANEL_ID), span(PANEL_NET)];
        for pair in quads.windows(2) {
            assert!(pair[0].1 < pair[1].0, "{:?} overlaps {:?}", pair[0], pair[1]);
        }
        assert!(quads[0].0 >= -0.26 - 1e-6 && quads[2].1 <= 0.26 + 1e-6, "inside the 0.52 m band");
    }

    const DIST: f32 = 0.9;
    const DROP: f32 = -0.10;

    fn head_at(yaw: f32, pitch: f32, roll: f32) -> glam::Quat {
        glam::Quat::from_euler(glam::EulerRot::YXZ, yaw, pitch, roll)
    }

    fn angles(head: glam::Vec3, facing: glam::Quat) -> (f32, f32) {
        let (pos, _) = band(head, facing, DIST, DROP).unwrap();
        let elevation = ((pos.y - head.y) / DIST).asin();
        let pitch = (facing * glam::Vec3::NEG_Z).y.asin();
        (elevation, elevation - pitch)
    }

    #[test]
    fn nothing_inside_the_slack_reaches_the_band() {
        let head = glam::Vec3::new(0.3, 1.6, -0.2);
        let (want_pos, want_rot) = band(head, head_at(0.7, 0.0, 0.0), DIST, DROP).unwrap();
        for (pitch, roll) in [(0.6, 0.0), (1.2, 0.0), (-0.34, 0.0), (0.0, 0.4), (-0.2, -0.3)] {
            let (pos, rot) = band(head, head_at(0.7, pitch, roll), DIST, DROP).unwrap();
            assert!(
                pos.abs_diff_eq(want_pos, 1e-4),
                "pitch {pitch} roll {roll} moved the band: {pos} vs {want_pos}"
            );
            assert!(
                rot.abs_diff_eq(want_rot, 1e-4),
                "pitch {pitch} roll {roll} turned the band: {rot} vs {want_rot}"
            );
        }
    }

    #[test]
    fn a_head_down_posture_keeps_the_band_in_the_field() {
        let head = glam::Vec3::new(-0.4, 1.5, 0.9);
        for pitch in [-0.4, -0.6, -0.7, -0.9, -1.2] {
            let (_, above_gaze) = angles(head, head_at(-1.3, pitch, 0.0));
            assert!(
                above_gaze < 0.25,
                "{:.0}° up at a {:.0}° nod — off the top of the field",
                above_gaze.to_degrees(),
                -pitch.to_degrees()
            );
        }
    }

    #[test]
    fn the_band_follows_downward_only() {
        let head = glam::Vec3::Y * 1.6;
        let rest = (DROP / DIST).asin();
        for pitch in [0.0, 0.3, 0.9, 1.4] {
            let (elevation, _) = angles(head, head_at(2.2, pitch, 0.0));
            assert!((elevation - rest).abs() < 1e-4, "looking up at {pitch} lifted the band");
        }
        let (_, first) = angles(head, head_at(2.2, -0.5, 0.0));
        for pitch in [-0.7, -1.0, -1.3] {
            let (_, above_gaze) = angles(head, head_at(2.2, pitch, 0.0));
            assert!((above_gaze - first).abs() < 1e-4, "the offset moved at {pitch}");
        }
    }

    #[test]
    fn following_holds_the_distance() {
        let head = glam::Vec3::new(2.0, 1.62, -0.5);
        for pitch in [0.0, -0.35, -0.6, -1.0, -1.4] {
            let (pos, _) = band(head, head_at(0.1, pitch, 0.0), DIST, DROP).unwrap();
            assert!(
                ((pos - head).length() - DIST).abs() < 1e-4,
                "{:.3} m away at a {:.0}° nod",
                (pos - head).length(),
                -pitch.to_degrees()
            );
        }
    }

    #[test]
    fn the_band_hangs_level() {
        for pitch in [-1.0, -0.3, 0.0, 0.5, 1.2] {
            let (_, rot) = band(glam::Vec3::Y * 1.6, head_at(-2.1, pitch, 0.35), DIST, DROP)
                .expect("a head this side of vertical has a yaw");
            assert!((rot * glam::Vec3::Y).abs_diff_eq(glam::Vec3::Y, 1e-4), "rolled at {pitch}");
            assert!((rot * glam::Vec3::Z).y.abs() < 1e-4, "pitched at {pitch}");
        }
    }

    #[test]
    fn it_hangs_below_the_eyes_and_faces_them() {
        let head = glam::Vec3::new(-1.0, 1.55, 2.0);
        let (pos, rot) = band(head, head_at(1.9, 0.4, 0.0), DIST, DROP).unwrap();
        assert!((pos.y - (head.y + DROP)).abs() < 1e-4, "wrong height: {}", pos.y);
        assert!(((pos - head).length() - DIST).abs() < 1e-4, "wrong distance");
        let flat = glam::Vec3::new(pos.x - head.x, 0.0, pos.z - head.z);
        let ahead = head_at(1.9, 0.4, 0.0) * glam::Vec3::NEG_Z;
        assert!(flat
            .normalize()
            .abs_diff_eq(glam::Vec3::new(ahead.x, 0.0, ahead.z).normalize(), 1e-4));
        assert!((rot * glam::Vec3::Z).abs_diff_eq(-flat.normalize(), 1e-4), "facing away");
    }

    #[test]
    fn a_vertical_head_has_no_yaw() {
        let head = glam::Vec3::Y * 1.6;
        for pitch in [std::f32::consts::FRAC_PI_2, -std::f32::consts::FRAC_PI_2] {
            assert!(band(head, head_at(0.4, pitch, 0.0), DIST, DROP).is_none());
        }
    }
}
