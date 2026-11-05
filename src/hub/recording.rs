//! Takes: starting and stopping them through the task worker, and the
//! recording state the writer reports back.

use std::sync::atomic::Ordering;

use super::{task, Hub, Rec};
use crate::misc::clock::boottime_ns;

impl Hub {
    /// Only during a take: a finished take may already be uploaded with
    /// `success: true`, and a later local edit would never reach the bucket.
    pub fn mark_failed(&self) {
        if self.recording() {
            self.send_task(task::Event::Fail(boottime_ns()));
        }
    }

    pub(crate) fn task_view(&self) -> task::View {
        self.task.get().map(|task| task.snapshot()).unwrap_or_default()
    }

    pub fn task_display(&self) -> task::Display {
        let Some(task) = self.task.get() else { return Default::default() };
        let snapshot = task.snapshot();
        let view = snapshot.core;
        let mut display = view.display;
        if self.recording() && view.recording {
            let delta = (boottime_ns() as f64 / 1e9 - view.sampled_at).max(0.0);
            if !snapshot.paused {
                display.subtask_elapsed_sec += delta;
            }
            display.episode_elapsed_sec += delta;
        }
        display
    }

    pub(crate) fn send_task(&self, event: task::Event) -> bool {
        self.task.get().is_some_and(|task| task.send(event))
    }

    pub fn set_recording(&self, on: bool) -> bool {
        if on && self.refuse_start().is_some() {
            return false;
        }
        if !on {
            self.control(Rec::Pause);
        }
        self.send_task(if on { task::Event::Start } else { task::Event::Stop(true) })
    }

    pub fn stop_and_wait(&self) {
        self.control(Rec::Pause);
        if let Some(task) = self.task.get() {
            task.stop_and_wait();
        }
        let _ = self.record_call(Rec::Shutdown);
    }

    pub fn recording(&self) -> bool {
        self.state.lock().unwrap().rec.active
    }

    /// A take is recording or still open in the writer. Stopping clears
    /// `recording` at once, for the panel, while the take closes after it;
    /// whatever reads or moves the takes waits for this instead.
    pub fn busy(&self) -> bool {
        self.recording() || self.take_open.load(Ordering::Relaxed)
    }

    pub(crate) fn set_take_open(&self, open: bool) {
        self.take_open.store(open, Ordering::Relaxed);
    }

    pub(crate) fn capturing(&self) -> bool {
        self.armed.load(Ordering::Relaxed) || self.recording()
    }

    pub fn recording_failed(&self) -> bool {
        self.state.lock().unwrap().rec.failed
    }

    pub(crate) fn dropped(&self) -> u64 {
        self.state.lock().unwrap().rec.dropped
    }

    pub(crate) fn record_started(&self) {
        let mut s = self.state.lock().unwrap();
        s.rec.active = true;
        s.rec.failed = false;
        s.rec.refused = false;
        s.rec.refused_why.clear();
        s.rec.refused_by_core = None;
        s.rec.core_block = None;
        s.rec.dropped = 0;
    }

    pub(crate) fn record_stopped(&self, failed: bool) {
        let mut s = self.state.lock().unwrap();
        s.rec.active = false;
        s.rec.failed = failed;
        self.trigger.lock().unwrap().armed_until_ns = None;
    }
}
