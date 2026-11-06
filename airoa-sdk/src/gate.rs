use serde::{Deserialize, Serialize};

use crate::Error;

pub const OK: u8 = 0;
pub const BLOCK_START: u8 = 1;
pub const HARD_STOP: u8 = 2;

pub fn diag_level_for(escalation: i32, passed: bool) -> Result<u8, Error> {
    let v = crate::runtime::call(
        "diag_level_for",
        serde_json::json!({"escalation": escalation, "passed": passed}),
    )?;
    v.as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .ok_or_else(|| Error::Python(format!("diag_level_for: expected 0..=2, got {v}")))
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub name: String,
    pub passed: bool,
    pub reason: String,
    pub escalation: u8,
    #[serde(default)]
    pub settle_exempt: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GroupState {
    pub name: String,
    pub level: u8,
    pub settled: bool,
    pub settle_is_initial: bool,
    pub settle_start_time: f64,
    pub settle_sec: f64,
    pub recovery_sec: f64,
}

/// A gate group before its first verdict: upstream's own two windows, named
/// rather than positional so a call site cannot swap them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Group<'a> {
    pub name: &'a str,
    /// How long every condition must hold before the group is believed.
    pub settle: std::time::Duration,
    /// How long a recovered condition must hold before the group reopens.
    pub recovery: std::time::Duration,
}

impl GroupState {
    /// Fail-closed: a fresh group starts at [`HARD_STOP`] and only opens once
    /// its conditions have held for `settle`.
    pub fn new(group: Group<'_>, now_sec: f64) -> GroupState {
        GroupState {
            name: group.name.to_string(),
            level: HARD_STOP,
            settled: false,
            settle_is_initial: true,
            settle_start_time: now_sec,
            settle_sec: group.settle.as_secs_f64(),
            recovery_sec: group.recovery.as_secs_f64(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GroupVerdict {
    pub name: String,
    pub level: u8,
    pub settled: bool,
    pub settle_is_initial: bool,
    pub settle_start_time: f64,
    pub failing: Vec<Condition>,
    pub results: Vec<Condition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Verdict {
    pub gate_level: u8,
    pub groups: Vec<GroupVerdict>,
}

pub fn orchestrate(
    states: &mut [GroupState],
    conditions: &[(String, Vec<Condition>)],
    prev_gate_level: u8,
    now_sec: f64,
) -> Result<Verdict, Error> {
    let groups: Vec<serde_json::Value> = states
        .iter()
        .map(|state| {
            let empty = Vec::new();
            let conditions = conditions
                .iter()
                .find(|(name, _)| *name == state.name)
                .map(|(_, c)| c)
                .unwrap_or(&empty);
            let mut body = serde_json::to_value(state).unwrap_or_default();
            body["conditions"] = serde_json::to_value(conditions).unwrap_or_default();
            body
        })
        .collect();
    let v = crate::runtime::call(
        "evaluate_gate",
        serde_json::json!({
            "groups": groups,
            "prev_gate_level": prev_gate_level,
            "now": now_sec,
        }),
    )?;
    let verdict: Verdict = serde_json::from_value(v)
        .map_err(|e| Error::Python(format!("evaluate_gate answer: {e}")))?;
    for state in states.iter_mut() {
        if let Some(had) = verdict.groups.iter().find(|g| g.name == state.name) {
            state.level = had.level;
            state.settled = had.settled;
            state.settle_is_initial = had.settle_is_initial;
            state.settle_start_time = had.settle_start_time;
        }
    }
    Ok(verdict)
}

/// `evaluate_condition` — upstream compiles the expression, rejecting anything
/// outside its safe node set, and evaluates it against `msg`.
///
/// `compile_condition` itself answers with a Python code object, which cannot
/// cross the boundary; this is the usable projection — exactly what upstream's
/// own callers do with the compiled code,
/// `eval(code, {"__builtins__": {}}, {"msg": msg})`, coerced to a bool. `msg`
/// is a JSON object: nested objects answer attribute access and arrays answer
/// subscripts, so `msg.effort[0] < 10.0` reads as it does on a ROS message.
/// An unsafe or unparsable expression, or a field the message does not carry,
/// arrives as [`Error::Refused`].
pub fn evaluate_condition(expr: &str, msg: &serde_json::Value) -> Result<bool, Error> {
    let v =
        crate::runtime::call("evaluate_condition", serde_json::json!({"expr": expr, "msg": msg}))?;
    v.as_bool()
        .ok_or_else(|| Error::Python(format!("evaluate_condition: expected a bool, got {v}")))
}

/// `msg_refs` — the leaf `msg.*` paths an expression reads, so a host can
/// subscribe to exactly what a condition needs. `msg.pose.position.x` is
/// returned; the `msg.pose` prefix it covers is not.
pub fn msg_refs(expr: &str) -> Result<Vec<String>, Error> {
    let v = crate::runtime::call("msg_refs", serde_json::json!({"expr": expr}))?;
    serde_json::from_value(v).map_err(|e| Error::Python(format!("msg_refs: {e}")))
}

/// `load_gate_config` — the default YAML, deep-merged with an optional
/// override. Both files are read *inside* the sandbox, so the caller must
/// [`crate::add_preopen`] their directory first and pass guest paths. An empty
/// `override_path`, or one naming a file that is not there, leaves the default
/// alone; a missing default is a fault.
pub fn load_config(default_path: &str, override_path: &str) -> Result<serde_json::Value, Error> {
    crate::runtime::call(
        "load_gate_config",
        serde_json::json!({"default_path": default_path, "override_path": override_path}),
    )
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyValue {
    pub key: String,
    pub value: String,
}

/// One diagnostic payload: upstream's own dict shape, per condition and per
/// group. `level` is a diagnostic level (0=OK, 1=WARN, 2=ERROR), which is
/// *not* the escalation scale.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub name: String,
    pub level: u8,
    pub message: String,
    pub hardware_id: String,
    pub values: Vec<KeyValue>,
}

/// `gate_diagnostics` — the per-condition and per-group diagnostics for a tick
/// [`orchestrate`] has just answered. The settle windows come from `states`
/// and the results from `verdict`, so pass the pair from one tick.
pub fn diagnostics(
    states: &[GroupState],
    verdict: &Verdict,
    now_sec: f64,
) -> Result<Vec<Diagnostic>, Error> {
    let groups: Vec<serde_json::Value> = states
        .iter()
        .map(|state| {
            let had = verdict.groups.iter().find(|g| g.name == state.name);
            let empty = Vec::new();
            let mut body = serde_json::to_value(state).unwrap_or_default();
            body["failing"] =
                serde_json::to_value(had.map(|g| &g.failing).unwrap_or(&empty)).unwrap_or_default();
            body["results"] =
                serde_json::to_value(had.map(|g| &g.results).unwrap_or(&empty)).unwrap_or_default();
            body
        })
        .collect();
    let v = crate::runtime::call(
        "gate_diagnostics",
        serde_json::json!({"groups": groups, "now": now_sec}),
    )?;
    serde_json::from_value(v).map_err(|e| Error::Python(format!("gate_diagnostics answer: {e}")))
}
