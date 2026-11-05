//! What the tracking runtime says about one part on one tick.

/// One component (position or orientation) of a pose as OpenXR flags it;
/// `TRACKED` only comes with `VALID`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ComponentTracking<T> {
    Tracked(T),
    Inferred(T),
    Lost,
}

impl<T: Copy> ComponentTracking<T> {
    pub fn is_tracked(self) -> bool {
        matches!(self, ComponentTracking::Tracked(_))
    }

    pub fn is_valid(self) -> bool {
        !matches!(self, ComponentTracking::Lost)
    }

    pub fn value(self) -> Option<T> {
        match self {
            ComponentTracking::Tracked(v) | ComponentTracking::Inferred(v) => Some(v),
            ComponentTracking::Lost => None,
        }
    }
}

/// The YUBI ROS stack's (yubi-sw) `tracked` / `valid` are both components
/// taken together.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Located {
    pub position: ComponentTracking<[f64; 3]>,
    pub orientation: ComponentTracking<[f64; 4]>,
    pub velocity: Velocity,
}

/// What `XrSpaceVelocity` says, in the same frame as the pose: linear in m/s,
/// angular in rad/s. Each half is `None` when OpenXR does not flag it valid.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Velocity {
    pub linear: Option<[f64; 3]>,
    pub angular: Option<[f64; 3]>,
}

impl Located {
    pub const LOST: Located = Located {
        position: ComponentTracking::Lost,
        orientation: ComponentTracking::Lost,
        velocity: Velocity { linear: None, angular: None },
    };

    pub fn new(
        position: ComponentTracking<[f64; 3]>,
        orientation: ComponentTracking<[f64; 4]>,
    ) -> Self {
        Located { position, orientation, velocity: Velocity::default() }
    }

    pub fn tracked(pose: [f64; 7]) -> Self {
        let (p, o) = components(pose);
        Located::new(ComponentTracking::Tracked(p), ComponentTracking::Tracked(o))
    }

    pub fn inferred(pose: [f64; 7]) -> Self {
        let (p, o) = components(pose);
        Located::new(ComponentTracking::Inferred(p), ComponentTracking::Inferred(o))
    }

    pub fn pose(self) -> Option<[f64; 7]> {
        let (p, o) = (self.position.value()?, self.orientation.value()?);
        Some([p[0], p[1], p[2], o[0], o[1], o[2], o[3]])
    }

    /// The pose with NaN in place of a lost component, so a tick that lost only
    /// one half still keeps the other. `None` only when both are lost.
    pub fn partial_pose(self) -> Option<[f64; 7]> {
        if !self.position.is_valid() && !self.orientation.is_valid() {
            return None;
        }
        let p = self.position.value().unwrap_or([f64::NAN; 3]);
        let o = self.orientation.value().unwrap_or([f64::NAN; 4]);
        Some([p[0], p[1], p[2], o[0], o[1], o[2], o[3]])
    }

    pub fn is_tracked(self) -> bool {
        self.position.is_tracked() && self.orientation.is_tracked()
    }

    pub fn is_valid(self) -> bool {
        self.position.is_valid() && self.orientation.is_valid()
    }
}

/// `[x, y, z, qx, qy, qz, qw]` as position and orientation.
pub fn components(pose: [f64; 7]) -> ([f64; 3], [f64; 4]) {
    ([pose[0], pose[1], pose[2]], [pose[3], pose[4], pose[5], pose[6]])
}
