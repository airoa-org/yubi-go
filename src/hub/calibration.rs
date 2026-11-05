//! The gripper calibration: the sweep the operator runs from the button, and
//! the stored calibration that turns raw encoder readings into angles.

use std::sync::atomic::Ordering;

use log::{error, info};

use super::{Hub, SRC_ENCODER};
use crate::misc::calib;
use crate::misc::clock::boottime_ns;

impl Hub {
    fn encoders_live(&self) -> [bool; 2] {
        let stats = self.stats();
        SRC_ENCODER.map(|i| stats[i].live)
    }

    pub fn calibration_presses(&self) -> u32 {
        self.calibration_presses.load(Ordering::Relaxed)
    }

    pub fn toggle_calibration(&self) {
        self.calibration_presses.fetch_add(1, Ordering::Relaxed);
        let live = self.encoders_live();
        let mut s = self.calibration.lock().unwrap();
        // A sweep mid-take changes the calibration under a take that stored
        // the old one, and turns off the stop gesture while it runs.
        if self.busy() && s.prompt().is_none() {
            info!("calibration: refused, a take is recording");
            return;
        }
        s.toggle(live);
        info!(
            "calibration: {}",
            match (s.prompt(), s.message().is_empty()) {
                (Some(p), _) => p.text(),
                (None, true) => "sweep abandoned",
                (None, false) => "refused, a gripper is not sending",
            }
        );
    }

    /// Store a sweep that just finished.
    pub(crate) fn poll_calibration(&self) {
        let live = self.encoders_live();
        let finished = {
            let mut s = self.calibration.lock().unwrap();
            s.watch(live);
            let finished = s.take_finished();
            // Counted under the lock that clears the prompt, so whoever sees
            // the prompt gone also sees the count.
            if finished.is_some() {
                self.calibrations_finished.fetch_add(1, Ordering::Relaxed);
            }
            finished
        };
        let Some(calibration) = finished else { return };
        match calib::store(&self.dir(), &calibration) {
            Ok(()) => info!("calibration: stored — {}", describe(&calibration)),
            Err(e) => error!("calibration: could not store ({e}) — this session only"),
        }
    }

    /// Sweeps that finished since launch, stored or not.
    pub fn calibrations_finished(&self) -> u64 {
        self.calibrations_finished.load(Ordering::Relaxed)
    }

    pub fn calibration(&self) -> calib::Calibration {
        self.calibration.lock().unwrap().active().clone()
    }

    /// Read `calibration.json` again, after a reset removed it.
    pub fn reload_calibration(&self) {
        *self.calibration.lock().unwrap() = calib::Session::load(&self.dir());
    }

    pub fn gripper_angle(&self, side: usize, raw: Option<f32>) -> Option<f64> {
        self.calibration.lock().unwrap().angle(side, raw)
    }

    pub fn calibration_ui(&self) -> (Option<calib::Prompt>, [f32; 2], Vec<calib::Problem>) {
        let s = self.calibration.lock().unwrap();
        (s.prompt(), s.held(boottime_ns()), s.message().to_vec())
    }
}

pub(super) fn describe(c: &calib::Calibration) -> String {
    let side = |i: usize| match c.side(i) {
        Some(s) => format!("{} envelope {:.3} rad", s.device_id, s.envelope()),
        None => "uncalibrated".to_string(),
    };
    format!("left {}, right {}", side(0), side(1))
}
