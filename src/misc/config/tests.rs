use super::*;

fn parse(json: &str) -> Config {
    Config::from_raw(serde_json::from_str::<Raw>(json).unwrap())
}

fn loaded_with(name: &str, config: &str, profile: Option<&Profile>) -> Config {
    let dir = std::env::temp_dir().join(format!("yubigo_cfg_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.to_str().unwrap();
    std::fs::write(format!("{dir}/config.json"), config).unwrap();
    if let Some(profile) = profile {
        profile.save(dir).unwrap();
    }
    Config::load(dir)
}

fn profile() -> Profile {
    Profile {
        organization: "Sample Organization".into(),
        site: "Sample Organization (Default Site)".into(),
        location: "Sample Lab".into(),
        robot_type: "sample-robot".into(),
        robot_id: "00000000-0000-4000-8000-000000000001".into(),
        robot_name: "sample-robot-01".into(),
        operator: String::new(),
    }
}

const PRODUCTION: &str = r#"{
    "identity": {"organization": "typed-in", "robot_id": "yubi-go", "operator": "Tarou"},
    "backend": {"base_url": "http://backend.example.com:8000/api", "api_key": "k"}
}"#;

#[test]
fn a_production_identity_comes_from_the_backend() {
    let cfg = loaded_with("overlay", PRODUCTION, Some(&profile()));
    assert_eq!(cfg.identity.organization, "Sample Organization");
    assert_eq!(cfg.identity.robot_id, "00000000-0000-4000-8000-000000000001");
    assert_eq!(cfg.identity.robot_type, "sample-robot");
    assert_eq!(cfg.identity.operator, "Tarou");
}

#[test]
fn production_without_a_cached_profile_keeps_the_configured_identity() {
    let cfg = loaded_with("no_profile", PRODUCTION, None);
    assert_eq!(cfg.identity.organization, "typed-in");
    assert_eq!(cfg.identity.robot_id, "yubi-go");
}

fn queued(dir_name: &str, tasks: &[&str]) -> String {
    let dir = std::env::temp_dir().join(format!("yubigo_cfg_{dir_name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.to_str().unwrap().to_string();
    std::fs::write(format!("{dir}/config.json"), PRODUCTION).unwrap();
    let assigned: Vec<Assignment> = tasks
        .iter()
        .enumerate()
        .map(|(i, task)| Assignment {
            id: format!("ep-{i}"),
            task: (*task).to_string(),
            task_id: format!("task-{i}"),
            task_version_id: format!("ver-{i}"),
            subtasks: Vec::new(),
            spent: false,
        })
        .collect();
    Assignment::save_all(&dir, &assigned).unwrap();
    dir
}

#[test]
fn production_binds_the_first_unspent_episode() {
    let cfg = Config::load(&queued("assigned", &["ngc_test2", "fold the towel"]));
    assert_eq!(cfg.task(), "ngc_test2", "the head of the queue");
    let assignment = cfg.assignment.expect("an assigned episode");
    assert_eq!(assignment.id, "ep-0");
    assert_eq!(assignment.task_version_id, "ver-0");
}

#[test]
fn a_spent_row_still_names_the_task_but_binds_nothing() {
    let dir = queued("spent", &["ngc_test2"]);
    Assignment::consume(&dir, "ep-0").unwrap();

    let cfg = Config::load(&dir);
    assert_eq!(cfg.task(), "ngc_test2", "the operator is still collecting it");
    assert_eq!(cfg.assignment, None, "an unbound take, not a second claim on ep-0");
    assert!(cfg.task_completed());

    let mut rows = Assignment::load_all(&dir).unwrap();
    rows.push(Assignment {
        id: "ep-1".into(),
        task: "ngc_test2".into(),
        task_id: "task-0".into(),
        task_version_id: "ver-0".into(),
        subtasks: Vec::new(),
        spent: false,
    });
    Assignment::save_all(&dir, &rows).unwrap();
    let cfg = Config::load(&dir);
    assert!(!cfg.task_completed());
    assert_eq!(cfg.assignment.unwrap().id, "ep-1");
}

fn picking(dir: &str, picks: bool, selected: &str) -> Config {
    let mut doc: serde_json::Value = serde_json::from_str(PRODUCTION).unwrap();
    doc["operator_picks_task"] = picks.into();
    doc["selected_task"] = selected.into();
    std::fs::write(format!("{dir}/config.json"), doc.to_string()).unwrap();
    Config::load(dir)
}

#[test]
fn an_operator_who_picks_gets_the_oldest_episode_of_that_task() {
    let dir = queued("picked", &["wipe", "fold", "wipe", "fold"]);
    let cfg = picking(&dir, true, "fold");
    assert_eq!(cfg.task(), "fold");
    assert_eq!(cfg.assignment.unwrap().id, "ep-1");
    assert_eq!(cfg.queue, vec![("wipe".to_string(), 2), ("fold".to_string(), 2)]);

    Assignment::consume(&dir, "ep-1").unwrap();
    let cfg = picking(&dir, true, "fold");
    assert_eq!(cfg.assignment.unwrap().id, "ep-3", "the pick holds after a take");
    assert_eq!(cfg.queue, vec![("wipe".to_string(), 2), ("fold".to_string(), 1)]);
}

/// Recording the head of the queue instead would put a take under a task
/// the operator did not pick.
#[test]
fn a_pick_with_nothing_left_has_no_task_until_its_copy_arrives() {
    let dir = queued("picked_out", &["wipe", "fold"]);
    Assignment::consume(&dir, "ep-1").unwrap();
    let cfg = picking(&dir, true, "fold");
    assert!(!cfg.has_task(), "the start is refused");
    assert_eq!(cfg.assignment, None);
    assert_eq!(cfg.queue, vec![("wipe".to_string(), 1)]);
}

#[test]
fn a_pick_is_ignored_unless_the_operator_may_pick() {
    let cfg = picking(&queued("not_picking", &["wipe", "fold"]), false, "fold");
    assert_eq!(cfg.assignment.unwrap().id, "ep-0");
}

/// Production picks from the queue, which `config.json` does not hold, so
/// neither the declared list nor a re-declaration of it may refuse or drop it.
#[test]
fn a_production_pick_is_not_checked_against_the_declared_tasks() {
    let doc =
        patch(PRODUCTION, r#"{"operator_picks_task": true, "selected_task": "fold"}"#).unwrap();
    assert_eq!(doc["selected_task"], "fold");
    let doc = patch(&doc.to_string(), r#"{"tasks": ["wipe"]}"#).unwrap();
    assert_eq!(doc["selected_task"], "fold");
}

#[test]
fn production_with_an_empty_queue_has_no_task_at_all() {
    let cfg = Config::load(&queued("empty_queue", &[]));
    assert!(!cfg.has_task());
    assert_eq!(cfg.task(), "untitled");
    assert_eq!(cfg.assignment, None);
    assert!(!cfg.task_completed(), "nothing was ever served");
}

#[test]
fn a_collected_task_stays_collectable() {
    let dir = queued("collected", &["ngc_test2"]);
    Assignment::save_all(&dir, &[]).unwrap();
    let cfg = Config::load(&dir);
    assert_eq!(cfg.task(), "ngc_test2", "the task outlives its episode");
    assert_eq!(cfg.task(), "ngc_test2");
    assert_eq!(cfg.assignment, None, "nothing queued to bind to");
    assert!(cfg.task_completed());
}

#[test]
fn production_that_has_never_synced_has_no_task() {
    let cfg = loaded_with("never_synced", PRODUCTION, Some(&profile()));
    assert!(!cfg.has_task());
    assert_eq!(cfg.task(), "untitled");
    assert_eq!(cfg.assignment, None, "nothing to bind an episode to yet");
}

/// Ports are read by slot name, and a port list replaces the older rank
/// list when it is written.
#[test]
fn camera_ports_are_read_by_slot_and_supersede_camera_roles() {
    let load = |body: &str| Config::from_raw(serde_json::from_str::<Raw>(body).unwrap());
    let cfg = load(r#"{"camera_ports": {"head": "1-1.4", "left_wrist": "1-1.2"}}"#);
    assert_eq!(cfg.camera_ports, Some([Some("1-1.2".into()), None, Some("1-1.4".into())]));
    assert_eq!(load("{}").camera_ports, None);

    let doc =
        patch(r#"{"camera_roles": [1, 0, 2]}"#, r#"{"camera_ports": {"left_wrist": "1-1.2"}}"#)
            .unwrap();
    assert!(doc.get("camera_roles").is_none(), "{doc}");
    assert_eq!(doc["camera_ports"]["left_wrist"], "1-1.2");
}

/// A fresh install has nothing to talk to and records anyway: standalone,
/// "untitled", never refused for want of a task, and never told a queue it
/// never had is finished.
#[test]
fn a_fresh_install_records_standalone() {
    let cfg = Config::default();
    assert!(cfg.standalone && !cfg.production());
    assert!(cfg.has_task(), "no queue is not an empty queue");
    assert_eq!(cfg.task(), "untitled");
    assert!(!cfg.task_completed());
}

#[test]
fn standalone_takes_the_picked_task_or_the_first_on_its_list() {
    let cfg = parse(r#"{"tasks": ["wipe", "fold"], "selected_task": "fold"}"#);
    assert_eq!(cfg.task(), "fold");
    let cfg = parse(r#"{"tasks": ["wipe", "fold"]}"#);
    assert_eq!(cfg.task(), "wipe");
    let names: Vec<&str> = cfg.tasks.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["wipe", "fold"]);
    assert!(cfg.subtask_names().is_empty(), "a bare name is one step");
}

#[test]
fn a_standalone_take_walks_the_subtasks_its_task_declares() {
    let cfg = parse(
        r#"{"tasks": ["wipe", {"name": "fold", "subtasks": ["spread", "fold in half"]}],
            "selected_task": "fold"}"#,
    );
    assert_eq!(cfg.subtask_names(), ["spread", "fold in half"]);
    let cfg = parse(r#"{"tasks": ["wipe", {"name": "fold", "subtasks": ["spread"]}]}"#);
    assert!(cfg.subtask_names().is_empty(), "wipe is picked, and it has none");
}

#[test]
fn only_a_task_that_asks_advances_when_still() {
    let tasks = r#""tasks": ["wipe", {"name": "fold", "subtasks": ["spread"], "advance_when_still": true}]"#;
    let cfg = parse(&format!(r#"{{{tasks}, "selected_task": "fold"}}"#));
    assert!(cfg.advances_when_still());
    let cfg = parse(&format!(r#"{{{tasks}, "selected_task": "wipe"}}"#));
    assert!(!cfg.advances_when_still(), "off unless the task says so");
}

#[test]
fn the_page_saves_advance_when_still_only_when_on() {
    let doc = patch(
        "{}",
        r#"{"tasks": [{"name": "wipe", "subtasks": [], "advance_when_still": false},
                      {"name": "fold", "subtasks": [], "advance_when_still": true}]}"#,
    )
    .unwrap();
    assert_eq!(
        doc["tasks"],
        serde_json::json!(["wipe", {"name": "fold", "subtasks": [], "advance_when_still": true}]),
    );
    let err = patch("{}", r#"{"tasks": [{"name": "fold", "advance_when_still": 1}]}"#).unwrap_err();
    assert!(err.contains("true or false"), "{err}");
}

/// A name saved with translations by a pre-release build reads as its English.
#[test]
fn a_translated_name_reads_as_its_english() {
    let cfg = parse(
        r#"{"selected_task": "fold",
            "tasks": [{"name": {"en": "fold", "ja": "畳む"},
                       "subtasks": [{"en": "spread", "ja": "広げる"}, "fold in half"]}]}"#,
    );
    assert_eq!(cfg.task(), "fold");
    assert_eq!(cfg.subtask_names(), ["spread", "fold in half"]);
}

#[test]
fn the_page_declares_subtasks_under_a_task() {
    let doc = patch(
        "{}",
        r#"{"tasks": [{"name": "wipe", "subtasks": []},
                      {"name": "fold", "subtasks": ["spread", " ", "fold in half"]}],
            "selected_task": "fold"}"#,
    )
    .unwrap();
    assert_eq!(
        doc["tasks"],
        serde_json::json!(["wipe", {"name": "fold", "subtasks": ["spread", "fold in half"]}]),
        "a task without steps stays a bare string",
    );
    assert_eq!(doc["selected_task"], "fold", "a task with subtasks is selectable by name");
    for (body, want) in [
        (r#"{"tasks": [{"name": "", "subtasks": ["spread"]}]}"#, "task with no name"),
        (r#"{"tasks": ["wipe", {"name": "wipe", "subtasks": ["a"]}]}"#, "declared twice"),
        (r#"{"tasks": [{"name": "wipe", "subtasks": "a"}]}"#, "must be an array"),
    ] {
        let err = patch("{}", body).unwrap_err();
        assert!(err.contains(want), "{body}: {err}");
    }
}

/// `"mode": "standalone"` keeps a provisioned rig off the dashboard: no
/// credentials handed out, and nothing from the caches a production run left.
#[test]
fn asked_for_standalone_ignores_the_dashboard() {
    let config = r#"{
        "mode": "standalone", "tasks": ["wipe"],
        "identity": {"organization": "typed-in", "robot_id": "yubi-go"},
        "backend": {"base_url": "http://backend.example.com:8000/api", "api_key": "k"}
    }"#;
    let cfg = loaded_with("asked_standalone", config, Some(&profile()));
    assert!(cfg.standalone);
    assert_eq!(cfg.identity.organization, "typed-in", "the profile is not overlaid");
    assert_eq!(cfg.task(), "wipe");
    let dir =
        std::env::temp_dir().join(format!("yubigo_cfg_asked_standalone-{}", std::process::id()));
    assert_eq!(creds(dir.to_str().unwrap()), None, "no backend worker may run");

    let cfg = loaded_with("provisioned", PRODUCTION, None);
    assert!(!cfg.standalone && !cfg.has_task(), "production still waits for the queue");
}

#[test]
fn video_defaults_to_h264_unless_the_rig_feeds_the_airoa_pipeline() {
    let production = r#""mode": "production", "backend": {"base_url": "http://x", "api_key": "k"}"#;
    assert_eq!(parse("{}").video, Video::H264);
    assert_eq!(parse(&format!("{{{production}}}")).video, Video::Jpeg);
    assert_eq!(parse(r#"{"video": "jpeg"}"#).video, Video::Jpeg);
    assert_eq!(parse(&format!(r#"{{{production}, "video": "h264"}}"#)).video, Video::H264);
    assert_eq!(patch("{}", r#"{"video": "jpeg"}"#).unwrap()["video"], "jpeg");
    assert!(patch("{}", r#"{"video": "av1"}"#).unwrap_err().contains("h264 or jpeg"));
}

/// A rig that is production in name, with no key, a task picked locally, and
/// an `assigned.json` left from when it had a dashboard. The local pick
/// labels the take, and no stale queue reads as finished.
#[test]
fn a_local_pick_beats_a_stale_dashboard_cache() {
    let dir = std::env::temp_dir().join(format!("yubigo_cfg_stale_cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.to_str().unwrap();
    let old = Assignment { id: "e1".into(), task: "stack the cups".into(), ..Default::default() };
    Assignment::save_all(dir, &[old]).unwrap();
    let config = r#"{"mode": "production", "backend": {"base_url": "http://x/api"},
                     "tasks": ["stack the cups", "test"], "selected_task": "test"}"#;
    std::fs::write(format!("{dir}/config.json"), config).unwrap();
    let cfg = Config::load(dir);
    assert!(cfg.standalone);
    assert_eq!(cfg.task(), "test");
    assert_eq!(cfg.assignment, None, "a take is not bound to the stale row");
    assert!(!cfg.task_completed());
}

/// Pre-release builds' `camera_roles` gave each camera's slot, with its headset
/// camera in slot 3; this build gives each slot's camera. `[1, 3, 0]` is the case
/// that tells a real translation from swapping 3 for 2.
#[test]
fn older_camera_roles_are_translated() {
    let roles = |json: &str| parse(json).camera_roles.map(|r| r.ranks());
    assert_eq!(roles(r#"{"camera_roles": [3, 1, 0]}"#), Some([2, 1, 0]));
    assert_eq!(roles(r#"{"camera_roles": [1, 3, 0]}"#), Some([2, 0, 1]));
    assert_eq!(roles(r#"{"camera_roles": [1, 0, 2]}"#), Some([1, 0, 2]), "this build's own form");
    assert!(!parse("{}").head_camera, "the default rig has only the wrists");
    assert!(parse(r#"{"front_camera": true}"#).head_camera, "pre-release builds' name for it");
    assert!(!parse(r#"{"head_camera": false, "front_camera": true}"#).head_camera);
    assert!(
        parse(r#"{"keep_faulted_takes": true}"#).keep_faulted_episodes,
        "pre-release builds' name"
    );
    assert!(
        !parse(r#"{"keep_faulted_episodes": false, "keep_faulted_takes": true}"#)
            .keep_faulted_episodes
    );
    let err = patch("{}", r#"{"front_camera": true}"#).unwrap_err();
    assert!(err.contains("head_camera"), "the page may not write the old name: {err}");
    let err = patch("{}", r#"{"keep_faulted_takes": true}"#).unwrap_err();
    assert!(err.contains("keep_faulted_episodes"), "{err}");
}

#[test]
fn a_device_with_no_config_has_the_defaults() {
    let dir = std::env::temp_dir().join(format!("yubigo_cfg_absent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let cfg = Config::load(dir.to_str().unwrap());
    assert_eq!(cfg, Config::default());
    assert_eq!(cfg.backend, Backend::default(), "nothing to talk to yet");
    assert_eq!(cfg.task(), "untitled");
    assert_eq!(cfg.identity.organization, "");
    assert_eq!(cfg.identity.robot_type, "YubiGo");
}

#[test]
fn a_malformed_config_falls_back_to_the_defaults() {
    let dir = std::env::temp_dir().join(format!("yubigo_cfg_broken-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(FILE), "{ this is not json").unwrap();

    assert_eq!(Config::load(dir.to_str().unwrap()), Config::default());
}

#[test]
fn a_backend_is_read_as_written() {
    let cfg = parse(r#"{"backend": {"base_url": "http://rig:8000/api", "api_key": "k"}}"#);
    assert_eq!(cfg.backend.base_url, "http://rig:8000/api");
    assert_eq!(cfg.backend.api_key, "k");
}

#[test]
fn a_partial_config_keeps_the_defaults_for_what_it_omits() {
    let cfg = parse(r#"{"identity": {"site": "trc", "robot_id": "yubi-go-001"}}"#);
    assert_eq!(cfg.identity.site, "trc");
    assert_eq!(cfg.identity.robot_id, "yubi-go-001");
    assert_eq!(cfg.identity.robot_type, "YubiGo", "omitted, so unchanged");
    assert_eq!(cfg.identity.organization, "");
    assert_eq!(cfg.backend, Backend::default());
}

#[test]
fn the_folder_for_new_takes_is_stored_clean_and_refused_when_it_climbs() {
    assert_eq!(patch("{}", r#"{"folder": "/cups//day1/"}"#).unwrap()["folder"], "cups/day1");
    assert!(patch("{}", r#"{"folder": "../x"}"#).is_err());
    assert!(patch("{}", r#"{"folder": 3}"#).is_err());
    assert_eq!(patch("{}", r#"{"announce_steps": false}"#).unwrap()["announce_steps"], false);
    assert!(Config::default().announce_steps, "on unless switched off");
    assert_eq!(patch("{}", r#"{"require_operator": false}"#).unwrap()["require_operator"], false);
    assert!(Config::default().require_operator, "upstream's default");
}

fn patch(current: &str, patch: &str) -> Result<serde_json::Value, String> {
    Config::merge(&serde_json::from_str(current).unwrap(), &serde_json::from_str(patch).unwrap())
}

#[test]
fn every_identity_field_may_be_left_blank() {
    let doc = patch(
        r#"{"identity": {"robot_type": "Custom", "robot_id": "rig-7", "site": "trc"}}"#,
        r#"{"identity": {"organization": "", "site": "", "location": "", "operator": "",
                         "robot_type": " ", "robot_id": ""}}"#,
    )
    .unwrap();

    let cfg = Config::from_raw(serde_json::from_value::<Raw>(doc).unwrap());
    assert_eq!(cfg.identity.site, "");
    assert_eq!(cfg.identity.robot_type, "");
    assert_eq!(cfg.identity.robot_id, "");
}

#[test]
fn a_commit_leaves_alone_what_it_does_not_mention() {
    let doc = patch(
        r#"{"identity": {"site": "trc", "operator": "tarou"},
            "backend": {"base_url": "http://rig:8000/api", "api_key": "secret"}}"#,
        r#"{"camera_roles": [2, 0, 1]}"#,
    )
    .unwrap();

    let cfg = Config::from_raw(serde_json::from_value::<Raw>(doc).unwrap());
    assert_eq!(cfg.identity.operator, "tarou", "an untouched field");
    assert_eq!(cfg.backend.api_key, "secret", "the credential the picker cannot see");
    assert_eq!(cfg.camera_roles.expect("the mapping landed").ranks(), [2, 0, 1]);
}

#[test]
fn an_enriched_row_reads_as_an_assignment() {
    let body = r#"[{
        "episodeId": "e1",
        "taskId": "t1",
        "taskVersionId": "v1",
        "assignedRobotId": "r1",
        "task": {"id": "t1", "name": "ngc_test2", "description": ""},
        "subtasks": [
            {"id": "a", "subtask_id": "d1", "name": "pick", "orderIndex": 1, "status": 1},
            {"id": "b", "subtask_id": "d2", "name": "place", "orderIndex": 2, "status": 0}
        ]
    }]"#;
    let rows: Vec<Assignment> = serde_json::from_str(body).unwrap();
    assert_eq!(rows[0].id, "e1");
    assert_eq!(rows[0].task, "ngc_test2", "the name lives inside the task object");
    assert_eq!(rows[0].task_id, "t1");
    assert_eq!(rows[0].task_version_id, "v1");
    let names: Vec<&str> = rows[0].subtasks.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["pick", "place"], "airoa_core sorted them");

    let written = serde_json::to_string(&rows[0]).unwrap();
    assert!(written.contains(r#""task":"ngc_test2""#), "the cache keeps a name: {written}");
    assert_eq!(serde_json::from_str::<Assignment>(&written).unwrap(), rows[0]);
}

#[test]
fn a_null_field_reads_as_empty_rather_than_failing_the_whole_payload() {
    let body = r#"[{
        "id": "e1", "task_name": null, "task_id": null, "task_version_id": null,
        "user_id": null, "subtasks": null
    }]"#;
    let rows: Vec<Assignment> = serde_json::from_str(body).unwrap();
    assert_eq!(rows[0].id, "e1");
    assert_eq!(rows[0].task, "");
    assert!(rows[0].subtasks.is_empty());

    let steps: Vec<Subtask> =
        serde_json::from_str(r#"[{"id":"s","name":"pick","status":null}]"#).unwrap();
    assert_eq!(steps[0].status, 0);
}

#[test]
fn the_wire_names_map_onto_the_cached_names() {
    let body = r#"[{"id": "e1", "task_name": "fold the towel", "task_id": "t1"}]"#;
    let rows: Vec<Assignment> = serde_json::from_str(body).unwrap();
    assert_eq!(rows[0].task, "fold the towel");
    assert_eq!(rows[0].task_id, "t1");
    assert!(!rows[0].spent, "spent is ours, never the backend's");

    let written = serde_json::to_string(&rows[0]).unwrap();
    assert!(written.contains(r#""task":"fold the towel""#), "{written}");
    assert!(!written.contains("task_name"), "{written}");
    let back: Assignment = serde_json::from_str(&written).unwrap();
    assert_eq!(back, rows[0]);
}

#[test]
fn the_current_subtask_is_the_in_progress_one_or_the_first_outstanding() {
    let steps = |statuses: &[u8]| Assignment {
        subtasks: statuses.iter().map(|&status| Subtask { status, ..Default::default() }).collect(),
        ..Default::default()
    };
    assert_eq!(steps(&[2, 1, 0]).current_subtask().unwrap().0, 1);
    assert_eq!(steps(&[2, 0, 0]).current_subtask().unwrap().0, 1);
    assert!(steps(&[2, 3]).current_subtask().is_none());
}

#[test]
fn the_queue_survives_a_round_trip_through_the_cache() {
    let dir = std::env::temp_dir().join(format!("yubigo-assigned-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let dir = dir.to_str().unwrap();

    assert_eq!(Assignment::load_all(dir), None, "never asked is not an empty queue");
    let assigned = vec![Assignment {
        id: "49f0d208".into(),
        task: "ngc_test2".into(),
        task_id: "d2fe3261".into(),
        task_version_id: "73d77c2a".into(),
        spent: false,
        subtasks: vec![Subtask {
            id: "38d0f73c".into(),
            name: "pick and place ECU".into(),
            status: 1,
        }],
    }];
    Assignment::save_all(dir, &assigned).unwrap();
    assert_eq!(Assignment::load_all(dir), Some(assigned));

    Assignment::save_all(dir, &[]).unwrap();
    assert_eq!(Assignment::load_all(dir), Some(Vec::new()));
    assert_eq!(Config::load(dir).task(), "ngc_test2");
    std::fs::remove_dir_all(dir).ok();
}

#[test]
fn a_field_that_does_not_exist_is_an_error_rather_than_a_no_op() {
    for (body, want) in [
        (r#"{"nonsense": 1}"#, "not a configurable field"),
        (r#"{"identity": {"colour": "red"}}"#, "not a field"),
        (r#"{"selected_task": "ngc_test2"}"#, "not one of the declared tasks"),
        (r#"{"mode": "production"}"#, "needs a backend URL"),
        (r#"{"backend": {"base_url": "backend.example.com:8000"}}"#, "scheme"),
        (r#"{"backend": {"api_key": "k"}}"#, "needs backend.base_url"),
        (r#"{"head_camera": "yes"}"#, "true or false"),
        (r#"{"camera_roles": "left"}"#, "must be an array"),
        (r#"{"camera_roles": [0, 0, 1]}"#, "must be a permutation"),
        (r#"{"camera_ports": ["1-1.2"]}"#, "must be an object"),
        (r#"{"camera_ports": {"chest": "1-1.2"}}"#, "is not a camera"),
        (r#"{"camera_ports": {"left_wrist": "1-1.2", "head": "1-1.2"}}"#, "two cameras"),
    ] {
        let err = patch("{}", body).unwrap_err();
        assert!(err.contains(want), "{body} gave {err:?}");
    }
}

/// The page's edits: a patch never
/// clears what it does not mention (the page never sees the API key), and
/// re-declaring the task list drops a selection it no longer contains.
#[test]
fn the_page_edits_a_patch_at_a_time() {
    let start = r#"{"backend": {"base_url": "http://x/api", "api_key": "secret"}}"#;
    let doc = patch(start, r#"{"identity": {"site": "trc"}, "mode": "production"}"#).unwrap();
    assert_eq!(doc["backend"]["api_key"], "secret", "an identity edit keeps the key");
    assert_eq!(doc["identity"]["site"], "trc");

    let doc =
        patch(&doc.to_string(), r#"{"backend": {"base_url": ""}, "mode": "standalone"}"#).unwrap();
    assert!(doc.get("backend").is_none(), "clearing the address clears the key with it");

    let doc = patch("{}", r#"{"tasks": ["wipe", " ", "fold"], "selected_task": "fold"}"#).unwrap();
    assert_eq!(doc["tasks"], serde_json::json!(["wipe", "fold"]));
    assert_eq!(doc["selected_task"], "fold");
    let doc = patch(&doc.to_string(), r#"{"tasks": ["wipe"]}"#).unwrap();
    assert!(doc.get("selected_task").is_none());

    let doc = patch("{}", r#"{"head_camera": false, "keep_faulted_episodes": true}"#).unwrap();
    assert_eq!(
        (doc["head_camera"].as_bool(), doc["keep_faulted_episodes"].as_bool()),
        (Some(false), Some(true))
    );
}

/// The page sends the URL on every save and the key only when typed, so a
/// save that keeps the host keeps the key, and one that moves it asks.
#[test]
fn the_key_goes_to_another_host_only_when_typed_again() {
    let start = r#"{"backend": {"base_url": "http://rig.lan:8000/api", "api_key": "secret"}}"#;
    let moved = patch(start, r#"{"backend": {"base_url": "http://evil.lan:8000/api"}}"#);
    assert!(moved.unwrap_err().contains("enter the API key again"));
    let port = patch(start, r#"{"backend": {"base_url": "http://rig.lan:9000/api"}}"#);
    assert!(port.is_err(), "another port is another service");

    let doc = patch(start, r#"{"backend": {"base_url": "http://rig.lan:8000/api/"}}"#).unwrap();
    assert_eq!(doc["backend"]["api_key"], "secret", "same host, the page's every save");
    let retyped = r#"{"backend": {"base_url": "http://new.lan/api", "api_key": "k2"}}"#;
    let doc = patch(start, retyped).unwrap();
    assert_eq!(doc["backend"]["api_key"], "k2");
    let doc = patch("{}", r#"{"backend": {"base_url": "http://new.lan/api"}}"#).unwrap();
    assert_eq!(doc["backend"]["base_url"], "http://new.lan/api", "no key to send yet");
}

#[test]
fn a_debug_print_says_whether_a_key_is_set_and_not_what_it_is() {
    let backend = Backend { base_url: "http://rig.lan/api".into(), api_key: "hunter2".into() };
    let shown = format!("{:?}", Config { backend, ..Config::default() });
    assert!(!shown.contains("hunter2") && shown.contains("<redacted>"), "{shown}");
    assert!(shown.contains("http://rig.lan/api"));
}

/// A file that cannot be parsed reads as `{}`, so saving an edit over it
/// would keep the edit and lose every other setting.
#[test]
fn an_unreadable_config_is_not_overwritten() {
    let dir = std::env::temp_dir().join(format!("yubigo_cfg_unreadable-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let broken = r#"{"backend": {"base_url": "http://rig.lan/api", "api_key": "k"},"#;
    std::fs::write(dir.join(FILE), broken).unwrap();
    let dir = dir.to_str().unwrap();

    let err = Config::save(dir, &serde_json::json!({"video": "jpeg"})).unwrap_err();
    assert!(err.to_string().contains("not valid JSON"), "{err}");
    assert_eq!(std::fs::read_to_string(Path::new(dir).join(FILE)).unwrap(), broken);

    std::fs::remove_file(Path::new(dir).join(FILE)).unwrap();
    Config::save(dir, &serde_json::json!({"video": "jpeg"})).unwrap();
    Config::save(dir, &serde_json::json!({"video": "h264"})).unwrap();
}
