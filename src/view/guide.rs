//! Spoken steps for gripper calibration, so the operator can follow them with
//! the headset on the neck. Each tick names the line the panel shows; a line
//! is spoken when it changes.

use crate::misc::calib::{Problem, Prompt};

pub const CALIB_DONE: &str = "calib.done";
pub const CALIB_CANCELLED: &str = "calib.cancelled";
pub const CAMS_DONE: &str = "cams.done";

/// Every line this module can produce, for the clip check in `voice`.
#[cfg(test)]
pub fn all() -> Vec<&'static str> {
    let prompts = [None, Some(Prompt::Squeeze), Some(Prompt::Release)];
    let problems = [0, 1].into_iter().flat_map(|side| {
        [Problem::Silent(side), Problem::Barely(side, 0.0), Problem::Wrapped(side, 4.0)]
    });
    let mut all: Vec<_> = prompts.iter().filter_map(|&p| calibration(p, &[])).collect();
    all.extend(problems.filter_map(|p| calibration(None, &[p])));
    all.extend([CALIB_DONE, CALIB_CANCELLED, CAMS_DONE]);
    all
}

/// The calibration line: a problem first, because the panel shows it over
/// the prompt, then the prompt.
pub fn calibration(prompt: Option<Prompt>, problems: &[Problem]) -> Option<&'static str> {
    if let Some(problem) = problems.first() {
        return Some(match problem {
            Problem::Silent(0) => "left_gripper_sensor.silent",
            Problem::Silent(_) => "right_gripper_sensor.silent",
            Problem::Barely(0, _) => "calib.left_barely",
            Problem::Barely(..) => "calib.right_barely",
            Problem::Wrapped(0, _) => "calib.left_wrapped",
            Problem::Wrapped(..) => "calib.right_wrapped",
        });
    }
    prompt.map(|prompt| match prompt {
        Prompt::Squeeze => "calib.squeeze",
        Prompt::Release => "calib.release",
    })
}

/// The last line spoken, so a line held for many ticks is said once.
#[derive(Default)]
pub struct Said(Option<&'static str>);

impl Said {
    /// The line to speak now, if it differs from the last one.
    pub fn changed(&mut self, now: Option<&'static str>) -> Option<&'static str> {
        let changed = now != self.0;
        self.0 = now;
        now.filter(|_| changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_said_once_and_again_after_it_changes() {
        let mut said = Said::default();
        assert_eq!(said.changed(Some("calib.squeeze")), Some("calib.squeeze"));
        assert_eq!(said.changed(Some("calib.squeeze")), None);
        assert_eq!(
            said.changed(Some("left_gripper_sensor.silent")),
            Some("left_gripper_sensor.silent")
        );
        assert_eq!(
            said.changed(Some("calib.squeeze")),
            Some("calib.squeeze"),
            "back to the prompt"
        );
        assert_eq!(said.changed(None), None);
        assert_eq!(said.changed(Some("calib.squeeze")), Some("calib.squeeze"), "a new sweep");
    }

    #[test]
    fn a_problem_is_said_over_the_prompt() {
        let barely = [Problem::Barely(1, 0.1)];
        assert_eq!(calibration(Some(Prompt::Release), &[]), Some("calib.release"));
        assert_eq!(calibration(None, &barely), Some("calib.right_barely"));
        assert_eq!(
            calibration(Some(Prompt::Squeeze), &[Problem::Silent(0)]),
            Some("left_gripper_sensor.silent")
        );
        assert_eq!(calibration(None, &[]), None);
    }
}
