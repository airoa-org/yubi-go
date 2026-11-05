//! The hub driven end to end: a real writer and task worker on a temporary
//! folder, with airoa_core loaded and a fake backend where a take needs one.

use super::*;
use crate::misc::capture::Capture;
use crate::misc::config::{Assignment, Config, Subtask};
use crate::misc::pose::Located;
use click::Gesture;
use serde_json::{json, Value};
use std::path::PathBuf;
use task::Event;

struct Rig {
    hub: Arc<Hub>,
    dir: PathBuf,
}

impl Rig {
    fn new(tasks: bool, writable: bool) -> Self {
        airoa_sdk::start(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("airoa-sdk/build"))
            .unwrap();
        // The FSM opens its report queue under the guest files root.
        test_http::files_root();
        let dir = std::env::temp_dir().join(format!("yubigo-fsm-{}", uuid_v4().unwrap()));
        if writable {
            std::fs::create_dir(&dir).unwrap();
        } else {
            std::fs::write(&dir, b"not a directory").unwrap();
        }
        if tasks {
            // Every take asks the backend for its operator, so serve one
            // with an operator logged in.
            let (base, _) =
                test_http::serve(r#"{"active_operator":{"user_id":"u","display_name":"op"}}"#);
            test_http::point_at(dir.to_str().unwrap(), &base);
            Assignment::save_all(
                dir.to_str().unwrap(),
                &[Assignment {
                    id: uuid_v4().unwrap(),
                    task: "collect".into(),
                    subtasks: ["pick", "place"]
                        .into_iter()
                        .enumerate()
                        .map(|(i, name)| Subtask {
                            id: format!("s{i}"),
                            name: name.into(),
                            status: 0,
                        })
                        .collect(),
                    ..Default::default()
                }],
            )
            .unwrap();
        }
        let hub = Arc::new(Hub::default());
        let (tx, rx) = sync_channel(RECORD_QUEUE);
        assert!(hub.writer.set(tx).is_ok());
        assert!(hub.dir.set(dir.to_str().unwrap().into()).is_ok());
        *hub.verdict.lock().unwrap() = Some(gate::Verdict { gate_level: 0, groups: vec![] });
        let writer = hub.clone();
        let base = dir.clone();
        std::thread::spawn(move || write_episodes(&writer, rx, base.to_str().unwrap()));
        assert!(hub.task.set(task::Task::start(hub.clone(), dir.to_str().unwrap().into())).is_ok());
        Self { hub, dir }
    }

    fn send(&self, event: Event) -> Feedback {
        assert!(self.hub.send_task(event));
        let task = self.hub.task.get().unwrap();
        let deadline = Instant::now() + Duration::from_secs(45);
        loop {
            if let Some(feedback) = task.feedback() {
                return feedback;
            }
            assert!(Instant::now() < deadline, "task worker timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn sample(&self, at_ns: u64) {
        self.hub
            .record_call(Rec::Pose {
                at_ns,
                capture: Capture::default(),
                part: 2,
                located: Located::tracked([0., 0., 1., 0., 0., 0., 1.]),
            })
            .unwrap();
    }

    fn latest(&self) -> PathBuf {
        let base = self.dir.to_str().unwrap();
        std::path::Path::new(&episodes::takes(base)).join(episodes::list(base).first().unwrap())
    }

    fn meta(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.latest().join("meta.json")).unwrap()).unwrap()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.hub.stop_and_wait();
        if self.dir.is_dir() {
            let _ = std::fs::remove_dir_all(&self.dir);
        } else {
            let _ = std::fs::remove_file(&self.dir);
        }
    }
}

fn gesture() -> Gesture {
    let now = boottime_ns();
    Gesture {
        side: 0,
        started_ns: now,
        ended_ns: now,
        task_end_ns: now,
        task_start_ns: now + 1_000_000,
        held: false,
    }
}

#[test]
fn a_refusal_from_core_is_shown_even_while_the_gate_is_open() {
    let rig = Rig::new(true, true);
    rig.hub.core_blocked("Cannot start recording: no active operator logged in on the robot");
    rig.hub.task_refused(true);
    assert_eq!(rig.hub.gate_level(), gate::OK, "the gate has nothing against it");
    assert_eq!(
        rig.hub.refusal(),
        ["No operator logged in", "Open this robot's dashboard teleop screen"]
    );

    rig.hub.task_refused(false);
    assert!(rig.hub.refusal().is_empty());
    rig.hub.task_refused(true);
    assert!(rig.hub.refusal().is_empty(), "a block answers one gesture, not the next");
}

#[test]
fn a_gate_refusal_stays_visible_across_background_refreshes() {
    let rig = Rig::new(false, true);
    rig.hub.verdict.lock().unwrap().as_mut().unwrap().gate_level = gate::HARD_STOP;
    rig.hub.trigger.lock().unwrap().pending = Some(gesture());
    assert_eq!(rig.hub.poll_trigger(), Some(Feedback::Refused));
    assert!(!rig.hub.refusal().is_empty());

    let sampled = rig.hub.task_view().core.sampled_at;
    let deadline = Instant::now() + Duration::from_secs(5);
    while rig.hub.task_view().core.sampled_at <= sampled {
        assert!(Instant::now() < deadline, "background View did not refresh");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!rig.hub.refusal().is_empty(), "View must not dismiss the refusal");
    rig.hub.verdict.lock().unwrap().as_mut().unwrap().gate_level = gate::OK;
    assert!(rig.hub.refusal().is_empty(), "gate recovery clears the displayed reasons");
}

#[test]
fn core_advances_native_segments_and_refuses_an_unassigned_repeat() {
    let rig = Rig::new(true, true);
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Started);
    assert_eq!(rig.hub.task_view().step_line().as_deref(), Some("1/2 pick"));
    assert_eq!(rig.hub.task_view().config.task(), "collect");
    rig.sample(boottime_ns());
    let assigned = rig.hub.task_view().config.assignment.unwrap();
    Assignment::save_all(
        rig.dir.to_str().unwrap(),
        &[Assignment { task: "next assignment".into(), ..assigned.clone() }],
    )
    .unwrap();
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Advanced);
    assert_eq!(rig.hub.task_view().step_line().as_deref(), Some("2/2 place"));
    assert_eq!(rig.hub.task_view().config.task(), "collect", "active task stays pinned");
    Assignment::save_all(rig.dir.to_str().unwrap(), &[assigned]).unwrap();
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Stopped);
    assert!(!rig.hub.recording());
    assert!(rig.latest().join("meta.json").exists());
    let meta = rig.meta();
    assert_eq!(meta["labels"], json!(["pick", "place"]));
    assert_eq!(meta["segments"].as_array().unwrap().len(), 2);
    assert_eq!(meta["episode"]["success"], true);
    let config = Config::load(rig.dir.to_str().unwrap());
    assert!(config.assignment.is_none());
    assert_eq!(config.task(), "collect");
    // Nothing waits and the backend cannot be reached, so upstream's
    // selector has nothing to repeat from: the take is refused rather than
    // recorded as a one-step episode of YUBI Go's own making. (With no
    // config.json at all the rig would be standalone and record.)
    test_http::point_at(rig.dir.to_str().unwrap(), "http://127.0.0.1:1");
    assert_eq!(rig.send(Event::Start), Feedback::Refused);
    assert!(!rig.hub.recording());
    assert_eq!(episodes::list(rig.dir.to_str().unwrap()).len(), 1);
}

/// Stopping clears `recording` for the panel at once; the take is still
/// open until the writer closes it, and `busy` says so.
#[test]
fn a_stopped_take_is_busy_until_it_has_closed() {
    let rig = Rig::new(false, true);
    assert!(!rig.hub.busy());
    rig.hub.record_call(Rec::Start { gesture: None, config: Config::default() }).unwrap();
    assert!(rig.hub.recording() && rig.hub.busy());
    rig.sample(boottime_ns());
    rig.hub.control(Rec::Pause);
    rig.sample(boottime_ns());
    assert!(!rig.hub.recording(), "the panel shows the stop");
    assert!(rig.hub.busy(), "but the take is open");
    rig.hub.record_call(Rec::Stop { success: true, gesture: None }).unwrap();
    assert!(!rig.hub.busy());
    assert!(rig.latest().join("meta.json").exists());
}

#[test]
fn bad_capture_is_cancelled_and_the_next_take_can_start() {
    let rig = Rig::new(true, true);
    assert_eq!(rig.send(Event::Start), Feedback::Started);
    let at = boottime_ns();
    rig.sample(at);
    rig.sample(at - 1);
    assert_eq!(rig.send(Event::Stop(true)), Feedback::Cancelled);
    assert!(!rig.hub.recording());
    assert!(!rig.hub.task.get().unwrap().snapshot().core.recording);
    let base = rig.dir.to_str().unwrap();
    assert!(episodes::list(base).is_empty(), "the bad take deleted itself");
    assert_eq!(rig.send(Event::Start), Feedback::Started);
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Cancel), Feedback::Cancelled);
    assert!(episodes::list(base).is_empty(), "a cancelled take leaves nothing");
}

/// **Standalone, end to end through airoa_core.** No backend, no queue: the
/// task comes from `config.json`'s list, the gate is not refused for want
/// of one, and the take is labelled with it.
#[test]
fn a_standalone_rig_records_its_own_task_with_no_backend() {
    let rig = Rig::new(false, true);
    std::fs::write(
        rig.dir.join("config.json"),
        r#"{"tasks": ["wipe the table"], "selected_task": "wipe the table"}"#,
    )
    .unwrap();
    assert_eq!(rig.send(Event::Start), Feedback::Started);
    assert!(rig.hub.task_view().config.has_task());
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Stop(true)), Feedback::Stopped);
    assert_eq!(rig.meta()["episode"]["label"], "wipe the table");
    assert_eq!(rig.meta()["episode"]["success"], true);
}

#[test]
fn a_standalone_take_walks_its_own_subtasks() {
    let rig = Rig::new(false, true);
    std::fs::write(
        rig.dir.join("config.json"),
        r#"{"tasks": [{"name": "fold", "subtasks": ["spread", "fold in half"]}]}"#,
    )
    .unwrap();
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Started);
    assert_eq!(rig.hub.task_view().step_line().as_deref(), Some("1/2 spread"));
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Advanced);
    assert_eq!(rig.hub.task_view().step_line().as_deref(), Some("2/2 fold in half"));
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Stopped);
    let meta = rig.meta();
    assert_eq!(meta["labels"], json!(["spread", "fold in half"]));
    assert_eq!(meta["segments"].as_array().unwrap().len(), 2);
    assert_eq!(meta["episode"]["success"], true);
}

#[test]
fn a_native_start_failure_leaves_core_idle() {
    use std::os::unix::fs::PermissionsExt;
    let rig = Rig::new(true, true);
    // The assignment is in place; what fails is opening the episode.
    std::fs::set_permissions(&rig.dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(rig.send(Event::Start), Feedback::Refused);
    std::fs::set_permissions(&rig.dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!rig.hub.recording());
    assert!(!rig.hub.task.get().unwrap().snapshot().core.recording);
}

fn fold_rig() -> Rig {
    let rig = Rig::new(false, true);
    std::fs::write(
        rig.dir.join("config.json"),
        r#"{"tasks": [{"name": "fold", "subtasks": ["spread", "fold in half"]}]}"#,
    )
    .unwrap();
    rig
}

fn outcomes(meta: &Value) -> Vec<(Value, Value)> {
    let segments = meta["segments"].as_array().unwrap();
    segments.iter().map(|s| (s["label_idx"].clone(), s["success"].clone())).collect()
}

#[test]
fn b_fails_the_step_and_a_double_click_tries_it_again() {
    let rig = fold_rig();
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Started);
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Fail(boottime_ns())), Feedback::MarkedFailed);
    assert!(rig.hub.recording(), "B keeps the take open");
    let view = rig.hub.task_view();
    assert!(view.paused);
    assert_eq!(view.step_line().as_deref(), Some("1/2 spread · double-click to retry"));
    assert_eq!(rig.hub.task_display().subtask_elapsed_sec, 0.0, "the reset is off the clock");

    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Started);
    assert!(!rig.hub.task_view().paused);
    assert_eq!(rig.hub.task_view().step_line().as_deref(), Some("1/2 spread"));
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Advanced);
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Stopped);

    let meta = rig.meta();
    assert_eq!(
        outcomes(&meta),
        [(json!(0), json!(false)), (json!(0), json!(true)), (json!(1), json!(true))]
    );
    assert_eq!(meta["episode"]["success"], true, "every step ended in a success");
}

#[test]
fn the_last_step_waits_for_its_retry_instead_of_saving() {
    let rig = fold_rig();
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Started);
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Advanced);
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Fail(boottime_ns())), Feedback::MarkedFailed);
    assert!(rig.hub.recording());
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Started);
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Stopped);

    let meta = rig.meta();
    assert_eq!(
        outcomes(&meta),
        [(json!(0), json!(true)), (json!(1), json!(false)), (json!(1), json!(true))]
    );
    assert_eq!(meta["episode"]["success"], true);
}

#[test]
fn b_while_waiting_ends_the_take_and_keeps_what_was_done() {
    let rig = fold_rig();
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Started);
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Advanced);
    rig.sample(boottime_ns());
    assert_eq!(rig.send(Event::Fail(boottime_ns())), Feedback::MarkedFailed);
    assert_eq!(rig.send(Event::Fail(boottime_ns())), Feedback::EndedFailed, "not A's cue");
    assert!(!rig.hub.recording());
    assert!(!rig.hub.task_view().paused);

    let meta = rig.meta();
    assert_eq!(outcomes(&meta), [(json!(0), json!(true)), (json!(1), json!(false))]);
    assert_eq!(meta["episode"]["success"], false);
}

#[test]
fn b_ends_a_one_step_task_as_failed() {
    for config in [
        r#"{"tasks": ["wipe the table"]}"#,
        r#"{"tasks": [{"name": "wipe", "subtasks": ["wipe the table"]}]}"#,
    ] {
        let rig = Rig::new(false, true);
        std::fs::write(rig.dir.join("config.json"), config).unwrap();
        assert_eq!(rig.send(Event::Accept(gesture())), Feedback::Started);
        rig.sample(boottime_ns());
        assert_eq!(rig.send(Event::Fail(boottime_ns())), Feedback::EndedFailed, "{config}");
        assert!(!rig.hub.recording(), "{config}");
        assert!(!rig.hub.task_view().paused, "{config}");

        let meta = rig.meta();
        assert_eq!(meta["labels"], json!(["wipe the table"]));
        assert_eq!(outcomes(&meta), [(json!(0), json!(false))], "{config}");
        assert_eq!(meta["episode"]["success"], false, "{config}");
    }
}
