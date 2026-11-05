use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::misc::config::Video;

/// Bytes a second a take writes with both wrists and the stereo head camera,
/// measured on a Quest 3S on 2026-11-02. `h264`: 14.3-18.0 GB/h over ten takes of
/// 21-73 s, and the highest is used so the time left errs short. `jpeg`: one
/// 60 s take of 1.48 GB, which matches the cameras' 24.6 MB/s on the wire.
fn encode_bytes_per_sec(video: Video) -> u64 {
    match video {
        Video::H264 => 18_000_000_000 / 3600,
        Video::Jpeg => 88_000_000_000 / 3600,
    }
}

pub const CHARGE_FLOOR: Duration = Duration::from_secs(5 * 60);

const RESERVE_STEPS: [Duration; 2] = [Duration::from_secs(15 * 60), CHARGE_FLOOR];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub pct: i64,
    pub plugged: bool,
    pub charging: bool,
    pub eta: Option<Eta>,
}

impl Battery {
    pub fn left(self) -> Option<Duration> {
        match self.eta {
            Some(Eta::Empty(left)) => Some(left),
            Some(Eta::Full(_)) | None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eta {
    Full(Duration),
    Empty(Duration),
}

const WINDOW: Duration = Duration::from_secs(30 * 60);
const MIN_SPAN: Duration = Duration::from_secs(120);
const MIN_MOVE: i64 = 2;

pub struct Trend {
    samples: Vec<(Instant, i64)>,
    plugged: bool,
    charging: Option<bool>,
}

impl Default for Trend {
    fn default() -> Self {
        Self::new()
    }
}

impl Trend {
    pub const fn new() -> Self {
        Self { samples: Vec::new(), plugged: false, charging: None }
    }

    /// Only a change of `charging` restarts the slope: a plug that does not
    /// lift the charge leaves the battery draining at the same rate.
    pub fn note(&mut self, at: Instant, pct: i64, plugged: bool, charging: bool) {
        self.plugged = plugged;
        if self.charging != Some(charging) {
            self.samples.clear();
            self.charging = Some(charging);
        }
        self.samples.retain(|(t, _)| at.duration_since(*t) <= WINDOW);
        self.samples.push((at, pct));
    }

    pub fn battery(&self) -> Option<Battery> {
        let pct = self.samples.last()?.1;
        let charging = self.charging?;
        Some(Battery { pct, plugged: self.plugged, charging, eta: self.eta(pct, charging) })
    }

    fn eta(&self, pct: i64, charging: bool) -> Option<Eta> {
        let (from, first) = *self.samples.first()?;
        let span = self.samples.last()?.0.duration_since(from);
        if span < MIN_SPAN {
            return None;
        }
        let to = |left: i64, moved: i64| {
            Duration::from_secs_f64(span.as_secs_f64() * left as f64 / moved as f64)
        };
        match pct - first {
            moved if moved <= -MIN_MOVE => Some(Eta::Empty(to(pct, -moved))),
            moved if moved >= MIN_MOVE && charging => Some(Eta::Full(to(100 - pct, moved))),
            _ => None,
        }
    }
}

pub fn recording_left(free: u64, video: Video) -> Option<Duration> {
    let spare = free.checked_sub(crate::hub::SPACE_FLOOR)?;
    match spare {
        0 => None,
        spare => Some(Duration::from_secs(spare / encode_bytes_per_sec(video))),
    }
}

#[derive(Default)]
pub struct Reserve {
    depth: [usize; 2],
}

fn depth(left: Option<Duration>) -> usize {
    RESERVE_STEPS.iter().filter(|&&step| left.is_some_and(|left| left <= step)).count()
}

impl Reserve {
    pub const fn new() -> Self {
        Self { depth: [0, 0] }
    }

    /// The steps newly crossed, the charge first.
    pub fn poll(&mut self, charge: Option<Duration>, disk: Option<Duration>) -> Vec<Low> {
        let now = [depth(charge), depth(disk)];
        let low = (0..2)
            .filter(|&k| now[k] > self.depth[k])
            .map(|k| Low { disk: k == 1, left: RESERVE_STEPS[now[k] - 1] })
            .collect();
        self.depth = now;
        low
    }
}

/// A reserve step crossed: which resource, and the step it is now under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Low {
    pub disk: bool,
    pub left: Duration,
}

/// A supply that cannot hold the hub's load drops and comes back every ~3.5 s
/// (a power bank running low, 2026-10-26), so three drops in 30 s is a flap
/// and not someone plugging a cable in and out.
const FLAP_WINDOW: Duration = Duration::from_secs(30);
const FLAP_CHANGES: usize = 6;
const FLAP_REPEAT: Duration = Duration::from_secs(30);
/// A plug change is said once it has held this long. A flapping supply that
/// is read every second can look plugged for ~2 s at most.
const SETTLE: Duration = Duration::from_secs(3);
/// A state held this long ends a flap. The longest drop seen in a flap was
/// 4.4 s, and a missed 1 s read can join two drops into ~9 s.
const STEADY: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Supply {
    Connected,
    Disconnected,
    Unstable,
}

/// Plug changes: says a steady change once, and warns every [`FLAP_REPEAT`]
/// while the supply keeps dropping. Calm for a whole window rearms the warning.
pub struct Flaps {
    plugged: Option<(bool, Instant)>,
    said: Option<bool>,
    changes: Vec<Instant>,
    warned: Option<Instant>,
}

impl Flaps {
    // `const` for the `FLAPS` static, which a `Default` impl cannot fill.
    #[allow(clippy::new_without_default)]
    pub const fn new() -> Self {
        Self { plugged: None, said: None, changes: Vec::new(), warned: None }
    }

    /// The first reading is the state at launch, and is not said.
    pub fn note(&mut self, at: Instant, plugged: bool) -> Option<Supply> {
        self.changes.retain(|&t| at.duration_since(t) <= FLAP_WINDOW);
        if self.changes.is_empty() {
            self.warned = None;
        }
        match self.plugged {
            None => self.said = Some(plugged),
            Some((was, _)) if was != plugged => self.changes.push(at),
            Some(_) => {}
        }
        if self.plugged.is_none_or(|(was, _)| was != plugged) {
            self.plugged = Some((plugged, at));
        }
        let since = self.plugged.map_or(at, |(_, since)| since);
        if at.duration_since(since) >= STEADY {
            self.changes.clear();
            self.warned = None;
        }
        if self.changes.len() >= FLAP_CHANGES {
            let due = self.warned.is_none_or(|w| at.duration_since(w) >= FLAP_REPEAT);
            if due {
                self.warned = Some(at);
            }
            return due.then_some(Supply::Unstable);
        }
        if at.duration_since(since) >= SETTLE && self.said != Some(plugged) {
            self.said = Some(plugged);
            return Some(if plugged { Supply::Connected } else { Supply::Disconnected });
        }
        None
    }
}

static FLAPS: Mutex<Flaps> = Mutex::new(Flaps::new());

pub fn note_plugged(plugged: bool) -> Option<Supply> {
    FLAPS.lock().unwrap().note(Instant::now(), plugged)
}

static TREND: Mutex<Trend> = Mutex::new(Trend::new());
static RESERVE: Mutex<Reserve> = Mutex::new(Reserve::new());

pub fn note(pct: i64, plugged: bool, charging: bool) {
    TREND.lock().unwrap().note(Instant::now(), pct, plugged, charging);
}

pub fn battery() -> Option<Battery> {
    TREND.lock().unwrap().battery()
}

pub fn charge_left() -> Option<Duration> {
    battery()?.left()
}

pub fn poll_reserve(free: Option<u64>, video: Video) -> Vec<Low> {
    let disk = free.map(|free| recording_left(free, video).unwrap_or_default());
    RESERVE.lock().unwrap().poll(charge_left(), disk)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trend(samples: &[(u64, i64)], charging: bool) -> Trend {
        let base = Instant::now();
        let mut trend = Trend::new();
        for (secs, pct) in samples {
            trend.note(base + Duration::from_secs(*secs), *pct, charging, charging);
        }
        trend
    }

    #[test]
    fn nothing_sampled_says_nothing() {
        assert_eq!(Trend::new().battery(), None);
    }

    #[test]
    fn one_reading_is_a_percentage_with_no_estimate() {
        let b = trend(&[(0, 87)], false).battery().unwrap();
        assert_eq!((b.pct, b.charging, b.eta), (87, false, None));
    }

    #[test]
    fn a_short_span_is_not_an_estimate() {
        assert_eq!(trend(&[(0, 90), (60, 86)], false).battery().unwrap().eta, None);
    }

    #[test]
    fn a_move_too_small_to_divide_by_is_not_an_estimate() {
        assert_eq!(trend(&[(0, 90), (3600, 89)], false).battery().unwrap().eta, None);
    }

    #[test]
    fn discharging_counts_down_to_empty() {
        let b = trend(&[(0, 90), (300, 85), (600, 80)], false).battery().unwrap();
        assert_eq!(b.eta, Some(Eta::Empty(Duration::from_secs(80 * 60))));
    }

    #[test]
    fn charging_counts_up_to_full() {
        let b = trend(&[(0, 60), (600, 80)], true).battery().unwrap();
        assert_eq!(b.eta, Some(Eta::Full(Duration::from_secs(600))));
    }

    #[test]
    fn a_cable_that_cannot_keep_up_still_says_how_long_is_left() {
        let b = trend(&[(0, 68), (600, 66)], true).battery().unwrap();
        assert!(b.charging, "the flag is reported as the device gave it");
        assert_eq!(b.eta, Some(Eta::Empty(Duration::from_secs(330 * 60))));
    }

    #[test]
    fn plugging_in_throws_the_discharge_slope_away() {
        let base = Instant::now();
        let mut trend = Trend::new();
        trend.note(base, 90, false, false);
        trend.note(base + Duration::from_secs(600), 80, false, false);
        trend.note(base + Duration::from_secs(660), 81, true, true);
        let b = trend.battery().unwrap();
        assert_eq!((b.pct, b.charging, b.eta), (81, true, None));
    }

    #[test]
    fn a_plug_that_does_not_charge_keeps_the_discharge_slope() {
        let base = Instant::now();
        let mut trend = Trend::new();
        trend.note(base, 90, false, false);
        trend.note(base + Duration::from_secs(600), 80, true, false);
        let b = trend.battery().unwrap();
        assert_eq!((b.plugged, b.charging), (true, false));
        assert_eq!(b.eta, Some(Eta::Empty(Duration::from_secs(80 * 60))));
    }

    #[test]
    fn a_level_rising_off_the_cable_is_not_a_charge_estimate() {
        assert_eq!(trend(&[(0, 80), (600, 84)], false).battery().unwrap().eta, None);
    }

    #[test]
    fn samples_past_the_window_stop_counting() {
        let b = trend(&[(0, 95), (3600, 94), (4200, 84)], false).battery().unwrap();
        assert_eq!(b.eta, Some(Eta::Empty(Duration::from_secs(84 * 60))));
    }

    #[test]
    fn disk_time_counts_down_to_the_floor_not_to_zero() {
        let free = crate::hub::SPACE_FLOOR + encode_bytes_per_sec(Video::H264) * 3600;
        assert_eq!(recording_left(free, Video::H264), Some(Duration::from_secs(3600)));
        assert_eq!(recording_left(crate::hub::SPACE_FLOOR, Video::H264), None);
        assert_eq!(recording_left(0, Video::H264), None);
    }

    #[test]
    fn jpeg_takes_fill_the_disk_faster() {
        let free = crate::hub::SPACE_FLOOR + 88_000_000_000;
        assert_eq!(recording_left(free, Video::Jpeg), Some(Duration::from_secs(3600)));
        assert!(recording_left(free, Video::H264).unwrap() > Duration::from_secs(4 * 3600));
    }

    fn min(m: u64) -> Option<Duration> {
        Some(Duration::from_secs(m * 60))
    }

    fn said(r: &mut Reserve, charge: Option<Duration>, disk: Option<Duration>) -> bool {
        !r.poll(charge, disk).is_empty()
    }

    #[test]
    fn a_step_names_the_resource_and_the_step() {
        let mut r = Reserve::new();
        let low = |disk, m: u64| Low { disk, left: Duration::from_secs(m * 60) };
        assert_eq!(r.poll(min(14), min(30)), [low(false, 15)]);
        assert_eq!(r.poll(min(2), min(4)), [low(false, 5), low(true, 5)]);
    }

    #[test]
    fn a_step_is_announced_once() {
        let mut r = Reserve::new();
        assert!(!said(&mut r, min(30), None), "nothing to say above the first step");
        assert!(said(&mut r, min(14), None), "crossing fifteen minutes speaks");
        for _ in 0..20 {
            assert!(!said(&mut r, min(13), None), "and does not keep speaking");
        }
        assert!(said(&mut r, min(4), None), "crossing five minutes speaks again");
        assert!(!said(&mut r, min(1), None), "and there is no third step");
    }

    #[test]
    fn falling_past_both_steps_at_once_speaks_once() {
        let mut r = Reserve::new();
        assert!(said(&mut r, min(2), None));
        assert!(!said(&mut r, min(1), None));
    }

    #[test]
    fn the_disk_speaks_on_the_same_steps() {
        let mut r = Reserve::new();
        assert!(!said(&mut r, None, min(30)));
        assert!(said(&mut r, None, min(14)));
        assert!(!said(&mut r, None, min(13)));
        assert!(said(&mut r, None, min(4)));
    }

    #[test]
    fn one_fuse_deep_does_not_silence_the_other() {
        let mut r = Reserve::new();
        assert!(said(&mut r, min(2), min(30)), "the charge, past both steps");
        assert!(said(&mut r, min(2), min(14)), "and now the disk, past its first");
    }

    #[test]
    fn a_recovered_fuse_is_warned_about_again() {
        let mut r = Reserve::new();
        assert!(said(&mut r, min(4), None));
        assert!(!said(&mut r, min(90), None), "recovering is not itself news");
        assert!(said(&mut r, min(14), None), "and the way back down speaks again");
    }

    #[test]
    fn nothing_is_said_without_an_estimate() {
        let mut r = Reserve::new();
        for _ in 0..10 {
            assert!(!said(&mut r, None, None));
        }
    }

    fn flaps() -> impl FnMut(f64, bool) -> Option<Supply> {
        let base = Instant::now();
        let mut flaps = Flaps::new();
        move |secs, plugged| flaps.note(base + Duration::from_secs_f64(secs), plugged)
    }

    #[test]
    fn a_flapping_supply_is_warned_every_30_s_and_rearms_when_calm() {
        let mut note = flaps();
        let warned: Vec<f64> = (0..50)
            .map(|k| 6.0 + k as f64 * 1.75)
            .enumerate()
            .filter_map(|(k, at)| (note(at, k % 2 == 0) == Some(Supply::Unstable)).then_some(at))
            .collect();
        assert_eq!(warned.len(), 3, "about 90 s of flapping: {warned:?}");
        assert!(warned.windows(2).all(|w| w[1] - w[0] >= 30.0), "{warned:?}");
        assert_eq!(note(130.0, true), None, "calm, and only just plugged");
        let again: Vec<_> = (0..10).map(|k| note(131.0 + k as f64 * 1.75, k % 2 == 1)).collect();
        assert!(again.contains(&Some(Supply::Unstable)), "flapping again warns again");
    }

    #[test]
    fn a_steady_plug_change_is_said_once_after_it_settles() {
        let mut note = flaps();
        assert_eq!(note(0.0, false), None, "the state at launch is not said");
        assert_eq!(note(1.0, true), None);
        assert_eq!(note(2.0, true), None);
        assert_eq!(note(4.0, true), Some(Supply::Connected));
        assert_eq!(note(5.0, true), None, "once");
        assert_eq!(note(6.0, false), None);
        assert_eq!(note(7.0, true), None, "a blip back is not said");
        assert_eq!(note(20.0, false), None);
        assert_eq!(note(23.0, false), Some(Supply::Disconnected));
    }

    #[test]
    fn a_flap_that_ends_unplugged_says_disconnected_and_stops_warning() {
        let mut note = flaps();
        note(0.0, true);
        let flap: Vec<_> =
            (0..12).filter_map(|k| note(1.0 + k as f64 * 1.75, k % 2 == 1)).collect();
        assert_eq!(flap, [Supply::Unstable]);
        let after: Vec<_> = (0..40).filter_map(|k| note(22.0 + k as f64, false)).collect();
        assert_eq!(after, [Supply::Disconnected], "no warning once it has stopped");
    }

    #[test]
    fn a_flapping_supply_does_not_say_each_drop() {
        let mut note = flaps();
        let said: Vec<_> = (0..20).filter_map(|k| note(k as f64, k % 4 == 1)).collect();
        assert!(!said.contains(&Supply::Connected), "{said:?}");
        assert!(!said.contains(&Supply::Disconnected), "{said:?}");
    }

    #[test]
    fn a_filling_battery_has_no_time_left() {
        let filling = Battery {
            pct: 9,
            plugged: true,
            charging: true,
            eta: Some(Eta::Full(Duration::from_secs(60))),
        };
        assert_eq!(filling.left(), None, "9% and filling is not 1 minute left");
        let going = Battery {
            pct: 90,
            plugged: false,
            charging: false,
            eta: Some(Eta::Empty(Duration::from_secs(60))),
        };
        assert_eq!(going.left(), Some(Duration::from_secs(60)), "the level does not gate it");
    }

    #[test]
    fn the_gate_floor_is_the_last_warning() {
        assert_eq!(RESERVE_STEPS[RESERVE_STEPS.len() - 1], CHARGE_FLOOR);
    }
}
