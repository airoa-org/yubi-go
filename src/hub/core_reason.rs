//! Core's reasons for refusing a start, reworded as panel rows.

/// Core's reason for refusing a start, as panel rows: what is wrong, then
/// what to do about it.
pub(super) fn core_reason(reason: &str) -> Vec<String> {
    let text = reason.strip_prefix("Cannot start recording: ").unwrap_or(reason);
    let rows: &[&str] = match text {
        t if t.starts_with("no active operator") => {
            &["No operator logged in", "Open this robot's dashboard teleop screen"]
        }
        t if t.starts_with("cannot verify active operator") => {
            &["Cannot check the operator", "The backend is unreachable"]
        }
        t if t.starts_with("recording gate closed") => &["The gate is closed"],
        t if t.starts_with("episode has no subtasks") => &["The episode has no subtasks"],
        t if t.starts_with("invalid state") => &["Cannot start in this state"],
        "no episodes available" => &["Nothing assigned", "Assign an episode on the dashboard"],
        "all available episodes already completed" => &["Every assignment is done"],
        t if t.starts_with("episode '") && t.ends_with("not found") => {
            &["The requested episode was not found"]
        }
        "failed to fetch first available episode" => {
            &["Cannot fetch the assignment", "The backend is unreachable"]
        }
        "recorder: already recording" => &["Already recording"],
        "recorder: not started" => &["The recorder is not running"],
        t if t.starts_with("recorder: ") => {
            let detail = t["recorder: ".len()..].to_string();
            return vec!["Recorder error".to_string(), detail];
        }
        t => return vec![t.to_string()],
    };
    rows.iter().map(|row| row.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_reason_core_can_give_for_a_start_is_reworded() {
        let known = [
            "Cannot start recording: invalid state READY (expected CONFIRM_TASK)",
            "Cannot start recording: recording gate closed (level 1)",
            "Cannot start recording: cannot verify active operator (cannot reach or authenticate to backend)",
            "Cannot start recording: no active operator logged in on the robot",
            "Cannot start recording: episode has no subtasks",
            "Cannot start recording: no episodes available",
            "Cannot start recording: all available episodes already completed",
            "Cannot start recording: episode 'abc' not found",
            "Cannot start recording: failed to fetch first available episode",
            "Cannot start recording: recorder: already recording",
            "Cannot start recording: recorder: not started",
            "Cannot start recording: recorder: disk full",
        ];
        for reason in known {
            let en = core_reason(reason);
            assert!(!en[0].starts_with("Cannot start recording"), "{reason:?} is not reworded");
        }
        assert_eq!(core_reason("something new"), ["something new"], "unknown passes");
    }
}
