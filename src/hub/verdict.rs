//! The gate's side of the hub: what it reads, the verdict it hands back, and
//! what follows from that verdict. A start is refused with reasons, and a take
//! is faulted, stopped or cancelled.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use log::warn;

use super::core_reason::core_reason;
use super::{gate, task, Fault, Feedback, GroupVerdict, Hub, Reason, Rec, Roles};
use crate::misc::clock::boottime_ns;

/// How often the cue repeats while the fault holds. Chosen by hand: long
/// enough not to nag through a demonstration, short enough that the operator
/// hears it before walking back to the rig.
const FAULT_REPEAT: Duration = Duration::from_secs(10);

impl Hub {
    /// The reasons are read once, here, because the panel asks for them every
    /// frame and `ready()` costs a statvfs on /sdcard.
    pub(crate) fn task_refused(&self, refused: bool) {
        let gate: Vec<Reason> = match refused {
            true => self.ready().err().unwrap_or_default(),
            false => Vec::new(),
        };
        let mut s = self.state.lock().unwrap();
        let core = s.rec.core_block.take();
        (s.rec.refused_why, s.rec.refused_by_core) = match (refused, gate.is_empty(), core) {
            (true, true, Some(reason)) => (Vec::new(), Some(reason)),
            _ => (gate, None),
        };
        s.rec.refused = refused;
    }

    pub(crate) fn core_blocked(&self, reason: &str) {
        self.state.lock().unwrap().rec.core_block = Some(reason.to_string());
    }

    /// The reasons the current verdict holds against a take.
    pub(crate) fn standing_faults(&self) -> Vec<Reason> {
        self.verdict.lock().unwrap().as_ref().map(gate::verdict_reasons).unwrap_or_default()
    }

    pub fn ready(&self) -> Result<(), Vec<Reason>> {
        match self.gate_level() {
            gate::OK => Ok(()),
            _ => {
                let mut reasons = gate::start_reasons(&self.gate_inputs());
                if reasons.is_empty() {
                    let fault = match crate::misc::core::up() {
                        true => Fault::Settling,
                        false => Fault::CoreDown,
                    };
                    reasons.push(Reason { source: "gate", fault });
                }
                Err(reasons)
            }
        }
    }

    pub(crate) fn gate_inputs(&self) -> gate::Inputs {
        gate::Inputs {
            stats: self.stats(),
            calibrated: self.calibration.lock().unwrap().active().complete(),
            has_task: !self.task_missing.load(Ordering::Relaxed),
            head_absent: self.head_absent.load(Ordering::Relaxed),
            cameras_matched: self.cameras_matched.load(Ordering::Relaxed),
            free: self.free_bytes(),
            charge: crate::misc::power::charge_left(),
            subtask_elapsed: self
                .recording()
                .then(|| Duration::from_secs_f64(self.task_display().subtask_elapsed_sec.max(0.0))),
        }
    }

    fn free_bytes(&self) -> Option<u64> {
        crate::misc::disk::usage(&self.dir()).map(|u| u.free)
    }

    pub fn gate_groups(&self) -> Vec<GroupVerdict> {
        self.verdict.lock().unwrap().as_ref().map(|v| v.groups.clone()).unwrap_or_default()
    }

    pub fn refusal(&self) -> Vec<String> {
        if let Some(reason) = &self.state.lock().unwrap().rec.refused_by_core {
            return core_reason(reason);
        }
        // Every reason `ready()` gives is conditioned on the gate, so a gate
        // that reopens has already dismissed them.
        if self.gate_level() == gate::OK {
            return Vec::new();
        }
        self.state.lock().unwrap().rec.refused_why.iter().map(|r| r.shown()).collect()
    }

    /// The first reason of the last refusal, in the gate's order. `None`
    /// when core refused for a reason of its own.
    pub fn refused_first(&self) -> Option<Reason> {
        self.state.lock().unwrap().rec.refused_why.first().copied()
    }

    /// What blocks a start now, or during a take what the gate faults it
    /// for, first reason only. `None` means ready.
    pub fn readiness(&self) -> Option<Reason> {
        match self.recording() {
            true => self.standing_faults().first().copied(),
            false => self.ready().err().and_then(|reasons| reasons.first().copied()),
        }
    }

    /// What a take does about its faults and a finished calibration sweep,
    /// on the gate's tick, so neither waits for the panel to be drawn.
    pub(crate) fn tick_controls(&self) {
        if let Some(cue) = self.poll_fault() {
            *self.fault_cue.lock().unwrap() = Some(cue);
        }
        self.poll_calibration();
    }

    /// The fault or time-warning cue raised since the last call.
    pub fn take_fault_cue(&self) -> Option<Feedback> {
        self.fault_cue.lock().unwrap().take()
    }

    fn poll_fault(&self) -> Option<Feedback> {
        let now = Instant::now();
        if !self.recording() {
            *self.fault_spoke.lock().unwrap() = None;
            self.time_warned.store(false, Ordering::Relaxed);
            return None;
        }
        let reasons = self.standing_faults();
        let gutted = reasons.iter().any(|reason| reason.fault.cancels_the_take())
            && !self.keep_faulted_episodes.load(Ordering::Relaxed);
        if gutted {
            for reason in reasons.iter().filter(|r| r.fault.cancels_the_take()) {
                warn!("episode cancelled: {}", reason.text());
            }
            *self.last_fault.lock().unwrap() =
                reasons.iter().find(|r| r.fault.cancels_the_take()).copied();
            self.cancel_recording();
            *self.fault_spoke.lock().unwrap() = None;
            return None;
        }
        if reasons.iter().any(|r| r.fault.stops_the_take()) {
            for reason in reasons.iter().filter(|r| r.fault.stops_the_take()) {
                warn!("episode stopped: {}", reason.text());
            }
            *self.last_fault.lock().unwrap() =
                reasons.iter().find(|r| r.fault.stops_the_take()).copied();
            self.control(Rec::Pause);
            self.send_task(task::Event::Stop(false));
            *self.fault_spoke.lock().unwrap() = None;
            return None;
        }
        let elapsed = Duration::from_secs_f64(self.task_display().subtask_elapsed_sec.max(0.0));
        let due = elapsed >= gate::SUBTASK_WARN;
        if !due {
            self.time_warned.store(false, Ordering::Relaxed);
        } else if !self.time_warned.swap(true, Ordering::Relaxed) {
            return Some(Feedback::TimeWarning);
        }
        if reasons.is_empty() {
            *self.fault_spoke.lock().unwrap() = None;
            return None;
        }
        let mut spoke = self.fault_spoke.lock().unwrap();
        if spoke.is_some_and(|at| now.duration_since(at) < FAULT_REPEAT) {
            return None;
        }
        *spoke = Some(now);
        for reason in &reasons {
            warn!("episode fault: {}", reason.text());
        }
        *self.last_fault.lock().unwrap() = reasons.first().copied();
        Some(Feedback::Fault)
    }

    /// The reason behind the fault, cancel or stop just announced, once.
    pub fn take_last_fault(&self) -> Option<Reason> {
        self.last_fault.lock().unwrap().take()
    }

    fn cancel_recording(&self) {
        self.control(Rec::Cancel);
        self.send_task(task::Event::Cancel);
    }

    /// Latches the refusal and names what failed, so a start that goes nowhere
    /// is not silence. `None` means the gate is willing.
    pub(super) fn refuse_start(&self) -> Option<Feedback> {
        self.ready().err()?;
        self.task_refused(true);
        warn!("start refused: {}", self.refusal().join(", "));
        Some(Feedback::Refused)
    }

    pub fn set_has_task(&self, has_task: bool) {
        self.task_missing.store(!has_task, Ordering::Relaxed);
    }

    pub fn cameras_matched(&self) -> bool {
        self.cameras_matched.load(Ordering::Relaxed)
    }

    pub fn set_cameras_matched(&self, matched: bool) {
        self.cameras_matched.store(matched, Ordering::Relaxed);
    }

    pub fn set_head_camera(&self, fitted: bool) {
        if self.head_absent.swap(!fitted, Ordering::Relaxed) == !fitted {
            return;
        }
        let mut s = self.state.lock().unwrap();
        if let Some(saved) = &s.saved_ports {
            s.roles = Roles::from_ports(saved, &s.cam_ports, fitted);
        }
    }

    pub fn head_absent(&self) -> bool {
        self.head_absent.load(Ordering::Relaxed)
    }

    pub fn set_keep_faulted_episodes(&self, keep: bool) {
        self.keep_faulted_episodes.store(keep, Ordering::Relaxed);
    }

    pub fn gate_level(&self) -> u8 {
        self.verdict.lock().unwrap().as_ref().map(|v| v.gate_level).unwrap_or(gate::HARD_STOP)
    }

    /// The gate tick's answer. Faults that opened or closed while recording
    /// reach the writer before the new verdict becomes visible.
    ///
    /// The send blocks while the writer's queue is full, so it happens under
    /// `verdict_order` rather than `verdict`: the panel and the writer itself
    /// (`standing_faults`) read `verdict` and must not wait on the writer.
    pub(crate) fn apply_verdict(&self, fresh: gate::Verdict) {
        let _order = self.verdict_order.lock().unwrap();
        if self.recording() {
            let (opened, closed) =
                gate::verdict_changes(self.verdict.lock().unwrap().as_ref(), &fresh);
            let at_ns = boottime_ns();
            for Reason { source, fault } in opened {
                self.control(Rec::Fault { at_ns, source, fault });
            }
            for Reason { source, fault } in closed {
                self.control(Rec::FaultOver { at_ns, source, fault });
            }
        }
        *self.verdict.lock().unwrap() = Some(fresh);
    }
}
