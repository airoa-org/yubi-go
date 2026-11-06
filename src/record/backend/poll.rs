use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use log::{info, warn};

use crate::hub::{Condition, GroupVerdict};
use crate::misc::clock::rfc3339;
use crate::misc::config::{creds, Assignment, Config, Profile};

pub const BEAT: Duration = Duration::from_secs(30);

/// How often the backend thread wakes. Upstream's `process_step` runs at the
/// same 1 Hz, and it is the pace the flush inherits: a backlog that appears
/// while the backend is up is retried on the next tick rather than the next
/// beat.
const TICK: Duration = Duration::from_secs(1);

/// The floor between two beats sent because the gate changed, so a flapping
/// condition cannot turn the heartbeat into a stream.
const GATE_THROTTLE: Duration = Duration::from_secs(2);

/// The profile is asked for this often until it answers.
const PROFILE: Duration = Duration::from_secs(60);

#[derive(Debug, Default, Clone)]
pub struct Status {
    pub robot_type: String,
    pub reported_at_ns: u64,
    pub battery: (i64, bool),
    pub uptime_sec: f64,
    pub disk: Option<crate::misc::disk::Usage>,
    pub recording: bool,
    pub gate_level: u8,
    pub gate: Vec<GroupVerdict>,
}

impl Status {
    fn summary(&self) -> String {
        let battery = match self.battery {
            (pct, _) if pct < 0 => "battery unknown".to_string(),
            (pct, true) => format!("battery {pct}% charging"),
            (pct, false) => format!("battery {pct}%"),
        };
        let disk = match self.disk {
            Some(disk) => format!("{:.1} GB free", disk.free as f64 / 1e9),
            None => "disk unknown".into(),
        };
        let refusing: Vec<&Condition> =
            self.gate.iter().flat_map(|g| &g.results).filter(|c| !c.passed).collect();
        let gate = match refusing.first() {
            None => format!("gate {}", self.gate_level),
            Some(first) => format!(
                "gate {} ({} refusing: {} {})",
                self.gate_level,
                refusing.len(),
                first.name,
                first.reason
            ),
        };
        format!("{battery}, {disk}, {gate}")
    }

    pub fn inputs(&self) -> airoa_sdk::StatusInputs {
        let level = self.gate.iter().map(|g| g.level).max().unwrap_or(0).max(self.gate_level);
        let groups: serde_json::Map<String, serde_json::Value> = self
            .gate
            .iter()
            .map(|group| {
                let body = serde_json::json!({
                    "level": group.level,
                    "settled": group.settled,
                    "conditions": group.results,
                });
                (group.name.clone(), body)
            })
            .collect();
        airoa_sdk::StatusInputs {
            robot_type: self.robot_type.clone(),
            battery_pct: self.battery.0,
            battery_charging: self.battery.1,
            disk_free: self.disk.map(|d| d.free).unwrap_or(0),
            disk_used: self.disk.map(|d| d.used).unwrap_or(0),
            is_recording: self.recording,
            gate_level: level,
            gate_conditions: serde_json::json!({"gate_level": level, "groups": groups}),
            uptime_sec: (self.uptime_sec * 10.0).round() / 10.0,
            reported_at: Some(rfc3339(self.reported_at_ns)),
            connection_quality_pct: 100,
        }
    }
}

static OFFLINE_SINCE_NS: AtomicU64 = AtomicU64::new(0);

pub fn offline_since_ns() -> Option<u64> {
    match OFFLINE_SINCE_NS.load(Ordering::Relaxed) {
        0 => None,
        ns => Some(ns),
    }
}

fn note_reachable(reachable: bool, at_ns: u64) {
    match reachable {
        true => OFFLINE_SINCE_NS.store(0, Ordering::Relaxed),
        false => {
            let _ =
                OFFLINE_SINCE_NS.compare_exchange(0, at_ns, Ordering::Relaxed, Ordering::Relaxed);
        }
    }
}

/// Fetch `/robot/me` and cache it. Answers whether the profile is now known,
/// so the caller can stop asking.
fn profile_once(dir: &str) -> bool {
    match creds(dir)
        .map(|(base_url, api_key)| airoa_sdk::backend::me::<Profile>(&base_url, &api_key))
    {
        Some(Ok(profile)) => {
            if profile.is_named() {
                info!(
                    "backend: this robot is {} ({}), org={} site={} location={}",
                    profile.robot_id,
                    profile.robot_type,
                    profile.organization,
                    profile.site,
                    profile.location
                );
                match profile.save(dir) {
                    Ok(()) => info!("backend: profile cached"),
                    Err(e) => warn!("backend: profile not cached: {e}"),
                }
                return true;
            }
            warn!("backend: /robot/me answered without a robot id");
        }
        Some(Err(e)) => warn!("backend: {e}"),
        None => {}
    }
    false
}

fn sync_once(dir: &str, last: &mut Option<Vec<Assignment>>) {
    let completed: Vec<String> = Assignment::load_all(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.spent)
        .map(|a| a.id)
        .collect();
    let config = Config::load(dir);
    let operator = &config.identity.operator;
    // Picking needs every queued episode, and the selector serves only the
    // next one. An empty list still goes to the selector, which repeats the
    // last episode.
    let picking = config.operator_picks_task && config.production();
    match creds(dir).map(|(base_url, api_key)| {
        let mut listed = match picking {
            true => airoa_sdk::backend::list_episodes::<Assignment>(
                &base_url, &api_key, operator, &completed,
            )?,
            false => Vec::new(),
        };
        // A task the operator has taken the last episode of is copied from
        // that take, so it stays pickable however many takes nobody planned.
        // The backend refuses the copy until the take's own finish or cancel
        // has landed, which is what makes the copy that take's task.
        if let Some(task) = crate::record::backend::episode::ended_task().filter(|_| picking) {
            if listed.iter().any(|a| a.task == task) {
                crate::record::backend::episode::ended_task_handled();
            } else if let Some(copy) =
                airoa_sdk::backend::repeat_last::<Assignment>(&base_url, &api_key, operator)?
            {
                if copy.task != task {
                    warn!("backend: repeating {task:?} gave {:?} ({})", copy.task, copy.id);
                }
                crate::record::backend::episode::ended_task_handled();
                listed.push(copy);
            }
        }
        if !listed.is_empty() {
            return Ok((listed, String::new()));
        }
        airoa_sdk::backend::select_episode::<Assignment>(
            &base_url, &api_key, true, operator, &completed,
        )
        .map(|s| (s.episode.into_iter().collect(), s.reason))
    }) {
        Some(Ok((assigned, reason))) => {
            note_reachable(true, 0);
            if last.as_ref() != Some(&assigned) {
                match assigned.first() {
                    Some(next) => info!("backend: next episode is {:?} ({})", next.task, next.id),
                    None => {
                        info!("backend: nothing queued for this robot ({reason})")
                    }
                }
                if let Err(e) = Assignment::save_all(dir, &assigned) {
                    warn!("backend: queue not cached: {e}");
                }
                *last = Some(assigned);
            }
        }
        Some(Err(e)) => {
            note_reachable(false, crate::misc::clock::realtime_ns());
            warn!("backend: {e}");
        }
        None => {}
    }
}

/// The timers that talk to the backend, on one thread so they cannot collide
/// with each other and pay for a second component instance. `episode::report`
/// spawns a thread per episode boundary, so this is not the only caller. Each
/// errand keeps its own due time; the loop wakes for the fastest of them.
///
/// The profile is asked for until it answers once — it does not change while
/// the rig is running.
pub fn poll(dir: String, period: Duration, sample: impl Fn() -> Status + Send + 'static) {
    std::thread::spawn(move || {
        let mut named = false;
        let mut profile_at = std::time::Instant::now();
        let mut assigned_at = std::time::Instant::now();
        let mut assigned: Option<Vec<Assignment>> = None;

        let mut failures: u32 = 0;
        let mut first = true;
        let mut waited = false;
        let mut sent = std::time::Instant::now() - period;
        let mut gate: Option<Vec<(String, bool)>> = None;

        loop {
            if !crate::misc::core::up() {
                if !waited {
                    waited = true;
                    info!("backend: heartbeats wait for airoa_core to start");
                }
                std::thread::sleep(TICK);
                continue;
            }
            let now = std::time::Instant::now();
            // The heartbeat goes first. Both errands below block for the HTTP
            // timeout when the backend is unreachable, and running them ahead
            // of it would stretch a 30 s cadence by two timeouts in exactly
            // the situation the beat exists to report.

            let status = sample();
            let conditions: Vec<(String, bool)> = status
                .gate
                .iter()
                .flat_map(|g| g.results.iter().map(|c| (c.name.clone(), c.passed)))
                .collect();
            let changed = gate.as_ref().is_some_and(|was| *was != conditions);
            gate = Some(conditions);
            if !(sent.elapsed() >= period || (changed && sent.elapsed() >= GATE_THROTTLE)) {
                std::thread::sleep(TICK);
                continue;
            }
            sent = std::time::Instant::now();
            match creds(&dir).map(|(base_url, api_key)| {
                airoa_sdk::backend::status(&base_url, &api_key, &status.inputs())
            }) {
                Some(Ok(_)) => {
                    if first {
                        info!("backend: heartbeat accepted — {}", status.summary());
                        first = false;
                    } else if failures > 0 {
                        info!("backend: heartbeat accepted after {failures} failures");
                    }
                    failures = 0;
                    // `sync_once` is the only other place that clears this,
                    // and it is skipped while offline.
                    note_reachable(true, 0);
                    // The beat has just proved the backend answers, so a
                    // backlog costs no wasted timeout here.
                    crate::record::backend::queue::flush_once(&dir);
                }
                Some(Err(e)) => {
                    failures += 1;
                    if failures <= 3 {
                        warn!("backend: {e}");
                    } else if failures.is_multiple_of(60) {
                        warn!("backend: heartbeat still failing after {failures}: {e}");
                    }
                }
                None => {}
            }

            // Only once the beat has been sent, and only while the backend is
            // answering — an outage makes these two a waste of a timeout.
            if offline_since_ns().is_none() {
                if !named && now >= profile_at {
                    profile_at = now + PROFILE;
                    named = profile_once(&dir);
                }
                // A finished take asks for the next one through
                // `episode::refresh_wanted`. This runs only after a beat, so
                // the answer comes with the next beat, or 2 s after a gate
                // change (`GATE_THROTTLE`).
                if now >= assigned_at
                    || crate::record::backend::episode::refresh_wanted()
                    || crate::record::backend::episode::ended_task().is_some()
                {
                    assigned_at = now + period;
                    sync_once(&dir, &mut assigned);
                }
            }
            std::thread::sleep(TICK);
        }
    });
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::test_http::{serve, serve_routes, up};

    #[test]
    fn wasi_http_sends_the_api_key_and_returns_the_profile() {
        up();
        let (base, rx) = serve(r#"{"id":"yubi-go-001","site_name":"TRC"}"#);
        let answer: Profile = airoa_sdk::backend::me(&base, "k").unwrap();
        assert_eq!(answer.robot_id, "yubi-go-001");
        let request = rx.recv().unwrap();
        assert!(request.starts_with("GET /robot/me"), "{request}");
        assert!(request.to_ascii_lowercase().contains("x-api-key: k"), "{request}");
    }

    #[test]
    fn a_report_carries_upstream_s_own_occurred_at() {
        up();
        let (base, rx) = serve("{}");
        airoa_sdk::backend::report(&base, "k", airoa_sdk::backend::Report::Start, "e1").unwrap();
        let request = rx.recv().unwrap();
        assert!(request.starts_with("POST /robot/episodes/e1/start"), "{request}");
        assert!(request.contains("occurred_at"), "upstream stamps its own clock: {request}");
    }

    #[test]
    fn selecting_from_an_empty_backend_answers_a_reason_rather_than_an_episode() {
        up();
        let (base, _rx) = serve("[]");
        let v =
            airoa_sdk::backend::select_episode::<Assignment>(&base, "k", false, "", &[]).unwrap();
        assert!(v.episode.is_none(), "{:?}", v.episode.map(|a| a.id));
        assert_eq!(v.reason, "no episodes available");
    }

    /// Newest first on the wire, as the backend lists them; no subtasks in a
    /// list row, so the next of each task is fetched whole.
    #[test]
    fn listing_serves_the_queue_oldest_first_with_the_next_of_each_task_whole() {
        up();
        let (base, rx) = serve_routes(&[
            (
                "/robot/episodes/e1",
                r#"{"id": "e1", "task_id": "t-wipe", "task_name": "wipe",
                "robot_id": "r", "user_id": "u", "subtasks": [{"id": "s1", "name": "spray"}]}"#,
            ),
            (
                "/robot/episodes/e2",
                r#"{"id": "e2", "task_id": "t-fold", "task_name": "fold",
                "robot_id": "r", "user_id": "u", "subtasks": []}"#,
            ),
            (
                "/robot/episodes",
                r#"[
                {"id": "e4", "task_id": "t-fold", "task_name": "fold", "robot_id": "r", "user_id": "u"},
                {"id": "e3", "task_id": "t-wipe", "task_name": "wipe", "robot_id": "r", "user_id": "u"},
                {"id": "e2", "task_id": "t-fold", "task_name": "fold", "robot_id": "r", "user_id": "u"},
                {"id": "e1", "task_id": "t-wipe", "task_name": "wipe", "robot_id": "r", "user_id": "u"},
                {"id": "e0", "task_id": "t-wipe", "task_name": "wipe", "robot_id": "r", "user_id": "u"}]"#,
            ),
        ]);
        let listed =
            airoa_sdk::backend::list_episodes::<Assignment>(&base, "k", "", &["e0".into()])
                .unwrap();
        let ids: Vec<&str> = listed.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["e1", "e2", "e3", "e4"], "oldest first, the completed one gone");
        assert_eq!(listed[1].task, "fold");
        assert_eq!(listed[0].subtasks.len(), 1, "the next wipe carries its steps");
        let fetched: Vec<String> = rx
            .try_iter()
            .filter_map(|r| r.split(' ').nth(1).map(str::to_string))
            .filter(|p| p.starts_with("/robot/episodes/"))
            .collect();
        assert_eq!(fetched, ["/robot/episodes/e1", "/robot/episodes/e2"], "one fetch per task");
    }

    #[test]
    fn a_picked_task_whose_last_episode_was_taken_is_copied_from_that_take() {
        up();
        let dir = std::env::temp_dir().join(format!("yubigo_poll_copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_str().unwrap().to_string();
        let (base, rx) = serve_routes(&[
            (
                "/robot/episodes/repeat-last",
                r#"{"id": "e9", "task_id": "t-wipe",
                "task_name": "wipe", "robot_id": "r", "user_id": "u"}"#,
            ),
            (
                "/robot/episodes/e9",
                r#"{"id": "e9", "task_id": "t-wipe", "task_name": "wipe",
                "robot_id": "r", "user_id": "u", "subtasks": [{"id": "s1", "name": "spray"}]}"#,
            ),
            (
                "/robot/episodes/e2",
                r#"{"id": "e2", "task_id": "t-fold", "task_name": "fold",
                "robot_id": "r", "user_id": "u"}"#,
            ),
            (
                "/robot/episodes",
                r#"[{"id": "e2", "task_id": "t-fold", "task_name": "fold",
                "robot_id": "r", "user_id": "u"}]"#,
            ),
        ]);
        let config = serde_json::json!({
            "backend": {"base_url": base, "api_key": "k"},
            "operator_picks_task": true, "selected_task": "wipe",
        });
        std::fs::write(format!("{dir}/config.json"), config.to_string()).unwrap();

        crate::record::backend::episode::take_ended("wipe");
        sync_once(&dir, &mut None);
        assert_eq!(crate::record::backend::episode::ended_task(), None, "copied once");
        let cfg = Config::load(&dir);
        let next = cfg.assignment.expect("the copy is the next wipe");
        assert_eq!((next.id.as_str(), next.subtasks.len()), ("e9", 1));
        assert!(rx.try_iter().any(|r| r.starts_with("POST /robot/episodes/repeat-last")));
    }

    fn group(level: u8, results: Vec<Condition>) -> GroupVerdict {
        GroupVerdict {
            name: "grippers".into(),
            level,
            settled: true,
            settle_is_initial: false,
            settle_start_time: 0.0,
            failing: Vec::new(),
            results,
        }
    }

    fn condition(name: &str, passed: bool, reason: &str) -> Condition {
        Condition {
            name: name.into(),
            passed,
            reason: reason.into(),
            escalation: 1,
            settle_exempt: false,
        }
    }

    #[test]
    fn a_refusal_reaches_the_dashboard_in_the_panel_s_own_words() {
        up();
        let status = Status {
            gate_level: 0,
            gate: vec![group(
                1,
                vec![
                    condition(
                        "left gripper",
                        false,
                        crate::record::gate::Fault::Uncalibrated.text(),
                    ),
                    condition("right gripper", true, "ok"),
                ],
            )],
            ..Default::default()
        };
        let v: serde_json::Value =
            serde_json::from_str(&airoa_sdk::status_payload(&status.inputs()).unwrap()).unwrap();
        let gate = &v["status"]["gate_conditions"];
        assert_eq!(gate["gate_level"], 1);
        let group = &gate["groups"]["grippers"];
        assert_eq!(group["level"], 1);
        assert_eq!(group["settled"], true);
        assert_eq!(group["conditions"][0]["name"], "left gripper");
        assert_eq!(group["conditions"][0]["passed"], false);
        assert_eq!(group["conditions"][0]["reason"], "never calibrated");
        assert_eq!(group["conditions"][0]["escalation"], 1);
        assert_eq!(group["conditions"][1]["passed"], true);
        assert_eq!(group["conditions"][1]["reason"], "ok");
        let metrics = v["status"]["metrics"].as_array().unwrap();
        let level = metrics.iter().find(|m| m["name"] == "gate_level").unwrap();
        assert_eq!(level["value"], 1);
    }
}
