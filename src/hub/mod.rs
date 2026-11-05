//! The state every thread shares. Sensors publish into [`Hub`], the writer and
//! the gate read from it, and `view/` and `stream/` display it.
//!
//! `Hub` is one struct, and each submodule implements one concern on it. This
//! file holds the struct, the writer's channel and the lists of what the
//! display may read from `record/`. The submodules reach `record/` and
//! `sensors/` through the imports here.

use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use log::info;

use crate::misc::clock::boottime_ns;
use crate::record::writer::write_episodes;
use crate::record::{click, gate, still, task, video};
use crate::sensors::stats::{Cam, CamInfo, Rate};
#[cfg(test)]
use crate::{record::record::uuid_v4, test_http};

pub use crate::record::gate::{Condition, Fault, GroupVerdict, Reason, CAMERAS, USB_HUB};
pub use crate::sensors::stats::{Ports, Roles, SourceStats};

mod calibration;
mod camera;
mod core_reason;
mod rec;
mod recording;
mod sources;
#[cfg(test)]
mod task_tests;
mod trigger;
mod verdict;

pub use camera::{UiSnapshot, CAM_STRIP, SLOT_HEAD, USB_CAM_SLOTS};
pub use rec::InputChange;
pub(crate) use rec::Rec;
pub(crate) use sources::{GRIPPER_PAIR, SOURCE_NAMES};
pub use sources::{SOURCES, SRC_CAM, SRC_CONTROLLER, SRC_ENCODER, SRC_HMD};
use trigger::Trigger;

/// What `view/` and `stream/` may read about the takes, the exports, the
/// uploads and the backend. They reach these through `hub` rather than into `record/`, so
/// `record/` can be rearranged freely as long as this list still resolves,
/// and the list is the whole surface the display depends on.
pub mod episodes {
    pub use crate::record::mcap::CAM_NAMES;
    pub use crate::record::playback::{index, Index};
    pub use crate::record::record::{
        clean_folder, folder_of, is_complete, list, set_folder, takes, FOLDER, META, SIGNALS,
    };
}
pub mod export {
    pub use crate::record::export::job::{
        dir, files, is_done, list, remove, running, safe_id, start, EXPORTS,
    };
    #[cfg(target_os = "android")]
    pub use crate::record::export::transcode::Transcoder;
    pub use crate::record::export::Video;
    pub use crate::record::export::{subtasks, Leave};
}
pub mod camera_calib {
    pub use crate::record::camera_calib::store::{load, safe_id, status, DIR};
}
pub mod upload {
    #[cfg(test)]
    pub(crate) use crate::record::upload::sending;
    pub use crate::record::upload::{
        depth, save, shown, state, unless_sending, was_sent, Level, State, NO_FILE,
    };
}
pub mod backend {
    pub use crate::record::backend::{episode::unbound, poll::offline_since_ns};
}

pub(crate) const EPISODE_DIR: &str = "/sdcard/Android/data/org.airoa.yubigo/files";

/// Messages the writer may fall behind by before [`Hub::record`] drops and
/// counts them (`dropped`). Chosen by hand.
const RECORD_QUEUE: usize = 512;

/// ROS's diagnostic_aggregator publishes at 1 Hz; nothing here changes faster
/// than the counters a camera keeps, and those go per frame on `uvc_frame`.
const DIAGNOSTICS_PERIOD: Duration = Duration::from_secs(1);

pub use crate::record::gate::SPACE_FLOOR;
pub use crate::record::still::sounding_until;

#[derive(Default)]
struct RecStatus {
    active: bool,
    failed: bool,
    dropped: u64,
    refused: bool,
    refused_why: Vec<Reason>,
    refused_by_core: Option<String>,
    core_block: Option<String>,
}

#[derive(Default)]
struct State {
    cams: [Cam; 3],
    /// By rank, not slot: `roles` decides which slot reads which.
    cam_info: [CamInfo; 3],
    sides: [Option<f32>; 2],
    poses: [Option<[f64; 7]>; 3],
    tracked: [bool; 3],
    valid: [bool; 3],
    untracked_since: [Option<Instant>; 3],
    roles: Roles,
    /// The port each open camera is plugged into, by rank.
    cam_ports: Ports,
    /// The port saved for each slot (`config.json`'s `camera_ports`), which
    /// `roles` follows whenever a camera opens.
    saved_ports: Option<Ports>,
    last_preview: [Option<Instant>; 3],
    rates: [Rate; SOURCES],
    rec: RecStatus,
}

#[derive(Default)]
pub struct Hub {
    state: Mutex<State>,
    writer: OnceLock<SyncSender<Rec>>,
    dir: OnceLock<String>,
    calibration: Mutex<crate::misc::calib::Session>,
    trigger: Mutex<Trigger>,
    armed: AtomicBool,
    fault_spoke: Mutex<Option<Instant>>,
    /// The gate reason behind the last fault, cancel or stop, kept until the
    /// voice says it.
    last_fault: Mutex<Option<Reason>>,
    /// The running subtask has had its 30-seconds-left warning.
    time_warned: AtomicBool,
    /// Calibration button presses, so the voice repeats a refusal on each.
    calibration_presses: std::sync::atomic::AtomicU32,
    /// Sweeps that finished, stored or not, so the voice can tell a finished
    /// sweep from an abandoned one.
    calibrations_finished: std::sync::atomic::AtomicU64,
    /// The cue `poll_fault` last raised, kept until the panel plays it.
    fault_cue: Mutex<Option<Feedback>>,
    task_missing: AtomicBool,
    /// No camera on the headset. False until `Config::head_camera` is applied at launch.
    head_absent: AtomicBool,
    /// Each wrist slot is known to show its own gripper, from the panel's
    /// motion check or a manual assignment. False at launch.
    cameras_matched: AtomicBool,
    keep_faulted_episodes: AtomicBool,
    /// The task's `advance_when_still`.
    advance_when_still: AtomicBool,
    /// The writer holds an open take (`busy`).
    take_open: AtomicBool,
    /// `Config::video` is H.264: frames go to `video` instead of the writer.
    h264: AtomicBool,
    video: video::Encoders,
    verdict: Mutex<Option<gate::Verdict>>,
    /// Held across `apply_verdict`, so verdicts are compared and replaced in
    /// the order they came.
    verdict_order: Mutex<()>,
    task: OnceLock<task::Task>,
    /// The latest status each part reported, by name; `/diagnostics` is a
    /// snapshot of all of them once a second.
    health: Mutex<std::collections::BTreeMap<String, crate::misc::status::Status>>,
    /// Every whole frame per slot, newest only, for the viewer's live view.
    /// Apart from the previews because the headset's panel decodes every
    /// preview it is handed, and behind locks of their own so a viewer
    /// polling them never waits on, or holds up, `state`.
    live: [Mutex<Live>; 3],
}

/// The newest whole frame of a slot, and how many have arrived.
type Live = (Option<Arc<[u8]>>, u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Feedback {
    Started,
    Stopped,
    Advanced,
    MarkedFailed,
    /// B or the subtask limit ended the episode as a failure.
    EndedFailed,
    Refused,
    Fault,
    Cancelled,
    RunningOut,
    TimeWarning,
    /// The cameras are in their slots, by hand or by the motion check.
    Assigned,
}

impl Hub {
    fn record(&self, msg: Rec) {
        let Some(writer) = self.writer.get() else { return };
        if let Err(TrySendError::Full(_)) = writer.try_send(msg) {
            self.state.lock().unwrap().rec.dropped += 1;
        }
    }

    fn control(&self, msg: Rec) {
        if let Some(writer) = self.writer.get() {
            let _ = writer.send(msg);
        }
    }

    /// The writer's answer, or why it could not give one. A plain `String`
    /// rather than the SDK's error: the recorder is ours, and its failures
    /// have nothing to do with the boundary to airoa_core.
    pub(crate) fn record_call(&self, command: Rec) -> Result<serde_json::Value, String> {
        let (reply, result) = std::sync::mpsc::channel();
        let error = |e| format!("recorder: {e}");
        self.writer
            .get()
            .ok_or_else(|| error("not started".to_string()))?
            .send(Rec::Call { command: Box::new(command), reply })
            .map_err(|e| error(e.to_string()))?;
        result.recv().map_err(|e| error(e.to_string()))?.map_err(error)
    }

    /// Replace the part's last status; it goes out with the next snapshot.
    pub fn set_status(&self, status: crate::misc::status::Status) {
        self.health.lock().unwrap().insert(status.name.clone(), status);
    }

    /// Something that happened at one instant: a session change, a recentre.
    pub fn publish_event(&self, status: crate::misc::status::Status) {
        if self.recording() {
            self.record(Rec::Status { at_ns: boottime_ns(), event: true, statuses: vec![status] });
        }
    }

    pub(crate) fn dir(&self) -> String {
        self.dir.get().cloned().unwrap_or_else(|| EPISODE_DIR.to_string())
    }

    /// Said from the battery watcher, which runs outside the render loop.
    #[cfg(target_os = "android")]
    pub fn say_supply(&self, supply: crate::misc::power::Supply) {
        crate::view::say_supply(supply);
    }

    pub fn start_writer(self: &Arc<Self>) {
        self.start_writer_in(EPISODE_DIR.to_string());
    }

    fn start_writer_in(self: &Arc<Self>, dir: String) {
        let (tx, rx) = sync_channel(RECORD_QUEUE);
        if self.writer.set(tx).is_err() {
            return;
        }
        let _ = self.dir.set(dir.clone());
        crate::record::record::sweep_incomplete(&dir);
        // The local half of the same interruption: the take is gone above, and
        // the backend still thinks it is running.
        crate::record::backend::episode::close_stale(&dir);
        let session = crate::misc::calib::Session::load(&dir);
        info!("calibration: {}", calibration::describe(session.active()));
        *self.calibration.lock().unwrap() = session;
        let hub = self.clone();
        std::thread::spawn(move || write_episodes(&hub, rx, &dir));
        let hub = self.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(DIAGNOSTICS_PERIOD);
            if hub.recording() {
                let statuses = hub.health.lock().unwrap().values().cloned().collect();
                hub.record(Rec::Status { at_ns: boottime_ns(), event: false, statuses });
            }
        });
        let _ = self.task.set(task::Task::start(self.clone(), self.dir()));
        gate::spawn_tick(self.clone());
    }
}
