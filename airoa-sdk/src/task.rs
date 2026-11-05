use serde::{Deserialize, Serialize};

use crate::Error;

#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
pub struct Display {
    pub status: String,
    pub next_task: String,
    pub subtask_elapsed_sec: f64,
    pub episode_elapsed_sec: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Accept,
    Reject,
    Cancel,
    Rewind,
    FailAdvance,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "method", content = "params", rename_all = "snake_case")]
pub enum Command {
    View,
    Start,
    Transit { action: Action, reason: String },
    StopSubtask { success: bool },
    StopRecording { save: bool, reason: String },
}

/// One call from upstream's `HostAdapter`, one arm, with upstream's names and
/// arguments. Logging sits beside the recording calls because upstream puts
/// it in the same protocol.
///
/// Deliberately not `#[non_exhaustive]`, so a variant added here breaks every
/// host's `match` until the host handles it. A method upstream adds is not
/// caught at build time: `Effect::parse` matches on the method name, and
/// refuses one it does not know with `unknown host method` at run time.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Carries the episode upstream is starting: the one handed over as
    /// pending, or the one its selector fetched or repeated by itself.
    StartRosbagRecording {
        episode: serde_json::Value,
    },
    StopRosbagRecording,
    CancelRosbagRecording,
    RunRecordRunner,
    SetEpisodeUuid(String),
    SetEpisode(serde_json::Value),
    RecordLabelAndSegment {
        name: String,
        start: f64,
        end: Option<f64>,
        success: bool,
    },
    OverrideLastSegmentSuccess {
        success: bool,
    },
    RemoveLastSegment,
    PublishRecordingBlock {
        reason: String,
    },
    LogInfo(String),
    LogWarning(String),
    LogError(String),
}

impl Effect {
    fn parse(need: &serde_json::Value) -> Result<Effect, Error> {
        let method = need["method"].as_str().unwrap_or_default();
        let args = need["args"].as_array().map(Vec::as_slice).unwrap_or_default();
        let kwargs = &need["kwargs"];
        let missing = |what: &str| Error::Python(format!("{method}: missing {what}"));
        let text = |i: usize, what: &str| {
            args.get(i)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| missing(what))
        };
        Ok(match method {
            "start_rosbag_recording" => {
                Effect::StartRosbagRecording { episode: need["episode"].clone() }
            }
            "stop_rosbag_recording" => Effect::StopRosbagRecording,
            "cancel_rosbag_recording" => Effect::CancelRosbagRecording,
            "run_record_runner" => Effect::RunRecordRunner,
            "set_episode_uuid" => Effect::SetEpisodeUuid(text(0, "episode id")?),
            "set_episode" => {
                Effect::SetEpisode(args.first().cloned().ok_or_else(|| missing("episode"))?)
            }
            "remove_last_segment" => Effect::RemoveLastSegment,
            "override_last_segment_success" => Effect::OverrideLastSegmentSuccess {
                success: args
                    .first()
                    .or_else(|| kwargs.get("success"))
                    .and_then(serde_json::Value::as_bool)
                    .ok_or_else(|| missing("success"))?,
            },
            "publish_recording_block" => {
                Effect::PublishRecordingBlock { reason: text(0, "reason").unwrap_or_default() }
            }
            "record_label_and_segment" => {
                let at = |i: usize, name: &str| {
                    args.get(i)
                        .filter(|v| !v.is_null())
                        .or_else(|| kwargs.get(name).filter(|v| !v.is_null()))
                };
                Effect::RecordLabelAndSegment {
                    name: at(0, "name")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| missing("name"))?,
                    start: at(1, "start")
                        .and_then(serde_json::Value::as_f64)
                        .ok_or_else(|| missing("start"))?,
                    end: at(2, "end").and_then(serde_json::Value::as_f64),
                    success: at(3, "success").and_then(serde_json::Value::as_bool).unwrap_or(true),
                }
            }
            "log_info" => Effect::LogInfo(text(0, "message").unwrap_or_default()),
            "log_warning" => Effect::LogWarning(text(0, "message").unwrap_or_default()),
            "log_error" => Effect::LogError(text(0, "message").unwrap_or_default()),
            other => return Err(Error::Python(format!("unknown host method {other}"))),
        })
    }

    /// Upstream declares three answer shapes: `tuple[bool, str]`, `bool`, and
    /// `None`. Which one an effect owes is a property of the effect, not of
    /// what the handler chose to report.
    fn answer(&self, report: &Report) -> serde_json::Value {
        match self {
            Effect::StartRosbagRecording { .. } => {
                serde_json::json!([
                    report.failed.is_none(),
                    report.failed.clone().unwrap_or_default()
                ])
            }
            // `-> None` upstream.
            Effect::RecordLabelAndSegment { .. }
            | Effect::PublishRecordingBlock { .. }
            | Effect::LogInfo(_)
            | Effect::LogWarning(_)
            | Effect::LogError(_) => serde_json::Value::Null,
            // `-> bool` upstream. Listed rather than wildcarded so a new
            // variant has to choose its answer shape.
            Effect::StopRosbagRecording
            | Effect::CancelRosbagRecording
            | Effect::RunRecordRunner
            | Effect::SetEpisodeUuid(_)
            | Effect::SetEpisode(_)
            | Effect::OverrideLastSegmentSuccess { .. }
            | Effect::RemoveLastSegment => serde_json::json!(report.failed.is_none()),
        }
    }
}

/// What the rig did. `Report::done()` is "carried it out, nothing else moved";
/// `Report::failed(why)` is upstream's own vocabulary for "could not", and is
/// an ordinary answer rather than an `Err`.
///
/// The two setters exist because upstream keeps its own view of these facts
/// and will not re-read them: say so when they change.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Report {
    failed: Option<String>,
    recording: Option<bool>,
    segments: Option<Count>,
    abort: Option<String>,
}

/// How many segments the episode holds after an effect. Upstream keeps the
/// running total, so a host that only added one need not know it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Count {
    Exactly(usize),
    OneMore,
}

impl Report {
    pub fn done() -> Self {
        Self::default()
    }

    pub fn failed(why: impl Into<String>) -> Self {
        Self { failed: Some(why.into()), ..Self::default() }
    }

    /// The rig cannot continue at all: stop the run and let nothing upstream
    /// act on it. `failed` is "could not do this one thing, carry on";
    /// `abort` is "do not publish anything about this episode".
    pub fn abort(why: impl Into<String>) -> Self {
        Self { abort: Some(why.into()), ..Self::default() }
    }

    /// The rig started or stopped recording as a result of this effect.
    pub fn recording(mut self, on: bool) -> Self {
        self.recording = Some(on);
        self
    }

    /// How many segments the episode holds now.
    pub fn segments(mut self, count: usize) -> Self {
        self.segments = Some(Count::Exactly(count));
        self
    }

    /// The episode gained one segment. Upstream counts them, so this says
    /// "one more than you had" without the caller tracking the total.
    pub fn added_segment(mut self) -> Self {
        self.segments = Some(Count::OneMore);
        self
    }

    fn facts(&self, so_far: usize) -> serde_json::Value {
        let mut facts = serde_json::Map::new();
        if let Some(on) = self.recording {
            facts.insert("is_recording".into(), serde_json::json!(on));
        }
        if let Some(count) = self.segments {
            let n = match count {
                Count::Exactly(n) => n,
                Count::OneMore => so_far + 1,
            };
            facts.insert("segment_count".into(), serde_json::json!(n));
        }
        serde_json::Value::Object(facts)
    }
}

/// The facts that do not change between calls in one session. Upstream reads
/// the operator from the backend's `/robot/me` itself, so it is not here.
#[derive(Default, Clone, PartialEq)]
pub struct Identity {
    pub base_url: String,
    pub api_key: String,
    /// The offline queue's guest path, shared with
    /// [`backend::Client`](crate::backend::Client) so the FSM's own reports
    /// and the episode boundaries keep one order. Empty means no queue.
    pub queue_path: String,
    /// A rig with no AIRoA services. Upstream's backend is replaced by one
    /// that needs no operator and sends nothing, so every episode must arrive
    /// as `pending`. `base_url` and `api_key` are ignored.
    pub local: bool,
    /// Start takes with no operator logged in on the robot: upstream's
    /// `require_active_operator: false`.
    pub operator_optional: bool,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("base_url", &self.base_url)
            .field("api_key", &crate::redacted(&self.api_key))
            .field("queue_path", &self.queue_path)
            .field("local", &self.local)
            .field("operator_optional", &self.operator_optional)
            .finish()
    }
}

/// The world as it is right now, supplied on every call.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tick {
    pub now: f64,
    pub gate_level: u8,
    /// The episode the rig could record next, when one is waiting. Upstream
    /// reads it whenever no take is open, not only on `Command::Start`.
    pub pending: Option<serde_json::Value>,
}

#[derive(Default, Debug, Clone, Deserialize)]
pub struct Snapshot {
    pub recording: bool,
    pub sampled_at: f64,
    pub step: usize,
    pub subtasks: usize,
    /// The episode the session is on, as upstream holds it; `{}` outside a take.
    #[serde(default)]
    pub episode: serde_json::Value,
    pub display: Display,
}

#[derive(Deserialize)]
pub struct Outcome {
    context: serde_json::Value,
    pub accepted: bool,
    pub message: String,
    #[serde(flatten)]
    pub snapshot: Snapshot,
}

/// One run of the task FSM. The upstream context is opaque and lives in here,
/// so the caller keeps no state of its own.
///
/// After an error, reconcile whatever host I/O already happened before
/// discarding a Session: a failed call is not replayed.
pub struct Session {
    identity: Identity,
    context: serde_json::Value,
    recovery_timeout_sec: f64,
    /// Upstream's planner keeps its deadline in a `_start_time` it sets on the
    /// first `step`. There is no planner object between calls here, so the
    /// value it would have kept is carried here and handed back each tick.
    recovery_started: Option<f64>,
}

/// What upstream's `StartupRecoveryPlanner` decided this tick.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Recovery {
    /// False means the planner is still waiting: tick it again.
    pub done: bool,
    pub action: RecoveryAction,
    /// Upstream's own words for what it is waiting on, or why it stopped.
    pub reason: String,
    /// Upstream calls this `is_recording_assumed`. It is one specific
    /// assumption and not a general one: the wait for `is_recording` timed
    /// out and the planner took `Some(false)` in its place, so a take that
    /// really was running will be cancelled as stale. Warn when it is set.
    pub assumed_not_recording: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// No conclusion yet — nothing to do but tick again.
    Wait,
    /// Nothing to recover: no episode was open, or the context had already
    /// moved past `CONFIRM_TASK`.
    Skip,
    /// A take was in progress and still is: call [`Session::recover_resume`],
    /// then start the recorder.
    Resume,
    /// An episode is open at the backend but nothing was recording: cancel it
    /// there and drop it.
    CancelStale,
}

#[derive(Deserialize)]
struct RecoveryReply {
    #[serde(flatten)]
    recovery: Recovery,
    start_time: Option<f64>,
}

impl Session {
    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            context: serde_json::json!({}),
            recovery_timeout_sec: 10.0,
            recovery_started: None,
        }
    }

    /// How long [`recover_step`](Self::recover_step) waits for the signals it
    /// needs before deciding without them. Default 10 s, upstream's own.
    ///
    /// It sits here rather than on `recover_step` because it is one property
    /// of one recovery, not of a tick: passing it per tick invites a caller
    /// to change it mid-decision, which moves a deadline that has already
    /// started running.
    pub fn with_recovery_timeout(mut self, timeout_sec: f64) -> Self {
        self.recovery_timeout_sec = timeout_sec;
        self
    }

    /// Decide what to do about a take the process died in the middle of.
    /// Call once per tick after a restart until `done`, then act on `action`.
    ///
    /// `is_recording` is `None` while the rig does not yet know whether it was
    /// recording; the planner waits for it up to the recovery timeout. The
    /// episode the backend still shows as in progress is read from the
    /// session context, so put it there (a [`Tick::pending`] on any earlier
    /// call) before recovering.
    pub fn recover_step(
        &mut self,
        now_monotonic: f64,
        is_recording: Option<bool>,
    ) -> Result<Recovery, Error> {
        let value = crate::runtime::call(
            "recover_step",
            serde_json::json!({
                "context": self.context,
                "now_monotonic": now_monotonic,
                "is_recording": is_recording,
                "timeout_sec": self.recovery_timeout_sec,
                "start_time": self.recovery_started,
            }),
        )?;
        let reply: RecoveryReply = serde_json::from_value(value)
            .map_err(|e| Error::Python(format!("recover_step response: {e}")))?;
        self.recovery_started = reply.start_time;
        Ok(reply.recovery)
    }

    /// Rebuild the context around the interrupted episode, leaving it at the
    /// first subtask that has not been resolved. Only after a
    /// [`RecoveryAction::Resume`]; start the recorder once it returns.
    pub fn recover_resume(&mut self, now_sec: f64) -> Result<(), Error> {
        self.context = crate::runtime::call(
            "recover_resume",
            serde_json::json!({"context": self.context, "now_sec": now_sec}),
        )?;
        Ok(())
    }

    /// The binding moved: a new assignment, a new operator, or an empty
    /// `base_url` because the take is unbound. Keeps the episode context.
    pub fn rebind(&mut self, identity: Identity) {
        self.identity = identity;
    }

    /// Ask upstream to advance, performing whatever effects it asks for.
    ///
    /// `Ok(outcome)` with `accepted == false` is upstream declining, which is
    /// ordinary: show `outcome.message`. `Err` means the boundary broke.
    pub fn run(
        &mut self,
        command: Command,
        tick: Tick,
        mut perform: impl FnMut(Effect) -> Report,
    ) -> Result<Outcome, Error> {
        let mut input = serde_json::json!({
            "now": tick.now,
            "gate_level": tick.gate_level,
            "base_url": self.identity.base_url,
            "api_key": self.identity.api_key,
            "context": self.context,
            "offline_queue_path": self.identity.queue_path,
            "local": self.identity.local,
            "operator_optional": self.identity.operator_optional,
        });
        let fields = input.as_object_mut().expect("built as an object");
        if let Some(pending) = tick.pending {
            fields.insert("pending".into(), pending);
        }
        let command = serde_json::to_value(&command).expect("Command serializes");
        fields.extend(command.as_object().expect("Command is a map").clone());

        let value = crate::runtime::call_with_host("task", input, |need| {
            let effect = Effect::parse(need)?;
            let so_far = need["segments"].as_u64().unwrap_or(0) as usize;
            let mut report = perform(effect.clone());
            if let Some(why) = report.abort.take() {
                return Err(Error::Python(why));
            }
            Ok(serde_json::json!({
                "value": effect.answer(&report),
                "facts": report.facts(so_far),
            }))
        })?;
        let outcome: Outcome = serde_json::from_value(value)
            .map_err(|e| Error::Python(format!("task response: {e}")))?;
        self.context = outcome.context.clone();
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn identity() -> Identity {
        crate::start(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build")).unwrap();
        Identity {
            base_url: backend_url().to_string(),
            api_key: "test".into(),
            queue_path: queue_path(),
            local: false,
            operator_optional: false,
        }
    }

    /// One queue file per test (the test thread carries the test's name), on
    /// a read-write mount shared by all of them. Two tests on one file would
    /// contend on SQLite's lock; the directory is never removed, because a
    /// worker spawned by another test opens every mount at once.
    fn queue_path() -> String {
        static FRESH: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
        let base = FRESH.get_or_init(|| {
            let base =
                std::env::temp_dir().join(format!("airoa_sdk_task_queue-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            std::fs::create_dir_all(&base).unwrap();
            base
        });
        crate::add_preopen(base, "/task_queue", crate::Access::ReadWrite);
        let name: String = std::thread::current()
            .name()
            .unwrap_or("main")
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        format!("/task_queue/db/{name}.db")
    }

    fn tick() -> Tick {
        Tick { now: 10.0, gate_level: 0, pending: None }
    }

    fn offering() -> Tick {
        Tick { pending: Some(queued()), ..tick() }
    }

    fn session() -> Session {
        Session::new(identity())
    }

    fn name(effect: &Effect) -> String {
        let text = format!("{effect:?}");
        text.split([' ', '(']).next().unwrap_or_default().to_string()
    }

    fn queued() -> Value {
        json!({"episodeId": "e1", "task": {"name": "collect"},
            "subtasks": [{"id": "s1", "name": "pick"}, {"id": "s2", "name": "place"}]})
    }

    fn rig(effect: Effect) -> Report {
        match effect {
            Effect::StartRosbagRecording { .. } => Report::done().recording(true).segments(0),
            Effect::RecordLabelAndSegment { .. } => Report::done().added_segment(),
            Effect::StopRosbagRecording | Effect::CancelRosbagRecording => {
                Report::done().recording(false)
            }
            _ => Report::done(),
        }
    }

    fn backend_url() -> &'static str {
        static SERVER: std::sync::OnceLock<crate::test_http::Server> = std::sync::OnceLock::new();
        &SERVER.get_or_init(crate::test_http::Server::new).url
    }

    fn start(session: &mut Session) -> Outcome {
        session.run(Command::Start, offering(), rig).unwrap()
    }

    #[test]
    fn upstream_advances_saves_and_resets_for_another_take() {
        let mut session = session();
        let mut calls = Vec::new();
        let mut run = |session: &mut Session, command, at: Tick| {
            session
                .run(command, at, |effect| {
                    let version = crate::runtime::call("version", json!({})).unwrap();
                    assert_eq!(version["platform"], "wasi");
                    let profile: Value = crate::backend::me(backend_url(), "test").unwrap();
                    assert_eq!(profile["robot_id"], "robot");
                    if matches!(effect, Effect::SetEpisode(_)) {
                        // Finalization asks a recorder thread to build metadata.
                        let (send, receive) = std::sync::mpsc::channel();
                        std::thread::spawn(move || {
                            let _ = send.send(crate::runtime::call("version", json!({})));
                        });
                        assert_eq!(
                            receive
                                .recv_timeout(std::time::Duration::from_secs(10))
                                .unwrap()
                                .unwrap()["platform"],
                            "wasi"
                        );
                    }
                    calls.push(name(&effect));
                    rig(effect)
                })
                .unwrap()
        };
        let started = run(&mut session, Command::Start, offering());
        assert!(started.accepted && started.snapshot.recording);
        assert_eq!(started.snapshot.display.status, "executing :pick");
        let advance = || Command::Transit { action: Action::Accept, reason: String::new() };
        let next = run(&mut session, advance(), tick());
        assert_eq!(next.snapshot.step, 1);
        let done = run(&mut session, advance(), tick());
        assert!(done.accepted && !done.snapshot.recording);
        assert_eq!(session.context["completed_episode_ids"], json!(["e1"]));
        assert_eq!(
            calls,
            [
                "StartRosbagRecording",
                "RunRecordRunner",
                "SetEpisodeUuid",
                "LogInfo",
                "RecordLabelAndSegment",
                "RecordLabelAndSegment",
                "SetEpisode",
                "StopRosbagRecording"
            ]
        );
        start(&mut session);
        assert_eq!(session.context["segment_count"], 0);
    }

    #[test]
    fn task_display_comes_from_upstream_formatting() {
        let mut session = session();
        start(&mut session);
        session
            .run(
                Command::Transit { action: Action::Accept, reason: String::new() },
                Tick { now: 15.0, ..tick() },
                rig,
            )
            .unwrap();
        let later = Tick { now: 17.5, ..tick() };
        let display = session.run(Command::View, later, rig).unwrap().snapshot.display;
        assert_eq!(display.status, "executing :place");
        assert_eq!(display.next_task, r#"{"id": "s2", "name": "place"}"#);
        assert_eq!(display.subtask_elapsed_sec, 2.5);
        assert_eq!(display.episode_elapsed_sec, 7.5);
    }

    #[test]
    fn early_failure_and_cancel_are_owned_by_core() {
        for save in [true, false] {
            let mut session = session();
            start(&mut session);
            session.run(Command::StopSubtask { success: false }, tick(), rig).unwrap();
            let result = session
                .run(Command::StopRecording { save, reason: "fault".into() }, tick(), |effect| {
                    if let Effect::SetEpisode(episode) = &effect {
                        assert_eq!(episode["success"], false);
                    }
                    rig(effect)
                })
                .unwrap();
            assert!(result.accepted && !result.snapshot.recording);
        }
    }

    #[test]
    fn gate_and_recorder_refusals_do_not_publish_start() {
        for level in [0, 1] {
            let backend = crate::test_http::Server::new();
            let mut session =
                Session::new(Identity { base_url: backend.url.clone(), ..identity() });
            let at = Tick { gate_level: level, ..tick() };
            let result =
                session
                    .run(Command::Start, Tick { pending: Some(queued()), ..at }, |effect| {
                        match effect {
                            Effect::LogInfo(_) | Effect::LogWarning(_) | Effect::LogError(_) => {
                                Report::done()
                            }
                            Effect::StartRosbagRecording { .. } if level == 0 => {
                                Report::failed("disk full")
                            }
                            Effect::PublishRecordingBlock { .. } if level == 1 => Report::done(),
                            other => panic!("unexpected effect: {other:?}"),
                        }
                    })
                    .unwrap();
            assert!(!result.accepted && !result.snapshot.recording);
            // Upstream reads /robot/me to see who is logged in before it will
            // start. What must not happen is the episode being published.
            let seen = backend.requests.lock().unwrap().clone();
            assert!(
                seen.iter().all(|r| r.contains("/robot/me")),
                "a refused start published something: {seen:?}"
            );
        }
    }

    #[test]
    fn a_failed_local_finalize_cannot_publish_finish() {
        let mut session = session();
        start(&mut session);
        session.run(Command::StopSubtask { success: true }, tick(), rig).unwrap();
        let previous = session.context.clone();
        let backend = crate::test_http::Server::new();
        session.rebind(Identity { base_url: backend.url.clone(), ..identity() });
        let error = session.run(
            Command::StopRecording { save: true, reason: String::new() },
            tick(),
            |effect| {
                assert!(matches!(effect, Effect::SetEpisode(_)));
                Report::abort("flush failed")
            },
        );
        assert!(error.is_err());
        assert_eq!(session.context, previous);
        assert!(
            backend.requests.lock().unwrap().is_empty(),
            "finish must follow durable local completion"
        );
    }

    #[test]
    fn callback_panic_does_not_poison_later_operations() {
        assert!(std::panic::catch_unwind(|| {
            let _ = session().run(Command::Start, offering(), |_| panic!("native failure"));
        })
        .is_err());
        assert!(start(&mut session()).accepted);
    }

    /// A restart with an episode still open: the take is only visible to
    /// recovery through the context, so seed it the way a live rig would.
    fn interrupted(episode: Value) -> Session {
        let mut session = session();
        session.run(Command::View, Tick { pending: Some(episode), ..tick() }, rig).unwrap();
        session
    }

    #[test]
    fn recovery_waits_for_the_recording_signal_until_it_times_out() {
        let mut session = interrupted(queued());
        let waiting = session.recover_step(100.0, None).unwrap();
        assert_eq!(waiting.action, RecoveryAction::Wait);
        assert!(!waiting.done && !waiting.assumed_not_recording);
        assert_eq!(waiting.reason, "is_recording signal");
        // Still inside the 10 s default, measured from the first step: the
        // deadline survives the call only because it is round-tripped.
        assert!(!session.recover_step(105.0, None).unwrap().done);
    }

    #[test]
    fn a_timed_out_recording_signal_is_taken_as_not_recording() {
        let mut session = interrupted(queued()).with_recovery_timeout(2.0);
        assert!(!session.recover_step(100.0, None).unwrap().done);
        let decided = session.recover_step(102.5, None).unwrap();
        assert!(decided.done && decided.assumed_not_recording);
        assert_eq!(decided.action, RecoveryAction::CancelStale);
    }

    #[test]
    fn resuming_leaves_the_context_on_the_first_unresolved_subtask() {
        let mut session = interrupted(json!({"episodeId": "e1", "task": {"name": "collect"},
            "subtasks": [{"id": "s1", "name": "pick", "status": 1},
                         {"id": "s2", "name": "place", "status": 0}]}));
        let decision = session.recover_step(100.0, Some(true)).unwrap();
        assert!(decision.done && !decision.assumed_not_recording);
        assert_eq!(decision.action, RecoveryAction::Resume);
        session.recover_resume(4200.0).unwrap();
        assert_eq!(session.context["tasks"]["episodeId"], "e1");
        assert_eq!(session.context["cur_subtask_index"], 1);
        // WAIT_SUBTASK: one subtask is still pending.
        assert_eq!(session.context["status"], 1);
        assert_eq!(session.context["has_subtasks_successed"], json!([true, null]));
        assert_eq!(session.context["task_start_time"], 4200.0);
    }

    #[test]
    fn an_open_episode_nobody_was_recording_is_stale() {
        let mut session = interrupted(queued());
        let decision = session.recover_step(100.0, Some(false)).unwrap();
        assert!(decision.done && !decision.assumed_not_recording);
        assert_eq!(decision.action, RecoveryAction::CancelStale);
    }

    #[test]
    fn recovery_skips_a_session_that_already_moved_on() {
        let mut session = session();
        start(&mut session);
        let decision = session.recover_step(100.0, None).unwrap();
        assert!(decision.done);
        assert_eq!(decision.action, RecoveryAction::Skip);
        assert_eq!(decision.reason, "state already advanced");
    }

    #[test]
    fn failed_connection_does_not_poison_later_http() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let unavailable = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        identity();
        let profile: Value = crate::backend::me(&unavailable, "test").unwrap();
        assert_eq!(profile["robot_id"], "");
        let profile: Value = crate::backend::me(backend_url(), "test").unwrap();
        assert_eq!(profile["robot_id"], "robot");
    }

    /// The FSM reports subtask completions and executions itself, so without a
    /// queue those are the one class of report an outage still loses while
    /// episode boundaries survive. The rig has to be running before the
    /// outage matters, so the take starts against a backend that answers and
    /// the network goes away underneath it.
    #[test]
    fn an_outage_mid_take_leaves_the_fsm_s_own_reports_in_the_queue() {
        let mut session = session();
        let client = crate::backend::Client::new(UNREACHABLE, "k", &queue_path());
        assert_eq!(client.pending().unwrap(), 0, "an untouched session has queued nothing");
        assert!(session.run(Command::Start, offering(), rig).unwrap().accepted);

        session.rebind(Identity { base_url: UNREACHABLE.into(), ..identity() });
        session.run(Command::StopSubtask { success: true }, tick(), rig).unwrap();

        // And it is the client half's queue, not one of the FSM's own: one
        // rig, one backlog, whichever half wrote it.
        assert!(client.pending().unwrap() > 0, "the FSM's report must survive the outage");
    }

    /// Standalone: a pending episode starts and runs through both subtasks
    /// with no operator and no backend to reach.
    #[test]
    fn a_local_session_records_with_no_backend() {
        let mut session =
            Session::new(Identity { base_url: UNREACHABLE.into(), local: true, ..identity() });
        let start = Tick { pending: Some(queued()), ..tick() };
        let result = session.run(Command::Start, start, rig).unwrap();
        assert!(result.accepted && result.snapshot.recording, "{}", result.message);
        let accept = Command::Transit { action: Action::Accept, reason: String::new() };
        assert_eq!(session.run(accept, tick(), rig).unwrap().snapshot.step, 1);
        let stop = Command::StopRecording { save: true, reason: String::new() };
        session.run(Command::StopSubtask { success: true }, tick(), rig).unwrap();
        assert!(!session.run(stop, tick(), rig).unwrap().snapshot.recording);
    }

    /// `require_operator: false`: a backend with nobody logged in still takes
    /// a start, and the default still refuses it.
    #[test]
    fn a_start_needs_no_operator_only_when_the_rig_says_so() {
        let backend = crate::test_http::Server::answering(
            r#"{"id":"robot","execution_id":"x1","active_operator":null}"#,
        );
        for optional in [false, true] {
            let mut session = Session::new(Identity {
                base_url: backend.url.clone(),
                operator_optional: optional,
                ..identity()
            });
            let result = session.run(Command::Start, offering(), rig).unwrap();
            assert_eq!(result.accepted, optional, "{}", result.message);
        }
    }

    /// Nothing listens on port 1, so every request fails to connect at once.
    const UNREACHABLE: &str = "http://127.0.0.1:1";
}
