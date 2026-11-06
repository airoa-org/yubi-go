use std::path::Path;

/// The finger joints' limit in `assets/yubi.urdf`, in radians; a test keeps
/// the two equal. The angle is clamped to it because the forward kinematics
/// takes no more. The travel the gripper measured is [`Side::envelope`].
pub const JOINT_LIMIT: f64 = 0.94;

/// A sweep that moved less than this (rad) did not happen: the operator
/// confirmed without touching the gripper, or the encoder is stuck. Chosen by
/// hand.
const MIN_TRAVEL: f64 = 0.10;

/// How far (rad) a jaw held against a stop may wander and still count as
/// still. Chosen by hand: wider than the encoder's noise, and tighter than any
/// move an operator makes on purpose.
const STILL_BAND: f64 = 0.02;

/// How long both sides must hold before a step captures, so passing through
/// a position does not capture it. Chosen by hand.
const STILL_TIME_NS: u64 = 1_000_000_000;

/// The least travel a sweep accepts as fully closed: 78% of the 0.77 rad both
/// PL/PR grippers measured on the 35 takes of 2026-10, so a half-hearted
/// squeeze (one stored 0.53 rad) is not taken for a full one.
const MIN_SWEEP: f64 = 0.6;

/// How long the gripper rests at its closed end before the squeeze counts,
/// so a brief partial close does not. Half the open-end hold; not measured.
const CLOSED_HOLD_NS: u64 = 500_000_000;

/// How far (rad) outside the calibrated envelope a reading may sit before it
/// counts as drift: room for a sweep that did not quite reach the stops, while
/// a reinstalled encoder still trips it. Chosen by hand.
const ENVELOPE_SLACK: f64 = 0.05;

const FILE: &str = "calibration.json";

pub fn wrap_to_pi(radians: f64) -> f64 {
    use std::f64::consts::{PI, TAU};
    (radians + PI).rem_euclid(TAU) - PI
}

#[derive(Clone, Debug, PartialEq)]
pub struct Side {
    pub device_id: String,
    pub closed: f64,
    pub open: f64,
    pub captured_realtime_ns: u64,
}

impl Side {
    fn travel(&self) -> f64 {
        wrap_to_pi(self.open - self.closed)
    }

    pub fn envelope(&self) -> f64 {
        self.travel().abs()
    }

    pub fn angle(&self, raw: f64) -> f64 {
        self.unclamped(raw).clamp(0.0, JOINT_LIMIT)
    }

    /// Below 0 where a squeeze goes past the sweep's `closed`, which the
    /// clamped angle shows as a flat 0 while the jaw already moves.
    pub fn unclamped(&self, raw: f64) -> f64 {
        wrap_to_pi(raw - self.closed) * self.travel().signum()
    }

    pub fn drift(&self, raw: f64) -> f64 {
        let angle = wrap_to_pi(raw - self.closed) * self.travel().signum();
        let over = angle - self.envelope();
        if angle < -ENVELOPE_SLACK {
            -angle
        } else if over > ENVELOPE_SLACK {
            over
        } else {
            0.0
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Calibration {
    pub sides: [Option<Side>; 2],
}

impl Calibration {
    pub fn side(&self, side: usize) -> Option<&Side> {
        self.sides[side].as_ref()
    }

    pub fn complete(&self) -> bool {
        self.sides.iter().all(Option::is_some)
    }

    fn to_json(&self) -> String {
        let entry = |s: &Option<Side>| match s {
            Some(s) => serde_json::json!({
                "device_id": s.device_id,
                "closed_rad": s.closed,
                "open_rad": s.open,
                "captured_realtime_ns": s.captured_realtime_ns,
            }),
            None => serde_json::Value::Null,
        };
        serde_json::json!({"left": entry(&self.sides[0]), "right": entry(&self.sides[1])})
            .to_string()
    }

    fn from_json(v: &serde_json::Value) -> Calibration {
        let side = |key: &str| -> Option<Side> {
            let e = &v[key];
            Some(Side {
                device_id: e["device_id"].as_str()?.to_string(),
                closed: e["closed_rad"].as_f64()?,
                open: e["open_rad"].as_f64()?,
                captured_realtime_ns: e["captured_realtime_ns"].as_u64()?,
            })
            .filter(|s| s.envelope() >= MIN_TRAVEL)
        };
        Calibration { sides: [side("left"), side("right")] }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prompt {
    Squeeze,
    Release,
}

impl Prompt {
    pub fn text(self) -> &'static str {
        match self {
            Prompt::Squeeze => "Close both grippers fully",
            Prompt::Release => "Open both fully and hold them still",
        }
    }
}

/// Why a sweep could not finish, per side (0 left, 1 right).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Problem {
    Silent(usize),
    Barely(usize, f64),
    Wrapped(usize, f64),
}

impl Problem {
    pub fn text(self) -> String {
        let name = |side: usize| if side == 0 { "left" } else { "right" };
        match self {
            Problem::Silent(s) => format!("{} gripper: no data", name(s)),
            Problem::Barely(s, rad) => format!("{} gripper barely moved ({rad:.2} rad)", name(s)),
            Problem::Wrapped(s, rad) => {
                format!("{} gripper: past half a turn ({rad:.2} rad)", name(s))
            }
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Track {
    previous: Option<f64>,
    continuous: f64,
    raw: f64,
    still_since: Option<u64>,
    still_from: f64,
    lo: f64,
    lo_raw: f64,
    hi: f64,
    hi_raw: f64,
    closed_is_lo: Option<bool>,
    /// The first reading. A gripper rests open (spring-loaded), so the
    /// closed end is the one away from here.
    start: f64,
}

impl Track {
    fn sample(&mut self, v: f64, at_ns: u64) {
        match self.previous {
            Some(previous) => self.continuous += wrap_to_pi(v - previous),
            None => {
                self.continuous = v;
                self.start = v;
                self.lo = v;
                self.hi = v;
                self.lo_raw = v;
                self.hi_raw = v;
            }
        }
        self.previous = Some(v);
        self.raw = v;

        if self.continuous < self.lo {
            self.lo = self.continuous;
            self.lo_raw = v;
        }
        if self.continuous > self.hi {
            self.hi = self.continuous;
            self.hi_raw = v;
        }
        let left_band =
            self.still_since.is_none() || wrap_to_pi(v - self.still_from).abs() > STILL_BAND;
        if left_band {
            self.still_since = Some(at_ns);
            self.still_from = v;
        }

        let closed_is_lo = self.start - self.lo > self.hi - self.start;
        let closed = if closed_is_lo { self.lo } else { self.hi };
        let resting = at_ns - self.still_since.unwrap_or(at_ns) >= CLOSED_HOLD_NS;
        if self.closed_is_lo.is_none()
            && self.hi - self.lo >= MIN_SWEEP
            && (self.continuous - closed).abs() <= STILL_BAND
            && resting
        {
            self.closed_is_lo = Some(closed_is_lo);
        }
    }

    fn span(&self) -> f64 {
        self.hi - self.lo
    }

    fn settled_open(&self) -> bool {
        let Some(closed_is_lo) = self.closed_is_lo else { return false };
        let open = if closed_is_lo { self.hi } else { self.lo };
        (self.continuous - open).abs() <= STILL_BAND
    }

    fn closed_raw(&self) -> f64 {
        if self.closed_is_lo == Some(true) {
            self.lo_raw
        } else {
            self.hi_raw
        }
    }

    fn held(&self, now_ns: u64) -> f32 {
        let since = self.still_since.unwrap_or(now_ns);
        (now_ns.saturating_sub(since) as f32 / STILL_TIME_NS as f32).min(1.0)
    }
}

#[derive(Default)]
pub struct Sweep {
    track: [Option<Track>; 2],
    ids: [Option<String>; 2],
}

impl Sweep {
    pub fn new() -> Sweep {
        Sweep::default()
    }

    pub fn prompt(&self) -> Prompt {
        let squeezed =
            |side: usize| self.track[side].is_some_and(|t: Track| t.closed_is_lo.is_some());
        if (0..2).all(squeezed) {
            Prompt::Release
        } else {
            Prompt::Squeeze
        }
    }

    pub fn sample(&mut self, side: usize, device_id: &str, raw: f64, at_ns: u64) {
        self.track[side].get_or_insert_with(Track::default).sample(raw, at_ns);
        self.ids[side] = Some(device_id.to_string());
    }

    pub fn held(&self, now_ns: u64) -> [f32; 2] {
        [0, 1].map(|side| match self.track[side] {
            Some(t) if t.settled_open() => t.held(now_ns),
            _ => 0.0,
        })
    }

    pub fn ready(&self, now_ns: u64) -> bool {
        (0..2)
            .all(|side| self.track[side].is_some_and(|t| t.settled_open() && t.held(now_ns) >= 1.0))
    }

    fn confirm(&mut self, now_realtime_ns: u64) -> Result<Calibration, Vec<Problem>> {
        let mut sides = [None, None];
        let mut bad = Vec::new();
        for (side, slot) in sides.iter_mut().enumerate() {
            let t = self.track[side].unwrap();
            let s = Side {
                device_id: self.ids[side].clone().unwrap_or_default(),
                closed: t.closed_raw(),
                open: t.raw,
                captured_realtime_ns: now_realtime_ns,
            };
            if s.envelope() < MIN_SWEEP {
                bad.push(Problem::Barely(side, s.envelope()));
            } else if t.span() >= std::f64::consts::PI {
                bad.push(Problem::Wrapped(side, t.span()));
            } else {
                *slot = Some(s);
            }
        }
        if !bad.is_empty() {
            return Err(bad);
        }
        Ok(Calibration { sides })
    }
}

#[derive(Default)]
pub struct Session {
    active: Calibration,
    sweep: Option<Sweep>,
    message: Vec<Problem>,
    pending: Option<Calibration>,
}

impl Session {
    pub fn load(dir: &str) -> Session {
        let path = Path::new(dir).join(FILE);
        let active = match std::fs::read_to_string(&path) {
            Err(_) => Calibration::default(),
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(v) => Calibration::from_json(&v),
                Err(e) => {
                    log::error!("{path:?} is not valid JSON ({e}) — treating as uncalibrated");
                    Calibration::default()
                }
            },
        };
        Session { active, ..Session::default() }
    }

    pub fn active(&self) -> &Calibration {
        &self.active
    }

    pub fn angle(&self, side: usize, raw: Option<f32>) -> Option<f64> {
        let raw = raw? as f64;
        self.active.side(side).map(|s| s.angle(raw))
    }

    pub fn unclamped(&self, side: usize, raw: f64) -> Option<f64> {
        self.active.side(side).map(|s| s.unclamped(raw))
    }

    pub fn prompt(&self) -> Option<Prompt> {
        self.sweep.as_ref().map(Sweep::prompt)
    }

    pub fn held(&self, now_ns: u64) -> [f32; 2] {
        self.sweep.as_ref().map_or([0.0; 2], |s| s.held(now_ns))
    }

    pub fn take_finished(&mut self) -> Option<Calibration> {
        self.pending.take()
    }

    pub fn message(&self) -> &[Problem] {
        &self.message
    }

    pub fn sample(&mut self, side: usize, device_id: &str, raw: f64, at_ns: u64, realtime_ns: u64) {
        let Some(sweep) = self.sweep.as_mut() else { return };
        sweep.sample(side, device_id, raw, at_ns);
        if !sweep.ready(at_ns) {
            return;
        }
        self.message.clear();
        match sweep.confirm(realtime_ns) {
            Ok(calibration) => {
                self.sweep = None;
                self.active = calibration.clone();
                self.pending = Some(calibration);
            }
            Err(reasons) => {
                let why: Vec<String> = reasons.iter().map(|r| r.text()).collect();
                log::warn!("calibration refused: {}", why.join("; "));
                self.sweep = None;
                self.message = reasons;
            }
        }
    }

    /// Start a sweep, or abandon the one running. `live` is whether each
    /// gripper is sending; a sweep cannot finish without both, so a start
    /// with one silent is refused and says which.
    pub fn toggle(&mut self, live: [bool; 2]) {
        self.message.clear();
        if self.sweep.take().is_some() {
            return;
        }
        self.message = silent(live);
        if self.message.is_empty() {
            self.sweep = Some(Sweep::new());
        }
    }

    /// End a sweep whose gripper stopped sending, rather than wait for a hand
    /// that can no longer be seen.
    pub fn watch(&mut self, live: [bool; 2]) {
        let silent = silent(live);
        if self.sweep.is_some() && !silent.is_empty() {
            log::warn!("calibration abandoned: a gripper stopped sending");
            self.sweep = None;
            self.message = silent;
        }
    }
}

fn silent(live: [bool; 2]) -> Vec<Problem> {
    (0..2).filter(|&side| !live[side]).map(Problem::Silent).collect()
}

pub fn store(dir: &str, calibration: &Calibration) -> std::io::Result<()> {
    crate::misc::save::replace(&Path::new(dir).join(FILE), calibration.to_json().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_joint_limit_is_the_urdfs() {
        use crate::misc::mount::{JointKind, JOINTS};
        let fingers: Vec<_> = JOINTS.iter().filter(|j| j.kind == JointKind::Revolute).collect();
        assert_eq!(fingers.len(), 4, "two fingers per hand");
        for j in fingers {
            let limit = j.limits[0].abs().max(j.limits[1].abs());
            assert_eq!(limit, JOINT_LIMIT, "{}", j.child);
        }
    }

    const DT: u64 = 5_000_000;

    struct Rig {
        session: Session,
        now: u64,
        at: [f64; 2],
    }

    impl Rig {
        fn new(open: [f64; 2]) -> Rig {
            let mut session = Session::default();
            session.toggle([true; 2]);
            Rig { session, now: 1_000_000_000, at: open }
        }

        fn move_to(&mut self, to: [f64; 2]) -> &mut Rig {
            let (from, steps) = (self.at, 40);
            for i in 1..=steps {
                let t = i as f64 / steps as f64;
                let at = [0, 1].map(|s| {
                    (from[s] + wrap_to_pi(to[s] - from[s]) * t).rem_euclid(std::f64::consts::TAU)
                });
                self.feed(at);
            }
            self.at = to;
            self
        }

        fn turn(&mut self, by: f64) -> &mut Rig {
            let (from, steps) = (self.at, (by.abs() / 0.05).ceil() as usize);
            for i in 1..=steps {
                let t = i as f64 / steps as f64;
                let at = [0, 1].map(|s| (from[s] + by * t).rem_euclid(std::f64::consts::TAU));
                self.feed(at);
            }
            self.at = [0, 1].map(|s| (from[s] + by).rem_euclid(std::f64::consts::TAU));
            self
        }

        fn hold(&mut self, ms: u64) -> &mut Rig {
            for _ in 0..(ms * 1_000_000 / DT) {
                let at = self.at;
                self.feed(at);
            }
            self
        }

        fn feed(&mut self, at: [f64; 2]) {
            for (side, &raw) in at.iter().enumerate() {
                let id = if side == 0 { "PL004" } else { "PR004" };
                self.session.sample(side, id, raw, self.now, 7);
            }
            self.now += DT;
        }
    }

    fn sweep(closed: [f64; 2], open: [f64; 2]) -> Result<Calibration, Vec<Problem>> {
        let mut rig = Rig::new(open);
        rig.hold(200).move_to(closed).hold(600).move_to(open).hold(1_200);
        match rig.session.take_finished() {
            Some(c) => Ok(c),
            None => Err(rig.session.message().to_vec()),
        }
    }

    #[test]
    fn the_hardware_case_a_running_maximum_needed_unwrapping_for() {
        use std::f64::consts::TAU;
        let c = sweep([0.6, 0.6], [TAU - 0.083, TAU - 0.083]).unwrap();
        let left = c.side(0).unwrap();

        let readings = [0.6, 0.48, 0.36, 0.24, 0.12, 0.06, TAU - 0.003, TAU - 0.033, TAU - 0.083];
        let angles: Vec<f64> = readings.iter().map(|&r| left.angle(r)).collect();
        for pair in angles.windows(2) {
            let step = pair[1] - pair[0];
            assert!(
                (0.0..0.15).contains(&step),
                "angle jumped by {step} across the wrap: {angles:?}"
            );
        }
        assert!(angles.last().unwrap() > &0.2, "travel was lost: {angles:?}");
        assert!(left.angle(0.6).abs() < 1e-12);
    }

    #[test]
    fn wrap_to_pi_takes_the_short_way_round() {
        use std::f64::consts::{PI, TAU};
        assert!((wrap_to_pi(0.1) - 0.1).abs() < 1e-12);
        assert!((wrap_to_pi(TAU - 0.1) + 0.1).abs() < 1e-12, "must go backwards, not forwards");
        assert!((wrap_to_pi(-TAU + 0.1) - 0.1).abs() < 1e-12);
        assert!(wrap_to_pi(PI).abs() - PI < 1e-12);
    }

    #[test]
    fn a_reading_outside_the_envelope_is_reported_as_drift() {
        let c = sweep([1.0, 1.0], [1.8, 1.8]).unwrap();
        let left = c.side(0).unwrap();
        assert_eq!(left.drift(1.4), 0.0, "mid-travel");
        assert_eq!(left.drift(1.0), 0.0);
        assert_eq!(left.drift(1.82), 0.0, "just past open, inside the slack");
        assert!((left.drift(1.9) - 0.1).abs() < 1e-12, "past open");
        assert!((left.drift(0.9) - 0.1).abs() < 1e-12, "past closed the other way");
    }

    #[test]
    fn a_spring_loaded_gripper_does_not_come_out_inverted() {
        let (rest, squeezed) = ([2.3669, 5.6604], [3.1370, 0.1626]);
        let c = sweep(squeezed, rest).unwrap();

        for side in 0..2 {
            let s = c.side(side).unwrap();
            assert!((s.closed - squeezed[side]).abs() < 1e-6, "closed is the squeeze: {s:?}");
            assert!((s.open - rest[side]).abs() < 1e-6, "open is the rest position: {s:?}");
            assert!(s.angle(rest[side]) > 0.7, "resting jaws must read open");
            assert!(s.angle(squeezed[side]).abs() < 1e-9, "a squeeze must read shut");
        }
    }

    #[test]
    fn a_release_that_settles_short_of_the_peak_is_still_accepted() {
        let mut rig = Rig::new([1.8, 1.8]);
        rig.hold(200).move_to([1.0, 1.0]).hold(600).move_to([1.79, 1.79]).hold(1_200);

        let c = rig.session.take_finished().expect("a real release must calibrate");
        for side in 0..2 {
            let s = c.side(side).unwrap();
            assert!((s.envelope() - 0.79).abs() < 1e-6, "{s:?}");
            assert!(s.angle(1.79) > 0.7, "resting jaws read open");
            assert!(s.angle(1.0).abs() < 1e-9, "a squeeze reads shut");
        }
    }

    #[test]
    fn a_partial_release_keeps_waiting_rather_than_capturing() {
        let mut rig = Rig::new([1.8, 1.8]);
        rig.hold(200).move_to([1.0, 1.0]).hold(600);
        assert_eq!(rig.session.prompt(), Some(Prompt::Release), "the squeeze was seen");

        rig.move_to([1.4, 1.4]).hold(3_000);
        assert_eq!(rig.session.prompt(), Some(Prompt::Release), "still asking");
        assert!(rig.session.take_finished().is_none(), "and nothing was persisted");
    }

    #[test]
    fn a_brief_close_is_not_a_squeeze() {
        let mut rig = Rig::new([1.8, 1.8]);
        rig.hold(200).move_to([1.0, 1.0]).hold(300).move_to([1.8, 1.8]).hold(1_200);
        assert_eq!(rig.session.prompt(), Some(Prompt::Squeeze), "0.3 s at the closed end");
        assert!(rig.session.take_finished().is_none());
    }

    #[test]
    fn a_partial_close_is_not_a_squeeze_however_long_it_is_held() {
        let mut rig = Rig::new([1.8, 1.8]);
        rig.hold(200).move_to([1.3, 1.3]).hold(3_000);
        assert_eq!(rig.session.prompt(), Some(Prompt::Squeeze), "0.5 rad is under the sweep");
        rig.move_to([1.0, 1.0]).hold(600);
        assert_eq!(rig.session.prompt(), Some(Prompt::Release), "closing the rest counts");
    }

    #[test]
    fn holding_the_squeeze_does_not_capture() {
        let mut rig = Rig::new([1.8, 1.8]);
        rig.hold(200).move_to([1.0, 1.0]).hold(3_000);
        assert!(rig.session.take_finished().is_none());
        assert_eq!(rig.session.held(rig.now), [0.0, 0.0], "and the bars do not promise one");
    }

    #[test]
    fn a_gripper_nobody_has_picked_up_yet_does_not_capture() {
        let mut rig = Rig::new([1.8, 1.8]);
        rig.hold(8_000);
        assert_eq!(rig.session.prompt(), Some(Prompt::Squeeze), "still waiting to be squeezed");
        assert!(rig.session.take_finished().is_none());
    }

    #[test]
    fn the_capture_waits_for_both_hands() {
        let mut rig = Rig::new([1.8, 1.8]);
        rig.hold(200).move_to([1.0, 1.0]).hold(600);
        for _ in 0..300 {
            rig.session.sample(0, "PL004", 1.8, rig.now, 7);
            rig.session.sample(1, "PR004", 1.0, rig.now, 7);
            rig.now += DT;
        }
        assert_eq!(rig.session.held(rig.now)[0], 1.0, "left is home and holding");
        assert_eq!(rig.session.held(rig.now)[1], 0.0, "right is still squeezed");
        assert!(rig.session.take_finished().is_none());
    }

    #[test]
    fn a_silent_gripper_refuses_the_start_and_says_which() {
        let mut session = Session::default();
        session.toggle([true, false]);
        assert_eq!(session.prompt(), None, "no sweep that could never finish");
        assert_eq!(session.message(), [Problem::Silent(1)]);
        session.toggle([false, false]);
        assert_eq!(session.message(), [Problem::Silent(0), Problem::Silent(1)]);
    }

    #[test]
    fn a_gripper_that_stops_mid_sweep_ends_it() {
        let mut rig = Rig::new([1.8, 1.8]);
        rig.hold(200).move_to([1.0, 1.0]).hold(600);
        rig.session.watch([true, true]);
        assert_eq!(rig.session.prompt(), Some(Prompt::Release), "both sending: carry on");
        rig.session.watch([false, true]);
        assert_eq!(rig.session.prompt(), None);
        assert_eq!(rig.session.message(), [Problem::Silent(0)]);
        assert!(rig.session.take_finished().is_none());
    }

    #[test]
    fn travel_past_half_a_turn_is_caught_at_calibration_time() {
        let mut rig = Rig::new([0.0, 0.0]);
        rig.hold(200).turn(4.0).hold(600).turn(-4.0).hold(1_200);
        let e = rig.session.message();
        assert!(matches!(e[0], Problem::Wrapped(..)), "{e:?}");
        let text = e[0].text();
        assert!(text.chars().count() <= 42, "too long for the panel: {text:?}");
        assert!(rig.session.take_finished().is_none());
        assert_eq!(rig.session.prompt(), None, "abandoned, not quietly restarted");
    }

    #[test]
    fn one_press_and_one_gesture_produce_a_calibration() {
        let mut rig = Rig::new([1.8, 1.8]);
        assert_eq!(rig.session.prompt(), Some(Prompt::Squeeze), "the press put it on screen");
        assert!(Prompt::Release.text().contains("Open both"));

        rig.hold(200).move_to([1.0, 1.0]).hold(600);
        assert_eq!(rig.session.prompt(), Some(Prompt::Release));

        rig.move_to([1.8, 1.8]).hold(500);
        let held = rig.session.held(rig.now);
        assert!((0.3..0.8).contains(&held[0]), "the panel can show progress: {held:?}");

        rig.hold(700);
        let c = rig.session.take_finished().expect("the hold finishes it");
        assert!(c.complete());
        assert_eq!(rig.session.prompt(), None, "and the panel goes back to normal");
        assert!(rig.session.message().is_empty());
    }

    #[test]
    fn the_button_starts_and_abandons_but_never_confirms() {
        let mut rig = Rig::new([1.8, 1.8]);
        rig.hold(200).move_to([1.0, 1.0]).hold(600);
        assert_eq!(rig.session.prompt(), Some(Prompt::Release), "mid-gesture");

        rig.session.toggle([true; 2]);
        assert_eq!(rig.session.prompt(), None, "abandoned");
        rig.session.toggle([true; 2]);
        assert_eq!(rig.session.prompt(), Some(Prompt::Squeeze), "and starts over from the squeeze");

        assert!(rig.session.take_finished().is_none());
        assert_eq!(*rig.session.active(), Calibration::default());
    }

    #[test]
    fn starting_over_clears_the_last_complaint() {
        let mut session = Session::default();
        session.toggle([true, false]);
        assert_eq!(session.message(), [Problem::Silent(1)]);

        session.toggle([true; 2]);
        assert!(session.message().is_empty());
        assert_eq!(session.prompt(), Some(Prompt::Squeeze));
    }

    #[test]
    fn a_calibration_survives_a_round_trip_and_absence_reads_as_uncalibrated() {
        let dir = std::env::temp_dir().join(format!("yubigo_calib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_str().unwrap();

        let empty = Session::load(dir);
        assert_eq!(*empty.active(), Calibration::default());
        assert!(!empty.active().complete());
        assert_eq!(empty.angle(0, Some(1.4)), None, "not Some(0.0)");

        let c = sweep([1.0, 2.0], [1.8, 2.9]).unwrap();
        store(dir, &c).unwrap();
        assert!(c.complete());
        let reloaded = Session::load(dir);
        assert_eq!(*reloaded.active(), c, "read back byte-identical");
        assert!((reloaded.angle(0, Some(1.4)).unwrap() - 0.4).abs() < 1e-6);
    }

    #[test]
    fn a_half_written_entry_is_not_a_calibration() {
        let partial = serde_json::json!({
            "left": {"device_id": "PL004", "closed_rad": 1.0},
            "right": {"device_id": "PR004", "closed_rad": 1.0, "open_rad": 1.01,
                      "captured_realtime_ns": 7},
        });
        let c = Calibration::from_json(&partial);
        assert!(c.side(0).is_none(), "missing open_rad");
        assert!(c.side(1).is_none(), "complete, but its envelope is noise");
    }
}
