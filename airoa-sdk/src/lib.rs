//! Host boundary to `airoa_core`, a preinitialized CPython/WASIp2 Component.
//! WIT defines calls and errors; HTTP uses wasi:http. Idle instances are reused,
//! and nested callbacks use separate instances without replaying operations.
//! Task and gate state retain the upstream JSON schema. All decisions stay upstream.
//!
//! One op per upstream call, one typed wrapper per op: gate evaluation,
//! condition expressions, diagnostics in ([`gate`]) and back out ([`status`]),
//! task sessions and startup recovery ([`task`]), the bridge session state
//! machine ([`bridge`]), backend calls and the offline queue that outlives an
//! outage ([`backend`]),
//! storage paths and GC policy (here), the record of what has been uploaded
//! ([`storage`]), and the version manifest ([`system_info`]). A wrapper
//! forwards and reports; it never re-implements a rule. Ops that read a file
//! read it inside the sandbox, so [`add_preopen`] the directory first and pass
//! guest paths.

pub mod backend;
pub mod bridge;
pub mod gate;
pub mod runtime;
pub mod status;
pub mod storage;
pub mod system_info;
pub mod task;

#[cfg(test)]
mod test_http;

use std::fmt;

pub use runtime::{
    add_preopen, engine_config, payload, ready, start, start_with, Access, PRECOMPILE,
};
use runtime::{call, call_str};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NotStarted,
    Start(String),
    Python(String),
    Refused(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::NotStarted => write!(f, "airoa_core is not started"),
            Error::Start(e) => write!(f, "airoa_core failed to start: {e}"),
            Error::Python(e) => write!(f, "airoa_core raised: {e}"),
            Error::Refused(e) => write!(f, "airoa_core refused: {e}"),
        }
    }
}

pub fn canonical_prefix(meta_json: &str) -> Result<String, Error> {
    call_str("canonical_prefix", serde_json::json!({"meta_json": meta_json}))
}

pub fn normalize(value: &str) -> Result<String, Error> {
    call_str("normalize", serde_json::json!({"value": value}))
}

pub fn resolve_s3_prefix(prefix: &str, dir_path: &str, path_rule: &str) -> Result<String, Error> {
    call_str(
        "resolve_s3_prefix",
        serde_json::json!({"prefix": prefix, "dir_path": dir_path, "path_rule": path_rule}),
    )
}

/// `storage_gc` — one local GC cycle over `root`, a guest path.
///
/// Upstream chooses what to delete. Finding the recordings and deleting them
/// is the adapter's work, because upstream's own cycle runs only on a MinIO
/// bucket.
///
/// With a [`storage::UploadState`], "already stored" is what that database
/// says about each recording, and the `storage_marker` filename in `gc` is
/// ignored — which is what lets a host stop writing receipt files. Without
/// one the filename scan stands, so a caller that keeps no state is not left
/// with a GC that collects nothing.
pub fn storage_gc(
    root: &str,
    gc: &serde_json::Value,
    state: Option<&storage::UploadState>,
) -> Result<serde_json::Value, Error> {
    let mut args = serde_json::json!({"root": root, "gc": gc});
    if let (Some(args), Some(state)) = (args.as_object_mut(), state) {
        args.insert("target".into(), state.target().into());
        args.insert("upload_state_path".into(), state.path().into());
    }
    call("storage_gc", args)
}

#[derive(Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct StorageTarget {
    pub name: String,
    pub endpoint: String,
    pub access_key: String,
    pub secret_key: String,
    pub use_ssl: bool,
    pub bucket: String,
    pub prefix: String,
    pub path_rule: String,
    pub region: String,
    /// Upstream's GC block, passed back verbatim into `storage_gc` — its
    /// semantics are upstream's, so it stays untyped on purpose.
    pub gc: Option<serde_json::Value>,
}

impl fmt::Debug for StorageTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StorageTarget")
            .field("name", &self.name)
            .field("endpoint", &self.endpoint)
            .field("access_key", &redacted(&self.access_key))
            .field("secret_key", &redacted(&self.secret_key))
            .field("use_ssl", &self.use_ssl)
            .field("bucket", &self.bucket)
            .field("prefix", &self.prefix)
            .field("path_rule", &self.path_rule)
            .field("region", &self.region)
            .field("gc", &self.gc)
            .finish()
    }
}

/// What a `Debug` shows of a credential: whether it is set. A key printed in
/// a log or a test failure is in every copy of that log.
pub(crate) fn redacted(secret: &str) -> &'static str {
    if secret.is_empty() {
        ""
    } else {
        "<redacted>"
    }
}

#[derive(Debug, Default, Clone, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct StorageConfig {
    pub targets: Vec<StorageTarget>,
    pub delete_after_upload: bool,
    /// How long a take stays on local disk after it has been uploaded, when
    /// `delete_after_upload` is off. Upstream's default is 24. Zero disables
    /// the sweep, which on a device with no `gc:` block means nothing is ever
    /// reclaimed.
    pub local_retention_hours: u32,
    /// How long a settled row stays in the upload state database. Upstream's
    /// default is 720 — thirty days.
    pub state_purge_age_hours: f64,
}

/// Upstream's parsed GC strategy. The predicate it carries is a Python
/// closure that cannot cross the boundary, so it arrives as its description
/// and its whole truth table — three booleans have only eight answers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct GcStrategy {
    /// Upstream's own rendering of the rule, e.g. `marker AND (age OR space)`.
    pub description: String,
    table: [bool; 8],
}

impl GcStrategy {
    /// Whether a recording with these three conditions met is to be deleted.
    pub fn evaluate(&self, marker: bool, age: bool, space: bool) -> bool {
        self.table[usize::from(marker) * 4 + usize::from(age) * 2 + usize::from(space)]
    }
}

/// `parse_gc_strategy` — read a target's `gc.strategy` value. A top-level list
/// is AND and an `any_of` entry is OR; an unknown condition name or an empty
/// list is [`Error::Refused`].
pub fn parse_gc_strategy(strategy: &serde_json::Value) -> Result<GcStrategy, Error> {
    let v = call("parse_gc_strategy", serde_json::json!({"strategy": strategy}))?;
    serde_json::from_value(v).map_err(|e| Error::Python(format!("parse_gc_strategy: {e}")))
}

/// What one GC pass did, as upstream's `GCCycleResult` — the fields
/// [`gc_diagnostic_level`] judges. `error` set means the pass failed.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct GcCycle {
    pub total_recordings: u64,
    pub total_bytes: u64,
    pub eligible_count: u64,
    pub error: Option<String>,
}

/// `compute_diagnostic_level` — how a GC pass reads as a diagnostic level
/// (0=OK, 1=WARN, 2=ERROR), given the same `gc` block [`storage_gc`] takes.
/// A block with no `strategy`, or none at all, is [`Error::Refused`].
pub fn gc_diagnostic_level(gc: &serde_json::Value, result: &GcCycle) -> Result<u8, Error> {
    let v = call("gc_diagnostic_level", serde_json::json!({"gc": gc, "result": result}))?;
    v.as_u64()
        .and_then(|n| u8::try_from(n).ok())
        .ok_or_else(|| Error::Python(format!("gc_diagnostic_level: expected 0..=2, got {v}")))
}

pub fn load_storage_config(path: &str) -> Result<StorageConfig, Error> {
    let v = call("load_storage_config", serde_json::json!({"path": path}))?;
    serde_json::from_value(v)
        .map_err(|e| Error::Python(format!("load_storage_config: unreadable answer: {e}")))
}

pub fn enrich_episodes(
    episodes: &[serde_json::Value],
    operator_name: &str,
) -> Result<Vec<serde_json::Value>, Error> {
    let v = call(
        "enrich_episodes",
        serde_json::json!({"episodes": episodes, "operator_name": operator_name}),
    )?;
    match v {
        serde_json::Value::Array(rows) => Ok(rows),
        other => Err(Error::Python(format!("enrich_episodes: expected a list, got {other}"))),
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct StatusInputs {
    pub robot_type: String,
    pub battery_pct: i64,
    pub battery_charging: bool,
    pub disk_free: u64,
    pub disk_used: u64,
    pub is_recording: bool,
    pub gate_level: u8,
    pub gate_conditions: serde_json::Value,
    pub uptime_sec: f64,
    pub reported_at: Option<String>,
    pub connection_quality_pct: u8,
}

pub fn status_payload(inputs: &StatusInputs) -> Result<String, Error> {
    Ok(call("status_payload", serde_json::json!({"inputs": inputs}))?.to_string())
}

pub fn build_metadata(args: serde_json::Value) -> Result<String, Error> {
    call_str("build_metadata", args)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::time::Duration;

    use super::*;

    fn up() {
        let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("build");
        start(&payload).unwrap_or_else(|e| panic!("{e}\n"));
    }

    #[test]
    fn starts_and_answers_a_version_probe() {
        up();
        assert!(ready());
        let v = call("version", serde_json::json!({})).unwrap();
        assert_eq!(v["platform"], "wasi", "{v}");
        let pinned =
            std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("build/PAYLOAD"))
                .unwrap();
        assert_eq!(
            v["payload"],
            pinned.as_str(),
            "the component reports the pins it was built from"
        );
        assert!(payload().contains_key("airoa_core_commit"), "{:?}", payload());
    }

    #[test]
    fn storage_gc_deletes_through_the_writable_mount() {
        up();
        let base = std::env::temp_dir().join(format!("airoa_sdk_gc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mk = |name: &str, markers: &[&str]| {
            let dir = base.join(name);
            std::fs::create_dir_all(dir.join("videos")).unwrap();
            std::fs::write(dir.join("videos/head.mp4"), [0u8; 8]).unwrap();
            for marker in markers {
                std::fs::write(dir.join(marker), "").unwrap();
            }
        };
        mk("episode_1", &[".done", ".stored"]);
        mk("episode_2", &[".done"]);
        add_preopen(&base, "/gc_probe", Access::ReadWrite);

        let gc = serde_json::json!({
            "strategy": ["marker"],
            "completion_marker": ".done",
            "storage_marker": ".stored",
            "orphan_age_hours": 0,
        });
        let v = storage_gc("/gc_probe", &gc, None).unwrap();
        assert_eq!(v["total_recordings"], 2, "{v}");
        assert_eq!(v["deleted"], serde_json::json!(["episode_1"]), "{v}");
        assert!(!base.join("episode_1").exists(), "the stored episode is reclaimed");
        assert!(base.join("episode_2").exists(), "the unstored episode is kept");
    }

    #[test]
    fn a_string_answer_round_trips() {
        up();
        assert!(normalize("東京ラボ").is_ok());
        assert!(canonical_prefix(r#"{"uuid":"x"}"#).is_ok());
    }

    #[test]
    fn a_refusal_arrives_as_refused() {
        up();
        assert!(matches!(canonical_prefix("{}"), Err(Error::Refused(_))));
    }

    #[test]
    fn a_fault_arrives_as_python() {
        up();
        assert!(matches!(call("no_such_op", serde_json::json!({})), Err(Error::Python(_))));
        assert!(matches!(call("normalize", serde_json::json!({})), Err(Error::Python(_))));
    }

    #[test]
    fn mirrored_escalation_levels_match_upstream() {
        up();
        let v = call("escalation_levels", serde_json::json!({})).unwrap();
        assert_eq!(v["OK"], gate::OK);
        assert_eq!(v["BLOCK_START"], gate::BLOCK_START);
        assert_eq!(v["HARD_STOP"], gate::HARD_STOP);
        assert!(gate::diag_level_for(2, false).is_ok());
    }

    #[test]
    fn status_payload_round_trips_as_json() {
        up();
        let inputs = StatusInputs {
            robot_type: "yubi-go".into(),
            battery_pct: 87,
            battery_charging: false,
            disk_free: 1,
            disk_used: 1,
            is_recording: true,
            gate_level: 0,
            gate_conditions: serde_json::json!({"gate_level": 0, "groups": {}}),
            uptime_sec: 1.5,
            reported_at: None,
            connection_quality_pct: 100,
        };
        let v: serde_json::Value = serde_json::from_str(&status_payload(&inputs).unwrap()).unwrap();
        assert!(v.is_object());
        let pct = &v["status"]["battery"]["pct"];
        assert!(pct.is_i64(), "the backend's schema says integer, and 87.0 is refused: {pct}");
    }

    #[test]
    fn episodes_come_back_enriched() {
        up();
        let raw = serde_json::json!({
            "id": "e1", "task_id": "t1", "task_version_id": "v1",
            "robot_id": "r1", "user_id": "u1", "task_name": "stack_cups",
            "subtasks": [
                {"id": "s2", "subtask_id": "d2", "name": "place", "order_index": 2},
                {"id": "s1", "subtask_id": "d1", "name": "pick", "order_index": 1},
            ],
        });
        let rows = enrich_episodes(&[raw], "Tarou").unwrap();
        assert_eq!(rows[0]["episodeId"], "e1");
        assert_eq!(rows[0]["taskVersionId"], "v1");
        assert_eq!(rows[0]["task"]["name"], "stack_cups");
        assert_eq!(rows[0]["assignedUserName"], "Tarou");
        let names: Vec<&str> = rows[0]["subtasks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["pick", "place"], "sorted by order_index upstream");

        let err = enrich_episodes(&[serde_json::json!({"id": "e1"})], "");
        assert!(matches!(err, Err(Error::Python(_))), "a row with no task_id raises: {err:?}");
    }

    #[test]
    fn the_gate_orchestrates_through_airoa_core() {
        up();
        let mut states = vec![gate::GroupState::new(
            gate::Group {
                name: "health",
                settle: Duration::from_millis(500),
                recovery: Duration::from_millis(200),
            },
            100.0,
        )];
        let pass = |name: &str| gate::Condition {
            name: name.to_string(),
            passed: true,
            reason: "ok".to_string(),
            escalation: gate::HARD_STOP,
            settle_exempt: false,
        };
        let conditions = vec![("health".to_string(), vec![pass("cam")])];

        let v = gate::orchestrate(&mut states, &conditions, gate::HARD_STOP, 100.0).unwrap();
        assert_eq!(v.gate_level, gate::BLOCK_START, "settling still blocks starts");
        assert!(!states[0].settled);

        let v = gate::orchestrate(&mut states, &conditions, v.gate_level, 100.6).unwrap();
        assert_eq!(v.gate_level, gate::OK, "settled after the window");
        assert!(states[0].settled);

        let mut failing = conditions.clone();
        failing[0].1[0].passed = false;
        failing[0].1[0].reason = "no data".to_string();
        let v = gate::orchestrate(&mut states, &failing, v.gate_level, 101.0).unwrap();
        assert_eq!(v.gate_level, gate::HARD_STOP);
        assert_eq!(v.groups[0].failing.len(), 1);

        let v = gate::orchestrate(&mut states, &conditions, v.gate_level, 101.1).unwrap();
        assert_eq!(v.gate_level, gate::BLOCK_START, "recovery hold before re-opening");
        let v = gate::orchestrate(&mut states, &conditions, v.gate_level, 101.4).unwrap();
        assert_eq!(v.gate_level, gate::OK, "recovered after recovery_sec");
    }

    #[test]
    fn a_condition_expression_is_compiled_and_judged_upstream() {
        up();
        let msg = serde_json::json!({
            "data": 3.0,
            "effort": [2.0, 40.0],
            "pose": {"position": {"x": 1.5}},
        });
        assert!(gate::evaluate_condition("msg.data < 5.0", &msg).unwrap());
        assert!(!gate::evaluate_condition("msg.effort[1] < 10.0", &msg).unwrap());
        assert!(
            gate::evaluate_condition("msg.pose.position.x > 1.0 and msg.data > 0", &msg).unwrap()
        );
        // Upstream's safety rules, not ours: no builtins, no other names, no
        // private attributes.
        for unsafe_expr in ["len(msg.data)", "__import__('os')", "msg.__class__", "x + 1"] {
            assert!(
                matches!(gate::evaluate_condition(unsafe_expr, &msg), Err(Error::Refused(_))),
                "{unsafe_expr} must be refused"
            );
        }
        assert!(matches!(gate::evaluate_condition("msg.absent < 1", &msg), Err(Error::Refused(_))));
    }

    #[test]
    fn only_the_leaf_msg_references_come_back() {
        up();
        let refs =
            gate::msg_refs("msg.pose.position.x > 1.0 and msg.data < msg.effort[0]").unwrap();
        assert_eq!(refs, ["msg.pose.position.x", "msg.data", "msg.effort[0]"]);
    }

    #[test]
    fn the_gate_config_override_merges_into_the_default() {
        up();
        let base =
            std::env::temp_dir().join(format!("airoa_sdk_gate_config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(
            base.join("gate.yaml"),
            "groups:\n  health:\n    settle_sec: 30\n    recovery_sec: 5\n",
        )
        .unwrap();
        std::fs::write(base.join("site.yaml"), "groups:\n  health:\n    settle_sec: 2\n").unwrap();
        add_preopen(&base, "/gate_config", Access::ReadOnly);

        let merged = gate::load_config("/gate_config/gate.yaml", "/gate_config/site.yaml").unwrap();
        assert_eq!(merged["groups"]["health"]["settle_sec"], 2, "the override wins");
        assert_eq!(merged["groups"]["health"]["recovery_sec"], 5, "and leaves the rest");

        let alone = gate::load_config("/gate_config/gate.yaml", "").unwrap();
        assert_eq!(alone["groups"]["health"]["settle_sec"], 30);
        let absent = gate::load_config("/gate_config/gate.yaml", "/gate_config/nope.yaml").unwrap();
        assert_eq!(absent, alone, "an override that is not there changes nothing");
    }

    #[test]
    fn a_settling_group_publishes_its_remaining_seconds() {
        up();
        let mut states = vec![gate::GroupState::new(
            gate::Group {
                name: "health",
                settle: Duration::from_secs(10),
                recovery: Duration::from_secs(5),
            },
            100.0,
        )];
        let conditions = vec![(
            "health".to_string(),
            vec![gate::Condition {
                name: "cam".to_string(),
                passed: true,
                reason: "ok".to_string(),
                escalation: gate::HARD_STOP,
                settle_exempt: false,
            }],
        )];
        let verdict = gate::orchestrate(&mut states, &conditions, gate::HARD_STOP, 104.0).unwrap();
        let rows = gate::diagnostics(&states, &verdict, 104.0).unwrap();

        let condition = &rows[0];
        assert_eq!(condition.name, "recording_gate/health/cam");
        assert_eq!(condition.level, 0, "a passing condition is OK whatever its escalation");
        assert_eq!(condition.hardware_id, "recording_gate");
        let group = &rows[1];
        assert_eq!(group.name, "recording_gate/health");
        assert_eq!(group.message, "settling (6.0s remaining)", "10s window, 4s in");
        // Settling blocks starts but stops nothing, so it publishes as WARN.
        assert_eq!(states[0].level, gate::BLOCK_START);
        assert_eq!(group.level, 1);
    }

    #[test]
    fn a_gc_strategy_answers_for_every_combination() {
        up();
        let and = parse_gc_strategy(&serde_json::json!(["marker", "age"])).unwrap();
        assert!(and.evaluate(true, true, false));
        assert!(!and.evaluate(true, false, true));

        let mixed = parse_gc_strategy(&serde_json::json!(["marker", {"any_of": ["age", "space"]}]))
            .unwrap();
        assert!(mixed.evaluate(true, false, true), "marker AND (age OR space)");
        assert!(!mixed.evaluate(false, true, true), "no marker, no deletion");
        assert!(!mixed.evaluate(true, false, false));

        // A bare string is the one-condition list spelled short.
        let bare = parse_gc_strategy(&serde_json::json!("marker")).unwrap();
        assert!(bare.evaluate(true, false, false));
        assert!(!bare.evaluate(false, true, true));
        assert!(matches!(parse_gc_strategy(&serde_json::json!([])), Err(Error::Refused(_))));
        assert!(matches!(parse_gc_strategy(&serde_json::json!(["nope"])), Err(Error::Refused(_))));
    }

    #[test]
    fn a_gc_pass_reads_as_a_diagnostic_level() {
        up();
        // 1 GB cap: WARN past 0.8 GB, and ERROR past the cap with nothing to reclaim.
        let gc = serde_json::json!({"strategy": ["marker"], "max_storage_gb": 1});
        let at = |bytes: u64, eligible: u64| GcCycle {
            total_bytes: bytes,
            eligible_count: eligible,
            ..GcCycle::default()
        };
        assert_eq!(gc_diagnostic_level(&gc, &at(500_000_000, 0)).unwrap(), 0);
        assert_eq!(gc_diagnostic_level(&gc, &at(900_000_000, 0)).unwrap(), 1);
        assert_eq!(gc_diagnostic_level(&gc, &at(2_000_000_000, 1)).unwrap(), 1, "reclaimable");
        assert_eq!(gc_diagnostic_level(&gc, &at(2_000_000_000, 0)).unwrap(), 2, "nothing to free");
        let failed = GcCycle { error: Some("mount gone".into()), ..GcCycle::default() };
        assert_eq!(gc_diagnostic_level(&gc, &failed).unwrap(), 2);
        // No cap configured is never anything but OK.
        let uncapped = serde_json::json!({"strategy": ["marker"]});
        assert_eq!(gc_diagnostic_level(&uncapped, &at(9_000_000_000, 0)).unwrap(), 0);
    }

    #[test]
    fn the_default_profile_is_the_one_upstream_falls_back_to() {
        up();
        let profile = backend::default_profile().unwrap();
        assert_eq!(profile, backend::Profile { id: "unknown".into(), ..Default::default() });
        // Every key upstream declares is present and only `id` is filled —
        // the shape a caller short-circuiting the backend stands in with.
        // `fetch_ok` is upstream saying the answer never came from a backend.
        let raw = call("default_robot_profile", serde_json::json!({})).unwrap();
        let raw = raw.as_object().unwrap();
        assert_eq!(raw["id"], "unknown");
        assert_eq!(raw["fetch_ok"], false);
        for key in
            ["name", "type", "organization_name", "location_name", "site_name", "active_operator"]
        {
            assert!(raw[key].is_null(), "{key} is not null: {raw:?}");
        }
    }

    #[test]
    fn the_operator_name_comes_out_of_a_profile_body() {
        up();
        let body = serde_json::json!({
            "id": "r1",
            "active_operator": {"display_name": "Tarou", "email": "t@example.com"},
        });
        assert_eq!(backend::operator_name(&body).unwrap(), "Tarou");
        // Nobody logged in, and an operator with no name, both read as empty —
        // so a caller can interpolate without a None check.
        assert_eq!(backend::operator_name(&serde_json::json!({"id": "r1"})).unwrap(), "");
        let nameless = serde_json::json!({"active_operator": {"email": "t@example.com"}});
        assert_eq!(backend::operator_name(&nameless).unwrap(), "");
    }
}
