use std::collections::VecDeque;
use std::f64::consts::PI;

/// At or below this the jaw counts as shut: 3°, the production
/// `closed_thresh` of the reference gripper trigger. `calib` takes `closed`
/// from the extreme a real squeeze reaches, so every squeeze gets here.
const CLOSED_RAD: f64 = 3.0 * PI / 180.0;

/// At or above this the jaw counts as open again. The 5° gap above
/// [`CLOSED_RAD`] is the reference trigger's hysteresis, because a jaw resting
/// on one threshold chatters, and every chatter would be a click.
const OPEN_RAD: f64 = 8.0 * PI / 180.0;

/// How long the jaw must stay shut for a close to count, so a spike as the
/// grip shifts is not a click. Chosen by hand.
const MIN_CLOSED_NS: u64 = 20_000_000;

/// How long the jaw may stay shut and still be a click: 500 ms. Of 4,341
/// clicks in the start gestures of 2,170 episodes (2026-10-21..11-04), none
/// stayed shut longer than 225 ms. A longer close is a grasp, because a part
/// thin enough to shut the jaw past [`CLOSED_RAD`] holds it there, and its
/// release must not count as a click (seen on an episode of 2026-11-04).
const MAX_CLOSED_NS: u64 = 500_000_000;

/// How long a lone click waits for its partner before it is forgotten: 0.6 s,
/// the reference trigger's code value.
const CLICK_GAP_NS: u64 = 600_000_000;

/// A double-click, and the only gesture counted in clicks: a longer one would
/// make every double wait to learn that it was not that gesture's start.
const CLICKS_PER_GESTURE: u8 = 2;

/// How long after a gesture fires before another may start, so the third and
/// fourth clicks of a stutter do not make a second gesture. Chosen by hand.
const COOLDOWN_NS: u64 = 500_000_000;

/// How much aperture history each side keeps for the onset search: all of
/// [`FALL_LOOKBACK_NS`] plus the dwell that follows it.
const TRACE_NS: u64 = 1_500_000_000;

/// How far from its extreme the jaw may be and still count as there, when an
/// onset is placed: 1°, a few LSBs of the AS5601 (0.088°), so it clears
/// sensor noise and nothing else.
const ONSET_EPS_RAD: f64 = PI / 180.0;

/// The same for the opening that starts the task, which is slow at first: in
/// an episode of 2026-11-03 the jaw moved 0.09° then 0.18° before 0.53°, while
/// the video showed it opening, so 1° put the start 10 ms late. 0.25° is
/// past two LSBs, so one step of noise at rest is still not an opening.
const RISE_EPS_RAD: f64 = 0.25 * PI / 180.0;

/// How far before the stop squeeze the widest jaw is looked for: 0.7 s, the
/// lookback of the reference converter's `adjust_end_idx`.
const FALL_LOOKBACK_NS: u64 = 700_000_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gesture {
    pub side: usize,
    pub started_ns: u64,
    pub ended_ns: u64,
    pub task_end_ns: u64,
    pub task_start_ns: u64,
    /// Both hands held still (`still`), not a double-click.
    pub held: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Click,
    Gesture(Gesture),
}

#[derive(Clone, Copy)]
struct Click {
    closed_at_ns: u64,
    fall_onset_ns: u64,
    opened_at_ns: u64,
    rise_onset_ns: u64,
}

#[derive(Default)]
struct Detector {
    closed: bool,
    since_ns: u64,
    onset_ns: u64,
    trace: VecDeque<(u64, f64)>,
}

fn rise_onset(trace: &VecDeque<(u64, f64)>, from_ns: u64, to_ns: u64) -> u64 {
    let dwell: Vec<(u64, f64)> =
        trace.iter().copied().filter(|&(t, _)| t >= from_ns && t <= to_ns).collect();
    if dwell.is_empty() {
        return to_ns;
    }
    let mut at = 0;
    for (i, &(_, angle)) in dwell.iter().enumerate() {
        if angle <= dwell[at].1 {
            at = i;
        }
    }
    let floor = dwell[at].1;
    for i in at + 1..dwell.len() {
        if dwell[i].1 > floor + RISE_EPS_RAD {
            return dwell[i - 1].0;
        }
    }
    to_ns
}

/// The last instant the jaw was at its widest before it shut, so a squeeze
/// that opens the jaw first is cut at the open jaw, not partway through the close.
fn fall_onset(trace: &VecDeque<(u64, f64)>, at_ns: u64) -> u64 {
    let from_ns = at_ns.saturating_sub(FALL_LOOKBACK_NS);
    let window: Vec<(u64, f64)> =
        trace.iter().copied().filter(|&(t, _)| t >= from_ns && t <= at_ns).collect();
    if window.is_empty() {
        return at_ns;
    }
    let mut at = 0;
    for (i, &(_, angle)) in window.iter().enumerate() {
        if angle > window[at].1 {
            at = i;
        }
    }
    let peak = window[at].1;
    for i in at + 1..window.len() {
        if window[i].1 < peak - ONSET_EPS_RAD {
            return window[i - 1].0;
        }
    }
    at_ns
}

impl Detector {
    fn sample(&mut self, angle: f64, at_ns: u64) -> Option<Click> {
        self.trace.push_back((at_ns, angle));
        while self.trace.front().is_some_and(|&(t, _)| at_ns.saturating_sub(t) > TRACE_NS) {
            self.trace.pop_front();
        }
        if self.closed {
            if angle >= OPEN_RAD {
                self.closed = false;
                let shut_ns = at_ns.saturating_sub(self.since_ns);
                if (MIN_CLOSED_NS..=MAX_CLOSED_NS).contains(&shut_ns) {
                    return Some(Click {
                        closed_at_ns: self.since_ns,
                        fall_onset_ns: self.onset_ns,
                        opened_at_ns: at_ns,
                        rise_onset_ns: rise_onset(&self.trace, self.since_ns, at_ns),
                    });
                }
            }
        } else if angle <= CLOSED_RAD {
            self.closed = true;
            self.since_ns = at_ns;
            self.onset_ns = fall_onset(&self.trace, at_ns);
        }
        None
    }
}

struct Burst {
    clicks: [u8; 2],
    side: usize,
    first: Click,
    last: Click,
}

#[derive(Default)]
pub struct Trigger {
    sides: [Detector; 2],
    burst: Option<Burst>,
    cooldown_until_ns: u64,
}

impl Trigger {
    pub fn sample(&mut self, side: usize, angle: Option<f64>, at_ns: u64) -> Option<Event> {
        if self
            .burst
            .as_ref()
            .is_some_and(|b| at_ns.saturating_sub(b.last.opened_at_ns) >= CLICK_GAP_NS)
        {
            self.burst = None;
        }

        let click = match angle {
            Some(angle) => self.sides[side].sample(angle, at_ns),
            None => {
                self.sides[side] = Detector::default();
                None
            }
        };

        let click = click?;
        if at_ns < self.cooldown_until_ns {
            return None;
        }
        let burst =
            self.burst.get_or_insert(Burst { clicks: [0; 2], side, first: click, last: click });
        burst.clicks[side] += 1;
        burst.last = click;
        if burst.clicks[0].max(burst.clicks[1]) < CLICKS_PER_GESTURE {
            return Some(Event::Click);
        }

        let burst = self.burst.take()?;
        self.cooldown_until_ns = at_ns + COOLDOWN_NS;
        Some(Event::Gesture(Gesture {
            side: burst.side,
            started_ns: burst.first.closed_at_ns,
            ended_ns: burst.last.opened_at_ns,
            task_end_ns: burst.first.fall_onset_ns,
            task_start_ns: burst.last.rise_onset_ns,
            held: false,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;
    const PERIOD_NS: u64 = 5 * MS;

    const SHUT: f64 = 0.0;
    const WIDE: f64 = 0.6;

    struct Rig {
        trigger: Trigger,
        now_ns: u64,
        angle: [Option<f64>; 2],
        fired: Vec<Gesture>,
        clicks: usize,
    }

    impl Default for Rig {
        fn default() -> Self {
            Self {
                trigger: Trigger::default(),
                now_ns: 1_000_000_000,
                angle: [Some(WIDE); 2],
                fired: Vec::new(),
                clicks: 0,
            }
        }
    }

    impl Rig {
        fn advance(&mut self, ms: u64) {
            for _ in 0..(ms * MS / PERIOD_NS) {
                self.now_ns += PERIOD_NS;
                for side in 0..2 {
                    match self.trigger.sample(side, self.angle[side], self.now_ns) {
                        Some(Event::Gesture(g)) => self.fired.push(g),
                        Some(Event::Click) => self.clicks += 1,
                        None => {}
                    }
                }
            }
        }

        fn ramp(&mut self, side: usize, to: f64, ms: u64) {
            let from = self.angle[side].unwrap_or(WIDE);
            let steps = (ms * MS / PERIOD_NS).max(1);
            for i in 1..=steps {
                self.angle[side] = Some(from + (to - from) * i as f64 / steps as f64);
                self.advance(PERIOD_NS / MS);
            }
        }

        fn click(&mut self, side: usize, hold: u64) {
            self.angle[side] = Some(SHUT);
            self.advance(hold);
            self.angle[side] = Some(WIDE);
            self.advance(10);
        }

        fn click_both(&mut self, hold: u64) {
            self.angle = [Some(SHUT); 2];
            self.advance(hold);
            self.angle = [Some(WIDE); 2];
            self.advance(10);
        }

        fn settle(&mut self) {
            self.advance(1_200);
        }
    }

    #[test]
    fn two_clicks_on_one_gripper_are_a_double() {
        let mut rig = Rig::default();
        rig.click(0, 40);
        rig.click(0, 40);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "{:?}", rig.fired);
        assert_eq!(rig.fired[0].side, 0);
    }

    #[test]
    fn both_grippers_double_clicking_is_one_double_not_two() {
        let mut rig = Rig::default();
        rig.click_both(40);
        rig.click_both(40);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "a bimanual double is one gesture: {:?}", rig.fired);
    }

    #[test]
    fn a_sloppy_bimanual_double_is_still_one_double() {
        let mut rig = Rig::default();
        rig.click_both(40);
        rig.click(0, 40);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "{:?}", rig.fired);
    }

    #[test]
    fn a_double_click_fires_on_the_second_click_not_after_a_wait() {
        let mut rig = Rig::default();
        rig.click(1, 40);
        assert!(rig.fired.is_empty(), "one click is not a gesture");
        let before = rig.now_ns;
        rig.click(1, 40);
        assert_eq!(rig.fired.len(), 1, "the second click must fire it outright");
        assert!(rig.now_ns - before <= 60 * MS, "fired late");
        assert_eq!(rig.fired[0].side, 1);
    }

    #[test]
    fn a_stuttered_double_does_not_start_and_immediately_stop() {
        let mut rig = Rig::default();
        rig.click(1, 40);
        rig.click(1, 40);
        rig.click(1, 40);
        rig.click(1, 40);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "four clicks in a stutter is one gesture: {:?}", rig.fired);
    }

    #[test]
    fn a_trailing_click_after_a_gesture_is_swallowed() {
        let mut rig = Rig::default();
        rig.click(0, 40);
        rig.click(0, 40);
        rig.click(0, 40);
        rig.advance(620);
        rig.click(0, 40);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "{:?}", rig.fired);
    }

    #[test]
    fn start_with_one_hand_and_stop_with_the_other() {
        let mut rig = Rig::default();
        rig.click(0, 40);
        rig.click(0, 40);
        rig.settle();
        rig.click(1, 40);
        rig.click(1, 40);
        rig.settle();
        assert_eq!(rig.fired.len(), 2, "{:?}", rig.fired);
        assert_eq!((rig.fired[0].side, rig.fired[1].side), (0, 1));
    }

    #[test]
    fn a_squeeze_shorter_than_min_closed_is_not_a_click() {
        let mut rig = Rig::default();
        rig.click(0, 10);
        rig.click(0, 10);
        rig.settle();
        assert!(rig.fired.is_empty(), "{:?}", rig.fired);
    }

    #[test]
    fn releasing_a_grasp_is_not_a_click() {
        let mut rig = Rig::default();
        rig.click(1, 4_800);
        rig.advance(300);
        rig.click(1, 100);
        rig.settle();
        assert!(rig.fired.is_empty(), "{:?}", rig.fired);
    }

    #[test]
    fn a_grasp_on_one_hand_does_not_move_the_other_hands_double() {
        let mut rig = Rig::default();
        rig.click(1, 3_000);
        rig.advance(300);
        let first = rig.now_ns;
        rig.click(0, 100);
        rig.click(0, 100);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "{:?}", rig.fired);
        assert!(rig.fired[0].started_ns >= first, "{:?}", rig.fired);
    }

    #[test]
    fn a_jaw_resting_on_the_threshold_does_not_chatter() {
        let mut rig = Rig::default();
        for _ in 0..8 {
            rig.angle[0] = Some(CLOSED_RAD - 0.001);
            rig.advance(40);
            rig.angle[0] = Some(CLOSED_RAD + 0.001);
            rig.advance(40);
        }
        rig.settle();
        assert!(rig.fired.is_empty(), "{:?}", rig.fired);
    }

    #[test]
    fn an_uncalibrated_gripper_is_inert_and_the_other_still_fires() {
        let mut rig = Rig::default();
        rig.angle[0] = None;
        rig.click(1, 40);
        rig.click(1, 40);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "{:?}", rig.fired);
        assert_eq!(rig.fired[0].side, 1);
    }

    #[test]
    fn a_stray_single_click_does_not_block_the_gesture_after_it() {
        let mut rig = Rig::default();
        rig.click(0, 40);
        rig.advance(620);
        assert!(rig.fired.is_empty(), "{:?}", rig.fired);

        rig.click(0, 40);
        rig.click(0, 40);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "a lone click must not arm the cooldown");
    }

    #[test]
    fn a_stray_click_does_not_pair_with_one_much_later() {
        let mut rig = Rig::default();
        rig.click(0, 40);
        rig.advance(900);
        rig.click(0, 40);
        assert!(rig.fired.is_empty(), "two clicks 0.9 s apart are not a gesture");
    }

    #[test]
    fn a_second_gesture_waits_out_the_cooldown() {
        let mut rig = Rig::default();
        rig.click(0, 40);
        rig.click(0, 40);
        assert_eq!(rig.fired.len(), 1);

        rig.advance(300);
        rig.click(0, 40);
        rig.click(0, 40);
        assert_eq!(rig.fired.len(), 1, "a gesture inside the cooldown must not land");

        rig.settle();
        rig.click(0, 40);
        rig.click(0, 40);
        assert_eq!(rig.fired.len(), 2, "{:?}", rig.fired);
    }

    #[test]
    fn an_unhurried_double_click_still_reads_as_one() {
        let mut rig = Rig::default();
        rig.click(0, 60);
        rig.advance(400);
        rig.click(0, 60);
        assert_eq!(rig.fired.len(), 1, "clicks 0.4 s apart are still one gesture");
    }

    #[test]
    fn a_gesture_reports_the_span_it_occupied() {
        let mut rig = Rig::default();
        let before = rig.now_ns;
        rig.click(0, 40);
        rig.click(0, 40);
        let after = rig.now_ns;
        rig.settle();
        let g = rig.fired[0];
        assert!(g.started_ns >= before && g.ended_ns <= after);
        assert!(g.started_ns < g.ended_ns, "the span must cover both clicks");
    }

    fn a_travelling_double_click(side: usize) -> (Rig, Gesture) {
        let mut rig = Rig::default();
        rig.advance(900);
        rig.ramp(side, SHUT, 100);
        rig.advance(60);
        rig.ramp(side, WIDE, 100);
        rig.advance(60);
        rig.ramp(side, SHUT, 100);
        rig.advance(60);
        rig.ramp(side, WIDE, 100);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "{:?}", rig.fired);
        let g = rig.fired[0];
        (rig, g)
    }

    #[test]
    fn the_task_ends_before_the_gesture_that_stops_it() {
        let (_, g) = a_travelling_double_click(0);
        assert!(
            g.task_end_ns < g.started_ns,
            "the fall onset must precede the closed threshold: {} vs {}",
            g.task_end_ns,
            g.started_ns
        );
        assert!(
            g.ended_ns - g.task_end_ns > 300 * MS,
            "the whole closing gesture belongs outside the episode, got {} ms",
            (g.ended_ns - g.task_end_ns) / MS
        );
    }

    #[test]
    fn the_task_starts_before_the_gesture_that_started_it_finishes() {
        let (_, g) = a_travelling_double_click(1);
        assert!(
            g.task_start_ns < g.ended_ns,
            "the rise onset must precede the open threshold: {} vs {}",
            g.task_start_ns,
            g.ended_ns
        );
        assert!(
            g.ended_ns - g.task_start_ns >= 10 * MS,
            "nothing was recovered at the head: {} ns",
            g.ended_ns - g.task_start_ns
        );
    }

    #[test]
    fn the_four_boundaries_are_in_order() {
        let (_, g) = a_travelling_double_click(0);
        assert!(
            g.task_end_ns <= g.started_ns
                && g.started_ns <= g.task_start_ns
                && g.task_start_ns <= g.ended_ns,
            "{g:?}"
        );
    }

    #[test]
    fn a_fall_with_no_history_behind_it_ends_at_the_first_sample() {
        let mut rig = Rig::default();
        let first = rig.now_ns + PERIOD_NS;
        rig.ramp(0, SHUT, 100);
        rig.advance(60);
        rig.ramp(0, WIDE, 100);
        rig.advance(60);
        rig.ramp(0, SHUT, 100);
        rig.advance(60);
        rig.ramp(0, WIDE, 100);
        rig.settle();
        let g = rig.fired[0];
        assert_eq!(g.task_end_ns, first, "the jaw was widest when the trace began");
    }

    #[test]
    fn a_jaw_opened_just_before_the_squeeze_ends_the_task_at_its_widest() {
        let mut rig = Rig::default();
        rig.angle[0] = Some(0.075);
        rig.advance(1_000);
        rig.ramp(0, WIDE, 250);
        rig.advance(150);
        let widest = rig.now_ns;
        rig.ramp(0, SHUT, 50);
        rig.advance(60);
        rig.ramp(0, WIDE, 100);
        rig.advance(60);
        rig.ramp(0, SHUT, 100);
        rig.advance(60);
        rig.ramp(0, WIDE, 100);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "{:?}", rig.fired);
        assert_eq!(
            rig.fired[0].task_end_ns, widest,
            "a lookback that starts shut must not hide the open jaw"
        );
    }

    /// The left jaw of that 2026-11-03 episode every 5 ms around its second
    /// click, from the encoder unclamped: the squeeze went 0.88° past the
    /// sweep's `closed`, then opened 0.09°, 0.18° and 0.53°.
    #[test]
    fn the_task_starts_where_the_jaw_leaves_its_tightest_squeeze() {
        let mut rig = Rig::default();
        rig.click(0, 40);
        let degrees = [
            26.81, 18.19, 5.27, 0.35, -0.79, -0.88, -0.88, -0.88, -0.88, -0.79, -0.70, -0.35, 0.26,
            1.14, 3.95, 7.82, 12.74,
        ];
        let mut at = Vec::new();
        for d in degrees {
            rig.angle[0] = Some(d * PI / 180.0);
            rig.advance(5);
            at.push(rig.now_ns);
        }
        rig.ramp(0, WIDE, 50);
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "{:?}", rig.fired);
        assert_eq!(rig.fired[0].task_start_ns, at[10], "the last sample within 0.25° of -0.88°");
    }

    #[test]
    fn a_lone_click_is_reported_so_the_ring_can_arm() {
        let mut rig = Rig::default();
        rig.click(0, 40);
        assert_eq!(rig.clicks, 1, "the first click must be announced");
        assert!(rig.fired.is_empty());
        rig.click(0, 40);
        assert_eq!(rig.clicks, 1, "the second click is a gesture, not a click");
        assert_eq!(rig.fired.len(), 1);
    }

    #[test]
    fn an_encoder_that_drops_out_after_firing_keeps_its_gesture() {
        let mut rig = Rig::default();
        rig.click(0, 40);
        rig.click(0, 40);
        rig.angle[0] = None;
        rig.settle();
        assert_eq!(rig.fired.len(), 1, "{:?}", rig.fired);
    }
}
