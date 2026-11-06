//! Per-source liveness and rate, the camera slots, and which camera plays
//! which role. Written by the sensor threads through `Hub`, read by the gate
//! and the panel.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::hub::{SLOT_HEAD, USB_CAM_SLOTS};

#[derive(Default, Clone, Copy)]
pub(crate) struct Rate {
    pub(crate) last: Option<Instant>,
    pub(crate) mean_dt: f32,
}

impl Rate {
    pub(crate) fn tick(&mut self, now: Instant) {
        if let Some(previous) = self.last {
            let dt = now.duration_since(previous).as_secs_f32();
            self.mean_dt = match (dt > 0.0 && dt < 1.0, self.mean_dt) {
                (false, _) => 0.0,
                (true, 0.0) => dt,
                (true, mean) => mean + 0.1 * (dt - mean),
            };
        }
        self.last = Some(now);
    }

    pub(crate) fn hz(&self) -> f32 {
        if self.live() && self.mean_dt > 0.0 {
            1.0 / self.mean_dt
        } else {
            0.0
        }
    }

    pub(crate) fn live(&self) -> bool {
        self.last.is_some_and(|t| t.elapsed() < Duration::from_secs(1))
    }
}

#[derive(Default, Clone, Copy)]
pub struct SourceStats {
    pub hz: f32,
    pub live: bool,
    pub width: u16,
    pub height: u16,
    pub nominal: f32,
    pub extrapolated: bool,
    /// The runtime gives no full pose at all, where `extrapolated` still has one.
    pub lost: bool,
    pub untracked_sec: f32,
    pub bytes_mb_s: f32,
}

/// A camera at or above this rate is not starved, whatever its peak. The
/// wrists reach ~100 fps, and 80% of that would fault footage that is fine at
/// 65. A camera whose 80% line is lower, such as a 30 fps module, is judged by
/// that line alone.
const GOOD_ENOUGH_HZ: f32 = 60.0;

impl SourceStats {
    pub fn starved(&self) -> bool {
        self.nominal > 0.0 && self.hz < self.nominal * 0.8 && self.hz < GOOD_ENOUGH_HZ
    }
}

/// A slot's preview.
#[derive(Default, Clone)]
pub(crate) struct Cam {
    pub(crate) frame: Option<Arc<[u8]>>,
    pub(crate) seq: u64,
}

/// What one camera is, by rank: it stays with the camera when the roles
/// move it to another slot.
#[derive(Default, Clone)]
pub(crate) struct CamInfo {
    pub(crate) name: String,
    pub(crate) size: (u16, u16),
    pub(crate) nominal: f32,
    pub(crate) peak: f32,
    pub(crate) bytes: Option<(f32, Instant)>,
    /// The side-by-side stereo module (`worker::STEREO_MODULE`).
    pub(crate) stereo: bool,
}

impl CamInfo {
    pub(crate) fn expected(&self) -> f32 {
        match self.nominal > 0.0 {
            true => self.peak.min(self.nominal),
            false => 0.0,
        }
    }
}

/// One USB port (`misc::usb::port_name`) per slot or per rank, `None` where
/// nothing is known.
pub type Ports = [Option<String>; USB_CAM_SLOTS.len()];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Roles([usize; USB_CAM_SLOTS.len()]);

impl Default for Roles {
    fn default() -> Self {
        Self(USB_CAM_SLOTS)
    }
}

impl Roles {
    /// Which camera each frame shows. Duplicates are legal while the operator
    /// is picking; only committing insists on a mapping.
    pub fn new(choices: [usize; USB_CAM_SLOTS.len()]) -> Self {
        Self(choices)
    }

    pub fn rank(&self, slot: usize) -> Option<usize> {
        self.0.get(slot).copied()
    }

    pub fn slot(&self, rank: usize) -> Option<usize> {
        self.0.iter().position(|&r| r == rank)
    }

    pub fn ranks(&self) -> [usize; USB_CAM_SLOTS.len()] {
        self.0
    }

    pub fn from_ranks(ranks: &[usize]) -> Option<Self> {
        let mut out = USB_CAM_SLOTS;
        if ranks.len() != out.len() {
            return None;
        }
        out.copy_from_slice(ranks);
        let mut seen = out;
        seen.sort_unstable();
        let mut want = USB_CAM_SLOTS;
        want.sort_unstable();
        (seen == want).then_some(Self(out))
    }

    /// Each slot reads the open camera plugged into the port saved for it; the
    /// slots left over take the remaining ranks, open ones first. `saved` is by
    /// slot, `open` by rank. Without a head camera the head slot keeps no port,
    /// so a wrist camera moved onto the head's old port still fills a wrist.
    pub fn from_ports(saved: &Ports, open: &Ports, head: bool) -> Self {
        let mut slots: [Option<usize>; USB_CAM_SLOTS.len()] = Default::default();
        for (slot, port) in saved.iter().enumerate() {
            if slot == SLOT_HEAD && !head {
                continue;
            }
            let found = port.as_ref().and_then(|p| open.iter().position(|o| o.as_ref() == Some(p)));
            slots[slot] = found.filter(|rank| !slots.contains(&Some(*rank)));
        }
        let mut spare: Vec<usize> =
            USB_CAM_SLOTS.into_iter().filter(|rank| !slots.contains(&Some(*rank))).collect();
        spare.sort_by_key(|&rank| open[rank].is_none());
        let mut spare = spare.into_iter();
        Self(slots.map(|s| s.or_else(|| spare.next()).expect("as many ranks as slots")))
    }

    /// `slot` reads `rank`, and the slot that read `rank` takes `slot`'s old
    /// one, so the result is still one camera per slot.
    pub fn swapped(mut self, slot: usize, rank: usize) -> Self {
        if let Some(other) = self.slot(rank) {
            self.0.swap(slot, other);
        }
        self
    }

    /// The port each slot reads, for saving: `open` is by rank.
    pub fn ports(&self, open: &Ports) -> Ports {
        self.0.map(|rank| open[rank].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ports(p: [&str; 3]) -> Ports {
        p.map(|s| (!s.is_empty()).then(|| s.to_string()))
    }

    /// The point of saving ports: cameras that open in another order after a
    /// replug still land in the slots they were assigned to.
    #[test]
    fn a_saved_port_follows_its_camera_whatever_order_it_opened_in() {
        let saved = ports(["1-1.2", "1-1.3", "1-1.4"]);
        let roles = Roles::from_ports(&saved, &ports(["1-1.4", "1-1.2", "1-1.3"]), true);
        assert_eq!(roles.ranks(), [1, 2, 0]);
        assert_eq!(roles.ports(&ports(["1-1.4", "1-1.2", "1-1.3"])), saved);
    }

    #[test]
    fn slots_without_a_present_port_take_the_spare_ranks_in_order() {
        let saved = ports(["1-1.3", "", "9-9"]);
        let roles = Roles::from_ports(&saved, &ports(["1-1.1", "1-1.2", "1-1.3"]), true);
        assert_eq!(roles.ranks(), [2, 0, 1], "left found; right and head fill in order");
        assert_eq!(
            Roles::from_ports(&ports(["", "", ""]), &ports(["a", "b", "c"]), true),
            Roles::default()
        );
    }

    #[test]
    fn one_port_saved_twice_is_used_once() {
        let roles =
            Roles::from_ports(&ports(["1-1.2", "1-1.2", ""]), &ports(["1-1.2", "", ""]), true);
        assert_eq!(roles.ranks(), [0, 1, 2]);
    }

    /// Three cameras down to two: the left wrist's port is empty and its camera
    /// now sits on the head's old port.
    #[test]
    fn without_a_head_camera_the_wrists_take_the_open_cameras() {
        let saved = ports(["1-1.3", "1-1.4", "1-1.2"]);
        let open = ports(["1-1.2", "", "1-1.4"]);
        assert_eq!(Roles::from_ports(&saved, &open, false).ranks(), [0, 2, 1]);
        assert_eq!(
            Roles::from_ports(&saved, &open, true).ranks(),
            [1, 2, 0],
            "fitted head keeps it"
        );
    }
}
