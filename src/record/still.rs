//! Both hands held still as a "next subtask", for tasks where both are
//! holding something when a step ends, so neither can double-click. And the
//! first motion of a step a hold started, so the step starts where the hands
//! do.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::misc::pose::Velocity;
use crate::record::click::Gesture;

/// How long both hands must stay still: 1 s, chosen by hand. Over 116 s
/// of eight takes recorded 2026-10-27..31, both hands never stayed under
/// [`LINEAR_MPS`] and [`ANGULAR_RPS`] this long.
const HOLD_NS: u64 = 1_000_000_000;

/// A controller slower than this counts as still: 3 cm/s. In those takes a
/// quarter of the samples of each hand are under 2.1 cm/s. Whether a person
/// holding an object can stay under it on purpose is untested on hardware.
const LINEAR_MPS: f64 = 0.03;

/// 0.2 rad/s, chosen with [`LINEAR_MPS`] from the same takes.
const ANGULAR_RPS: f64 = 0.2;

/// How far a jaw may move during the hold: 2°, chosen by hand. A jaw that
/// tightens or loosens its grip is a hand that is still working.
const JAW_RAD: f64 = 2.0 * PI / 180.0;

/// How long after the headset goes quiet a hold may begin, because an operator
/// who works from the spoken steps freezes to listen, and stays frozen a moment
/// after the voice ends. 0.5 s, chosen by hand.
const AFTER_SOUND_NS: u64 = 500_000_000;

/// Boottime when the cue or speech now playing goes quiet. The audio thread
/// writes it, because it alone knows how long each cue is. 0 until a sound plays.
static SOUNDING_UNTIL_NS: AtomicU64 = AtomicU64::new(0);

pub fn sounding_until(at_ns: u64) {
    SOUNDING_UNTIL_NS.store(at_ns, Ordering::Relaxed);
}

pub fn sounding_until_ns() -> u64 {
    SOUNDING_UNTIL_NS.load(Ordering::Relaxed)
}

#[derive(Default)]
pub struct Still {
    still: [bool; 2],
    jaws: [Option<f64>; 2],
    /// When both hands became still, and each jaw's angle then.
    run: Option<(u64, [Option<f64>; 2])>,
    /// A hold has fired, or a step has just started: the hands must move
    /// before the next hold counts.
    spent: bool,
}

impl Still {
    /// A controller's velocity. Fires once per hold, at the end of the hold.
    /// A hold only begins once the headset has been quiet since `sounding_until_ns`.
    pub fn hand(
        &mut self,
        side: usize,
        velocity: Velocity,
        at_ns: u64,
        sounding_until_ns: u64,
    ) -> Option<Gesture> {
        self.still[side] = speed(velocity.linear).is_some_and(|s| s < LINEAR_MPS)
            && speed(velocity.angular).is_some_and(|s| s < ANGULAR_RPS);
        if !self.still.iter().all(|&s| s) {
            self.moved();
            return None;
        }
        if self.spent {
            return None;
        }
        if sounding_until_ns > 0 && at_ns < sounding_until_ns + AFTER_SOUND_NS {
            self.run = None;
            return None;
        }
        let (since_ns, _) = *self.run.get_or_insert((at_ns, self.jaws));
        if at_ns.saturating_sub(since_ns) < HOLD_NS {
            return None;
        }
        self.run = None;
        self.spent = true;
        Some(Gesture {
            side,
            started_ns: since_ns,
            ended_ns: at_ns,
            task_end_ns: since_ns,
            task_start_ns: at_ns,
            held: true,
        })
    }

    pub fn jaw(&mut self, side: usize, angle: Option<f64>) {
        self.jaws[side] = angle;
        let start = self.run.and_then(|(_, jaws)| jaws[side]);
        if let (Some(start), Some(angle)) = (start, angle) {
            if (angle - start).abs() > JAW_RAD {
                self.moved();
            }
        }
    }

    /// A step started some other way, so a hold already running is not its end.
    pub fn restart(&mut self) {
        self.run = None;
        self.spent = true;
    }

    fn moved(&mut self) {
        self.run = None;
        self.spent = false;
    }
}

fn speed(v: Option<[f64; 3]>) -> Option<f64> {
    v.map(|[x, y, z]| (x * x + y * y + z * z).sqrt())
}

/// Where a step's hands first move. A lost velocity is not motion.
pub struct Onset {
    from_ns: u64,
    jaws: [Option<f64>; 2],
    moved_ns: Option<u64>,
}

impl Onset {
    pub fn new(from_ns: u64) -> Self {
        Self { from_ns, jaws: [None; 2], moved_ns: None }
    }

    pub fn hand(&mut self, velocity: Velocity, at_ns: u64) {
        let moving = speed(velocity.linear).is_some_and(|s| s >= LINEAR_MPS)
            || speed(velocity.angular).is_some_and(|s| s >= ANGULAR_RPS);
        if moving {
            self.saw_motion(at_ns);
        }
    }

    pub fn jaw(&mut self, side: usize, angle: Option<f64>, at_ns: u64) {
        let Some(angle) = angle.filter(|_| at_ns >= self.from_ns) else {
            return;
        };
        match self.jaws[side] {
            Some(rest) if (angle - rest).abs() > JAW_RAD => self.saw_motion(at_ns),
            Some(_) => {}
            None => self.jaws[side] = Some(angle),
        }
    }

    fn saw_motion(&mut self, at_ns: u64) {
        if at_ns >= self.from_ns && self.moved_ns.is_none() {
            self.moved_ns = Some(at_ns);
        }
    }

    pub fn moved_ns(&self) -> Option<u64> {
        self.moved_ns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn at_rest() -> Velocity {
        Velocity { linear: Some([0.01, 0.0, 0.0]), angular: Some([0.0, 0.05, 0.0]) }
    }

    fn moving() -> Velocity {
        Velocity { linear: Some([0.2, 0.0, 0.0]), angular: Some([0.0, 0.0, 0.0]) }
    }

    /// Both hands at 120 Hz from `from_ms` for `ms`, returning what fired.
    fn hold(s: &mut Still, from_ms: u64, ms: u64, v: [Velocity; 2]) -> Vec<Gesture> {
        hold_hearing(s, from_ms, ms, v, 0)
    }

    fn hold_hearing(
        s: &mut Still,
        from_ms: u64,
        ms: u64,
        v: [Velocity; 2],
        sounding_until_ns: u64,
    ) -> Vec<Gesture> {
        let mut fired = Vec::new();
        let mut t = from_ms * MS;
        while t <= (from_ms + ms) * MS {
            for (side, &v) in v.iter().enumerate() {
                fired.extend(s.hand(side, v, t, sounding_until_ns));
            }
            t += 8_333_333;
        }
        fired
    }

    #[test]
    fn a_second_of_both_hands_still_is_one_next() {
        let mut s = Still::default();
        let fired = hold(&mut s, 0, 3000, [at_rest(), at_rest()]);
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!(fired[0].task_end_ns, 0, "the step ends where the hold began");
        assert!(fired[0].task_start_ns >= HOLD_NS && fired[0].task_start_ns < HOLD_NS + 10 * MS);
    }

    #[test]
    fn one_hand_moving_is_not_a_hold() {
        let mut s = Still::default();
        assert!(hold(&mut s, 0, 3000, [at_rest(), moving()]).is_empty());
    }

    #[test]
    fn a_lost_velocity_is_not_still() {
        let mut s = Still::default();
        let lost = Velocity { linear: None, angular: None };
        assert!(hold(&mut s, 0, 3000, [at_rest(), lost]).is_empty());
    }

    #[test]
    fn a_short_pause_is_not_a_hold() {
        let mut s = Still::default();
        assert!(hold(&mut s, 0, 900, [at_rest(), at_rest()]).is_empty());
        hold(&mut s, 900, 50, [moving(), moving()]);
        assert!(hold(&mut s, 950, 900, [at_rest(), at_rest()]).is_empty());
    }

    #[test]
    fn the_hands_must_move_before_the_next_hold() {
        let mut s = Still::default();
        assert_eq!(hold(&mut s, 0, 1100, [at_rest(), at_rest()]).len(), 1);
        assert!(hold(&mut s, 1100, 3000, [at_rest(), at_rest()]).is_empty());
        hold(&mut s, 4100, 100, [moving(), at_rest()]);
        assert_eq!(hold(&mut s, 4200, 1100, [at_rest(), at_rest()]).len(), 1);
    }

    #[test]
    fn a_step_started_by_a_click_waits_for_motion() {
        let mut s = Still::default();
        hold(&mut s, 0, 500, [at_rest(), at_rest()]);
        s.restart();
        assert!(hold(&mut s, 500, 3000, [at_rest(), at_rest()]).is_empty());
    }

    #[test]
    fn freezing_to_listen_is_not_a_hold() {
        let mut s = Still::default();
        // The next step is spoken from 0 to 2.5 s, and the hands stay still
        // through it and for 1.4 s after: a hold only counts from 3 s.
        assert!(hold_hearing(&mut s, 0, 3900, [at_rest(), at_rest()], 2500 * MS).is_empty());
        let fired = hold_hearing(&mut s, 3900, 200, [at_rest(), at_rest()], 2500 * MS);
        assert_eq!(fired.len(), 1);
        assert!(fired[0].task_end_ns >= 3000 * MS, "the hold began after the voice: {fired:?}");
    }

    #[test]
    fn a_jaw_that_moves_breaks_the_hold() {
        let mut s = Still::default();
        s.jaw(0, Some(0.5));
        hold(&mut s, 0, 600, [at_rest(), at_rest()]);
        s.jaw(0, Some(0.5 + 3.0 * PI / 180.0));
        let fired = hold(&mut s, 600, 1200, [at_rest(), at_rest()]);
        assert_eq!(fired.len(), 1);
        assert!(fired[0].task_end_ns >= 600 * MS, "the hold restarted after the jaw moved");
    }

    #[test]
    fn jaw_noise_does_not_break_the_hold() {
        let mut s = Still::default();
        s.jaw(1, Some(0.5));
        hold(&mut s, 0, 600, [at_rest(), at_rest()]);
        s.jaw(1, Some(0.5 + PI / 180.0));
        assert_eq!(hold(&mut s, 600, 500, [at_rest(), at_rest()]).len(), 1);
    }

    #[test]
    fn a_step_starts_where_a_hand_first_moves() {
        let mut o = Onset::new(1000 * MS);
        o.hand(moving(), 900 * MS);
        o.hand(at_rest(), 1500 * MS);
        o.hand(Velocity::default(), 1600 * MS);
        assert_eq!(o.moved_ns(), None, "before the step, still, or lost is no motion");
        o.hand(moving(), 2400 * MS);
        o.hand(moving(), 2500 * MS);
        assert_eq!(o.moved_ns(), Some(2400 * MS));
    }

    #[test]
    fn a_jaw_moving_on_its_own_is_motion() {
        let mut o = Onset::new(1000 * MS);
        o.jaw(0, Some(0.78), 1000 * MS);
        o.jaw(0, Some(0.78 - PI / 180.0), 1500 * MS);
        assert_eq!(o.moved_ns(), None, "1° is noise");
        o.jaw(0, Some(0.70), 2000 * MS);
        assert_eq!(o.moved_ns(), Some(2000 * MS));
    }
}
