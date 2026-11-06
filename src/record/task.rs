use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use airoa_sdk::task::{Action, Command, Effect, Identity, Report, Session, Snapshot, Tick};
// The task half of the boundary lives here; `Display` is what the panel and
// `/state` render, and they take it from this module rather than the SDK.
pub use airoa_sdk::task::Display;
use airoa_sdk::Error;
use serde_json::{json, Value};

use crate::hub::{Feedback, Hub, Rec};
use crate::misc::config::{Assignment, Config};
use crate::record::click::Gesture;

pub(crate) enum Event {
    Start,
    Accept(Gesture),
    /// B during a take, at its boottime. A running step fails and the take
    /// waits on it; a waiting take ends.
    Fail(u64),
    Stop(bool),
    Cancel,
    Shutdown(Sender<()>),
}

pub(crate) struct Task {
    tx: Sender<Event>,
    view: Arc<Mutex<View>>,
    feedback: Mutex<Receiver<Feedback>>,
}

#[derive(Clone, Default)]
pub(crate) struct View {
    pub core: Snapshot,
    pub config: Config,
    /// A step failed, and the take waits for a double-click to try it again.
    pub paused: bool,
}

impl View {
    /// What the hands are on now, as a take says it: the subtask, or the task
    /// when it has none.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn step_text(&self) -> Option<String> {
        let subtasks = self.config.subtask_names();
        match subtasks.is_empty() {
            true => self.config.has_task().then(|| self.config.task().to_string()),
            false => subtasks.get(self.core.step).cloned(),
        }
    }

    pub fn step_line(&self) -> Option<String> {
        let names = self.config.subtask_names();
        let at = match &self.config.assignment {
            _ if self.core.recording => self.core.step,
            Some(assignment) => assignment.current_subtask().map_or(0, |(at, _)| at),
            None => 0,
        };
        let name = names.get(at)?;
        let line = match names.len() {
            1 => name.clone(),
            n => format!("{}/{n} {name}", at + 1),
        };
        Some(match self.paused {
            true => format!("{line} · double-click to retry"),
            false => line,
        })
    }
}

impl Task {
    pub fn start(hub: Arc<Hub>, dir: String) -> Self {
        let (tx, rx) = channel();
        let config = Config::load(&dir);
        hub.set_has_task(config.has_task());
        hub.set_head_camera(config.head_camera);
        hub.set_video(config.video);
        hub.set_keep_faulted_episodes(config.keep_faulted_episodes);
        hub.set_advance_when_still(config.advances_when_still());
        let view = Arc::new(Mutex::new(View { config, ..View::default() }));
        let shared = view.clone();
        let (feedback, outcomes) = channel();
        std::thread::spawn(move || run(hub, dir, shared, rx, feedback));
        Self { tx, view, feedback: Mutex::new(outcomes) }
    }

    pub fn send(&self, event: Event) -> bool {
        self.tx.send(event).is_ok()
    }

    pub fn stop_and_wait(&self) {
        let (tx, rx) = channel();
        if self.send(Event::Shutdown(tx)) {
            let _ = rx.recv();
        }
    }

    pub fn snapshot(&self) -> View {
        self.view.lock().unwrap().clone()
    }
    pub fn feedback(&self) -> Option<Feedback> {
        self.feedback.lock().unwrap().try_recv().ok()
    }
}

// With nothing waiting here, `{}` clears a stale pick from the context and
// upstream's selector fetches the next episode itself — repeating the last
// one through the backend when the queue is empty. A standalone rig has no
// backend to ask, so its take walks its own task's subtasks, or one step
// named after the task when it declares none.
fn pending(config: &Config) -> Result<Value, Error> {
    Ok(match &config.assignment {
        Some(a) => {
            json!({"episodeId": a.id, "task": {"name": config.task()}, "subtasks": a.subtasks})
        }
        None if config.standalone => {
            let mut names = config.subtask_names();
            if names.is_empty() {
                names.push(config.task().to_string());
            }
            json!({
                "episodeId": crate::record::record::uuid_v4().map_err(|e| Error::Python(e.to_string()))?,
                "task": {"name": config.task()},
                "subtasks": names.iter().enumerate()
                    .map(|(i, name)| json!({"id": format!("local-step-{i}"), "name": name}))
                    .collect::<Vec<_>>(),
            })
        }
        None => json!({}),
    })
}

/// The episode upstream started, as this rig's assignment. Never on a
/// standalone rig, because a bound take is reported to the backend.
fn picked(config: &Config, episode: &Value) -> Option<Assignment> {
    assignment_from(episode).filter(|_| !config.standalone)
}

/// The episode upstream reports itself on, as this rig's assignment.
fn assignment_from(episode: &Value) -> Option<Assignment> {
    serde_json::from_value::<Assignment>(episode.clone()).ok().filter(|a| !a.id.is_empty())
}

fn invoke(
    session: &mut Session,
    command: Command,
    hub: &Hub,
    config: &Config,
    gesture: Option<Gesture>,
    pending: Option<Value>,
) -> Result<Snapshot, Error> {
    let tick = Tick {
        now: crate::misc::clock::boottime_ns() as f64 / 1e9,
        gate_level: hub.gate_level(),
        pending,
    };
    // The FSM reports subtask completions and executions itself, and shares
    // the offline queue with the episode boundaries `report` sends, so the
    // two take turns on it.
    let result = crate::record::backend::queue::with_queue(|| {
        session.run(command, tick, |effect| perform(hub, config, gesture, effect))
    })?;
    if !result.accepted {
        return Err(Error::Refused(result.message));
    }
    Ok(result.snapshot)
}

/// The one place YUBI Go acts on upstream's behalf. A `Report::failed` here is
/// upstream being told "could not", which it handles; an outright refusal is
/// reserved for an effect this rig has no input for.
fn perform(hub: &Hub, config: &Config, gesture: Option<Gesture>, effect: Effect) -> Report {
    match effect {
        Effect::StartRosbagRecording { episode } => {
            // The take is recorded against the episode upstream chose, which
            // may be one it fetched itself rather than the one cached here.
            let config = Config {
                assignment: picked(config, &episode).or_else(|| config.assignment.clone()),
                ..config.clone()
            };
            let started =
                hub.ready().map_err(|_| "recording gate closed".to_string()).and_then(|_| {
                    hub.record_call(Rec::Start { gesture, config }).map_err(|e| e.to_string())
                });
            match started {
                Ok(_) => Report::done().recording(true).segments(0),
                Err(why) => Report::failed(why),
            }
        }
        Effect::SetEpisodeUuid(uuid) => match hub.record_call(Rec::Uuid(uuid)) {
            Ok(_) => Report::done(),
            Err(e) => Report::abort(e.to_string()),
        },
        Effect::RecordLabelAndSegment { .. } => Report::done().added_segment(),
        Effect::SetEpisode(episode) => {
            // Finalize locally before core publishes finish_episode. A failed
            // flush must never be reported to the backend as a finished take.
            let Some(success) = episode["success"].as_bool() else {
                return Report::abort("episode carried no outcome");
            };
            match hub.record_call(Rec::Stop { success, gesture }) {
                Ok(serde_json::Value::Bool(true)) => Report::done(),
                Ok(_) => Report::abort("episode failed its close checks"),
                Err(e) => Report::abort(e.to_string()),
            }
        }
        Effect::StopRosbagRecording => Report::done().recording(false),
        Effect::CancelRosbagRecording => match hub.record_call(Rec::Cancel) {
            Ok(_) => Report::done().recording(false),
            Err(e) => Report::abort(e.to_string()),
        },
        // The native writer already receives samples.
        Effect::RunRecordRunner => Report::done(),
        Effect::PublishRecordingBlock { reason } => {
            log::info!("core blocked the take: {reason}");
            hub.core_blocked(&reason);
            Report::done()
        }
        // Only from the rewind that puts a failed last step back to waiting
        // (see `run`). The recorder closes that try as failed itself.
        Effect::OverrideLastSegmentSuccess { success: false } => Report::done(),
        Effect::OverrideLastSegmentSuccess { .. } | Effect::RemoveLastSegment => {
            Report::abort("rewind is not a YUBI Go input")
        }
        Effect::LogInfo(msg) => {
            log::info!("core: {msg}");
            Report::done()
        }
        Effect::LogWarning(msg) => {
            log::warn!("core: {msg}");
            Report::done()
        }
        Effect::LogError(msg) => {
            log::error!("core: {msg}");
            Report::done()
        }
    }
}

fn run(
    hub: Arc<Hub>,
    dir: String,
    shared: Arc<Mutex<View>>,
    rx: Receiver<Event>,
    feedback: Sender<Feedback>,
) {
    let mut session = Session::new(Identity::default());
    let mut config = shared.lock().unwrap().config.clone();
    loop {
        let event = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(event) => Some(event),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let (before, paused) = {
            let view = shared.lock().unwrap();
            (view.core.clone(), view.paused)
        };
        let result = (|| -> Result<Option<Feedback>, Error> {
            if !crate::misc::core::up() {
                return Ok(None);
            }
            let mut queued = None;
            if !before.recording {
                config = Config::load(&dir);
                hub.set_has_task(config.has_task());
                hub.set_head_camera(config.head_camera);
                hub.set_video(config.video);
                hub.set_keep_faulted_episodes(config.keep_faulted_episodes);
                hub.set_advance_when_still(config.advances_when_still());
                queued = Some(pending(&config)?);
                session.rebind(Identity {
                    base_url: config.backend.base_url.clone(),
                    api_key: config.backend.api_key.clone(),
                    queue_path: crate::misc::core::queue_guest_path(),
                    local: config.standalone,
                    operator_optional: !config.require_operator,
                });
            }
            let gesture = match &event {
                Some(Event::Accept(g)) => Some(*g),
                _ => None,
            };
            let mut call = |command, gesture, pending| {
                invoke(&mut session, command, &hub, &config, gesture, pending)
            };
            let command = match &event {
                None => Command::View,
                Some(Event::Start) if !before.recording => Command::Start,
                // While paused, upstream waits on the failed step, so an
                // accept starts it again.
                Some(Event::Accept(_)) => {
                    Command::Transit { action: Action::Accept, reason: String::new() }
                }
                // A retry wait on a one-step task only repeats the episode,
                // so B ends it as a failure.
                Some(Event::Fail(_)) if before.recording && !paused && before.subtasks == 1 => {
                    call(Command::StopSubtask { success: false }, None, None)?;
                    Command::StopRecording { save: true, reason: String::new() }
                }
                Some(Event::Fail(_)) if before.recording && !paused => {
                    // Upstream's own reject starts the retry at once, which
                    // would put the reset in the data and on the 120 s clock.
                    // A failed stop waits instead. On the last step every step
                    // has an outcome, so upstream moves to COMPLETE_TASK, where
                    // the next accept would save; rewind → reject puts it back.
                    if before.step + 1 < before.subtasks {
                        Command::StopSubtask { success: false }
                    } else {
                        call(Command::StopSubtask { success: false }, None, None)?;
                        call(
                            Command::Transit { action: Action::Rewind, reason: String::new() },
                            None,
                            None,
                        )?;
                        Command::Transit { action: Action::Reject, reason: String::new() }
                    }
                }
                Some(Event::Fail(_)) | Some(Event::Stop(_)) | Some(Event::Shutdown(_))
                    if before.recording && paused =>
                {
                    Command::StopRecording { save: true, reason: String::new() }
                }
                Some(Event::Stop(_)) | Some(Event::Shutdown(_)) if before.recording => {
                    let success = match &event {
                        Some(Event::Stop(success)) => *success,
                        _ => true,
                    };
                    call(Command::StopSubtask { success }, None, None)?;
                    Command::StopRecording { save: true, reason: String::new() }
                }
                Some(Event::Cancel) if before.recording => Command::Transit {
                    action: Action::Cancel,
                    reason: "capture interrupted".into(),
                },
                _ => return Ok(None),
            };
            let view = call(command, gesture, queued)?;
            if !before.recording && view.recording {
                if let Some(picked) = picked(&config, &view.episode) {
                    config.assignment = Some(picked);
                }
            }
            if before.recording && !view.recording && config.operator_picks_task {
                if let Some(a) = &config.assignment {
                    crate::record::backend::episode::take_ended(&a.task);
                }
            }
            if let Some(gesture) = gesture.filter(|_| before.recording && view.recording) {
                if paused {
                    hub.record_call(Rec::Resume { gesture }).map_err(Error::Python)?;
                } else if view.step != before.step {
                    hub.record_call(Rec::Advance { gesture }).map_err(Error::Python)?;
                }
            }
            let pausing = !paused && view.recording;
            if let Some(Event::Fail(at_ns)) = event.as_ref().filter(|_| pausing) {
                hub.record_call(Rec::Fail { at_ns: *at_ns }).map_err(Error::Python)?;
            }
            let paused_now = view.recording
                && match &event {
                    Some(Event::Fail(_)) => pausing,
                    Some(Event::Accept(_)) => false,
                    _ => paused,
                };
            let feedback =
                event.as_ref().filter(|_| !view.recording || hub.recording()).map(|event| {
                    match event {
                        Event::Cancel => Feedback::Cancelled,
                        Event::Stop(false) => Feedback::EndedFailed,
                        // `before`: B on a one-step task, or while waiting for a
                        // retry, has already ended the episode.
                        Event::Fail(_) if before.recording && !view.recording => {
                            Feedback::EndedFailed
                        }
                        Event::Fail(_) if before.recording => Feedback::MarkedFailed,
                        Event::Accept(_) if paused && view.recording => Feedback::Started,
                        _ if !before.recording && view.recording => Feedback::Started,
                        _ if view.recording => Feedback::Advanced,
                        _ => Feedback::Stopped,
                    }
                });
            *shared.lock().unwrap() =
                View { core: view, config: config.clone(), paused: paused_now };
            // A background View refresh must not erase a refused gesture
            // before the UI can show it. Gate recovery hides the live reasons.
            if event.is_some() {
                hub.task_refused(false);
            }
            Ok(feedback)
        })();
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(Error::Refused(reason)) => {
                hub.task_refused(true);
                let shown = hub.refusal().join(", ");
                log::warn!("task refused: {}", if reason.is_empty() { shown } else { reason });
                Some(Feedback::Refused)
            }
            Err(e) => {
                log::error!("task: {e}");
                let _ = hub.record_call(Rec::Cancel);
                if let Some(a) = &config.assignment {
                    crate::record::backend::episode::report(
                        crate::record::backend::episode::Boundary::Cancelled,
                        a.id.clone(),
                    );
                }
                session = Session::new(Identity::default());
                *shared.lock().unwrap() = View { config: config.clone(), ..View::default() };
                Some(Feedback::Cancelled)
            }
        };
        if let Some(outcome) = outcome {
            let _ = feedback.send(outcome);
        }
        if let Some(Event::Shutdown(reply)) = event {
            let _ = reply.send(());
            return;
        }
    }
}
