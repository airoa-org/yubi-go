//! Upstream's `BridgeStateMachine` (`airoa_core.task.bridge`). Which calls to
//! make, and every UUID and label in them, are the caller's.

use serde_json::Value;

use crate::Error;

/// The machine's snapshot after every call in `calls`, in order. Each call is
/// `{"method": ..., "args": [...]}` named as upstream names it, so the caller
/// keeps the list and appends to it; a call that fails is an `Err` and was
/// never applied.
pub fn apply(calls: &[Value]) -> Result<Value, Error> {
    crate::runtime::call("bridge", serde_json::json!({ "calls": calls }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const EPISODE: &str = "6f1c0e0a-0000-4000-8000-000000000001";
    /// A PA (primitive action) is upstream's word for one labelled stretch
    /// inside a recording.
    const PA: &str = "6f1c0e0a-0000-4000-8000-000000000002";

    #[test]
    fn a_pa_runs_inside_a_recording_and_only_its_own_uuid_stops_it() {
        crate::start(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build")).unwrap();
        let mut calls = vec![
            json!({"method": "begin_recording", "args": [EPISODE, "t1", "collect"]}),
            json!({"method": "recording_started", "args": []}),
            json!({"method": "start_pa", "args": [PA, "pick"]}),
        ];
        let snapshot = apply(&calls).unwrap();
        assert_eq!(snapshot["state"], 4, "PA_ACTIVE");
        assert_eq!(snapshot["active_pa"], json!({"pa_uuid": PA, "label": "pick"}));
        assert_eq!(snapshot["task"]["episode_uuid"], EPISODE);

        calls.push(json!({"method": "stop_pa", "args": [EPISODE, 1]}));
        assert!(apply(&calls).is_err(), "another PA's uuid cannot stop this one");
        calls.pop();
        calls.push(json!({"method": "stop_pa", "args": [PA, 1]}));
        assert_eq!(apply(&calls).unwrap()["state"], 3, "RECORDING_IDLE");
    }
}
