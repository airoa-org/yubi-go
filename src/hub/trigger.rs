//! The gripper encoders as a trigger: their lines feed the calibration sweep
//! and the click detector, and a finished gesture becomes a task event. Where
//! the task asks for it, both hands held still are a "next" as well.

use std::sync::atomic::Ordering;
use std::time::Instant;

use super::{click, still, task, Feedback, Hub, Rec, SRC_ENCODER};
use crate::misc::clock::boottime_ns;

/// How long a click keeps the writer's pre-roll running. Longer than
/// `click::CLICK_GAP_NS` (600 ms), the wait for the click that would complete
/// a gesture, so the pre-roll outlives the click that armed it. The other
/// 300 ms is slack for the render tick that notices the arming, chosen by hand.
const ARM_NS: u64 = 900_000_000;

#[derive(Default)]
pub(super) struct Trigger {
    pub(super) clicks: click::Trigger,
    pub(super) pending: Option<click::Gesture>,
    pub(super) still: still::Still,
    pub(super) still_pending: Option<click::Gesture>,
    pub(super) armed_until_ns: Option<u64>,
    pub(super) armed: bool,
}

impl Hub {
    pub fn publish_gripper(&self, seq: Option<u64>, side: usize, device_id: &str, value: f32) {
        let at_ns = boottime_ns();
        {
            let mut s = self.state.lock().unwrap();
            s.sides[side] = Some(value);
            s.rates[SRC_ENCODER[side]].tick(Instant::now());
        }
        let (calib, unclamped, sweeping) = {
            let mut c = self.calibration.lock().unwrap();
            c.sample(side, device_id, value as f64, at_ns, crate::misc::clock::realtime_ns());
            (c.angle(side, Some(value)), c.unclamped(side, value as f64), c.prompt().is_some())
        };
        if !sweeping {
            let mut t = self.trigger.lock().unwrap();
            t.still.jaw(side, calib);
            match t.clicks.sample(side, unclamped, at_ns) {
                Some(click::Event::Click) => {
                    t.armed_until_ns = Some(at_ns + ARM_NS);
                }
                Some(click::Event::Gesture(gesture)) => {
                    t.armed_until_ns = Some(at_ns + ARM_NS);
                    t.pending = Some(gesture);
                }
                None => {}
            }
        }
        self.record(Rec::Gripper { at_ns, seq, side, raw: value as f64, calib });
    }

    pub fn publish_hand(&self, side: usize, velocity: crate::misc::pose::Velocity, at_ns: u64) {
        if self.advance_when_still.load(Ordering::Relaxed) {
            let mut t = self.trigger.lock().unwrap();
            if let Some(gesture) = t.still.hand(side, velocity, at_ns, still::sounding_until_ns()) {
                t.still_pending = Some(gesture);
            }
        }
    }

    pub fn set_advance_when_still(&self, on: bool) {
        self.advance_when_still.store(on, Ordering::Relaxed);
    }

    /// A take is on a step, not waiting after B, where an accept would retry.
    fn step_running(&self) -> bool {
        self.recording() && self.task.get().is_some_and(|task| !task.snapshot().paused)
    }

    pub fn poll_trigger(&self) -> Option<Feedback> {
        let (gesture, still, arm) = {
            let mut t = self.trigger.lock().unwrap();
            if t.pending.is_some() {
                t.still.restart();
            }
            let live = t.armed_until_ns.is_some_and(|until| boottime_ns() < until);
            let arm = (live != t.armed).then(|| {
                t.armed = live;
                live
            });
            (t.pending.take(), t.still_pending.take(), arm)
        };
        if let Some(on) = arm {
            if on {
                self.control(Rec::Arm);
                self.armed.store(true, Ordering::Relaxed);
            } else {
                self.armed.store(false, Ordering::Relaxed);
                self.control(Rec::Disarm);
            }
        }
        if let Some(gesture) = gesture {
            if !self.recording() {
                if let Some(refused) = self.refuse_start() {
                    return Some(refused);
                }
            }
            self.send_task(task::Event::Accept(gesture));
        } else if let Some(gesture) = still.filter(|_| self.step_running()) {
            log::info!("both hands still for 1 s: next subtask");
            self.send_task(task::Event::Accept(gesture));
        }
        self.task.get().and_then(|task| task.feedback())
    }
}
