//! `config.json`: what this rig is, where it reports, and what it records.
//!
//! This file reads the document into a [`Config`] and writes it back;
//! `edit` checks the `:8080` page's changes to it; `dashboard` holds the
//! backend's caches (`robot_me.json`, `assigned.json`) that a production
//! rig overlays on it.

mod dashboard;
mod edit;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::atomic::Ordering;

use serde::{Deserialize, Deserializer, Serialize};

use crate::hub::{Ports, Roles};

pub use dashboard::{Assignment, Profile, Subtask};
pub(crate) use edit::origin;
use edit::{EDITABLE, RENAMED};

/// `camera_ports`' keys, indexed by slot.
pub const CAMERA_KEYS: [&str; 3] = ["left_wrist", "right_wrist", "head"];

const FILE: &str = "config.json";

/// The page and the headset's camera picker each read, merge and write the
/// whole file, so two of them interleaved would drop one edit.
static WRITER: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A `null` reads as the field's default rather than failing the whole
/// document.
fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// A name as `config.json` holds it: a string, or `{"en": …}` from a
/// pre-release build that kept translations beside it, which reads as its
/// English.
#[derive(Deserialize)]
#[serde(untagged)]
enum Name {
    English(String),
    Translated(std::collections::BTreeMap<String, String>),
}

impl From<Name> for String {
    fn from(name: Name) -> String {
        match name {
            Name::English(en) => en,
            Name::Translated(mut all) => all.remove("en").unwrap_or_default(),
        }
    }
}

/// A standalone task: its name and the steps a take walks through. A bare
/// string in `config.json` is a task of one step named after itself.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct LocalTask {
    pub name: String,
    pub subtasks: Vec<String>,
    /// Both hands held still move to the next step, as a double-click does.
    /// Off by default, because a pause to think also ends the step.
    pub advance_when_still: bool,
}

impl<'de> Deserialize<'de> for LocalTask {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<LocalTask, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Declared {
            Name(String),
            Full {
                #[serde(default)]
                name: Option<Name>,
                #[serde(default, deserialize_with = "lenient")]
                subtasks: Vec<Name>,
                #[serde(default, deserialize_with = "lenient")]
                advance_when_still: bool,
            },
        }
        Ok(match Declared::deserialize(deserializer)? {
            Declared::Name(name) => LocalTask { name, ..LocalTask::default() },
            Declared::Full { name, subtasks, advance_when_still } => LocalTask {
                name: name.map(String::from).unwrap_or_default(),
                subtasks: subtasks.into_iter().map(String::from).collect(),
                advance_when_still,
            },
        })
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(default)]
pub struct Identity {
    #[serde(deserialize_with = "lenient")]
    pub organization: String,
    #[serde(deserialize_with = "lenient")]
    pub site: String,
    #[serde(deserialize_with = "lenient")]
    pub location: String,
    #[serde(deserialize_with = "lenient")]
    pub robot_type: String,
    #[serde(deserialize_with = "lenient")]
    pub robot_id: String,
    #[serde(deserialize_with = "lenient")]
    pub operator: String,
}

impl Default for Identity {
    fn default() -> Identity {
        Identity {
            organization: String::new(),
            site: String::new(),
            location: String::new(),
            robot_type: "YubiGo".into(),
            robot_id: "yubi-go".into(),
            operator: String::new(),
        }
    }
}

#[derive(Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct Backend {
    #[serde(deserialize_with = "lenient")]
    pub base_url: String,
    #[serde(deserialize_with = "lenient")]
    pub api_key: String,
}

impl std::fmt::Debug for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Backend")
            .field("base_url", &self.base_url)
            .field("api_key", &redacted(&self.api_key))
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

#[derive(Default, Deserialize)]
#[serde(default)]
struct Raw {
    #[serde(deserialize_with = "lenient")]
    identity: Identity,
    #[serde(deserialize_with = "lenient")]
    backend: Backend,
    /// Pre-release builds called it `keep_faulted_takes`.
    keep_faulted_episodes: Option<bool>,
    keep_faulted_takes: Option<bool>,
    camera_roles: Option<Vec<usize>>,
    /// The USB port each slot reads, by `CAMERA_KEYS`; supersedes `camera_roles`.
    #[serde(deserialize_with = "lenient")]
    camera_ports: std::collections::HashMap<String, String>,
    /// Whether the rig has a camera on the headset; pre-release builds called
    /// it `front_camera`.
    head_camera: Option<bool>,
    front_camera: Option<bool>,
    /// `"standalone"` keeps a rig off the backend even when one is configured.
    #[serde(deserialize_with = "lenient")]
    mode: String,
    /// Standalone's task list and the one picked from it.
    #[serde(deserialize_with = "lenient")]
    tasks: Vec<LocalTask>,
    #[serde(deserialize_with = "lenient")]
    selected_task: String,
    /// `"h264"` or `"jpeg"`; see [`Video`].
    #[serde(deserialize_with = "lenient")]
    video: String,
    /// The folder new takes are filed under; see `record::FOLDER`.
    #[serde(deserialize_with = "lenient")]
    folder: String,
    announce_steps: Option<bool>,
    require_operator: Option<bool>,
    #[serde(deserialize_with = "lenient")]
    operator_picks_task: bool,
}

/// How camera frames go into `signals.mcap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Video {
    /// The cameras' own JPEGs as `CompressedImage`, for tools that cannot read
    /// H.264.
    Jpeg,
    /// Re-encoded on the headset as H.264 `CompressedVideo`, about 5x smaller
    /// (`misc::power::encode_bytes_per_sec`).
    H264,
}

impl Video {
    pub fn name(self) -> &'static str {
        match self {
            Video::Jpeg => "jpeg",
            Video::H264 => "h264",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub identity: Identity,
    pub backend: Backend,
    pub assignment: Option<Assignment>,
    /// What an episode recorded now is labelled with. Empty until the queue
    /// has been synced at least once.
    pub task: String,
    pub keep_faulted_episodes: bool,
    pub camera_roles: Option<Roles>,
    pub camera_ports: Option<Ports>,
    /// Whether a camera is fitted on the headset. Off, the head slot is never
    /// judged and owes no frames: a two-wrist rig is not a broken three-camera one.
    pub head_camera: bool,
    /// Recording with no backend at all: no dashboard, no assignment, no
    /// profile. Either asked for (`"mode": "standalone"`) or the only thing a
    /// rig without a URL and key can be. The default, for open-source users.
    pub standalone: bool,
    /// Standalone's own task list (`config.json`'s `tasks`).
    pub tasks: Vec<LocalTask>,
    /// `"mode": "standalone"` rather than merely unprovisioned.
    pub explicit_standalone: bool,
    /// Whether a dashboard queue has ever been cached here (`assigned.json`).
    pub queued: bool,
    /// Unset, a standalone rig records H.264 and a production rig records
    /// JPEG, so that production episodes stay readable by tools that cannot
    /// read H.264.
    pub video: Video,
    /// The folder new takes are filed under, `""` for the root.
    pub folder: String,
    /// Whether the headset says each step's name after its tone.
    pub announce_steps: bool,
    /// Production only: refuse a start until an operator has this robot open in
    /// the backend's teleop screen. Upstream's `require_active_operator`.
    pub require_operator: bool,
    /// Production only: the operator picks which queued task to record next
    /// (`selected_task`), instead of taking the head of the dashboard's queue.
    pub operator_picks_task: bool,
    pub selected_task: String,
    /// The tasks still queued here and how many episodes each has left, in
    /// queue order.
    pub queue: Vec<(String, usize)>,
}

/// What a device with no `config.json` runs as: standalone, "untitled".
impl Default for Config {
    fn default() -> Config {
        Config::from_raw(Raw::default())
    }
}

impl Config {
    pub fn load(dir: &str) -> Config {
        let path = Path::new(dir).join(FILE);
        let mut config = match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<serde_json::Value>(&text)
                .inspect(warn_unknown)
                .and_then(serde_json::from_value::<Raw>)
            {
                Ok(raw) => Config::from_raw(raw),
                Err(e) => {
                    log::error!("{path:?} is not valid JSON ({e}) — using defaults");
                    Config::default()
                }
            },
            // No file is a fresh install, which records standalone.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => {
                log::error!("{path:?} is unreadable ({e}) — using defaults");
                Config::default()
            }
        };
        // Asked-for standalone takes nothing from the dashboard's caches, even
        // ones a production run left behind; nor does a task picked locally,
        // which a cached assignment for some other task must not replace.
        if !config.explicit_standalone {
            config.overlay_profile(dir);
            if !(config.standalone && !config.task.is_empty()) {
                config.overlay_assigned(dir);
            }
        }
        config
    }

    fn from_raw(raw: Raw) -> Config {
        let unprovisioned = raw.backend.base_url.is_empty() || raw.backend.api_key.is_empty();
        let standalone = raw.mode == "standalone" || unprovisioned;
        // Once: the config is reloaded twice a second.
        static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if standalone && raw.mode == "production" && !SAID.swap(true, Ordering::Relaxed) {
            log::warn!(
                "config.json: production mode with no backend.base_url and api_key — standalone"
            );
        }
        // The task picked from the list, else the first one; standalone's
        // label otherwise falls back to `task()`'s "untitled".
        let task = match standalone {
            true if !raw.selected_task.is_empty() => raw.selected_task.clone(),
            true => raw.tasks.first().map(|t| t.name.clone()).unwrap_or_default(),
            false => String::new(),
        };
        let camera_roles = raw.camera_roles.map(from_slot_per_camera).and_then(|ranks| {
            Roles::from_ranks(&ranks).or_else(|| {
                log::error!("config.json: camera_roles {ranks:?} is not a permutation — ignoring");
                None
            })
        });
        Config {
            identity: raw.identity,
            backend: raw.backend,
            keep_faulted_episodes: raw
                .keep_faulted_episodes
                .or(raw.keep_faulted_takes)
                .unwrap_or(false),
            camera_roles,
            camera_ports: (!raw.camera_ports.is_empty())
                .then(|| CAMERA_KEYS.map(|key| raw.camera_ports.get(key).cloned())),
            head_camera: raw.head_camera.or(raw.front_camera).unwrap_or(false),
            assignment: None,
            task,
            standalone,
            tasks: raw.tasks,
            explicit_standalone: raw.mode == "standalone",
            queued: false,
            video: match raw.video.as_str() {
                "jpeg" => Video::Jpeg,
                "h264" => Video::H264,
                _ if standalone => Video::H264,
                _ => Video::Jpeg,
            },
            folder: crate::hub::episodes::clean_folder(&raw.folder).unwrap_or_default(),
            announce_steps: raw.announce_steps.unwrap_or(true),
            require_operator: raw.require_operator.unwrap_or(true),
            operator_picks_task: raw.operator_picks_task,
            selected_task: raw.selected_task,
            queue: Vec::new(),
        }
    }

    fn overlay_profile(&mut self, dir: &str) {
        let Some(profile) = Profile::load(dir) else {
            return;
        };
        for (mine, theirs) in [
            (&mut self.identity.organization, profile.organization),
            (&mut self.identity.site, profile.site),
            (&mut self.identity.location, profile.location),
            (&mut self.identity.robot_type, profile.robot_type),
            (&mut self.identity.robot_id, profile.robot_id),
            (&mut self.identity.operator, profile.operator),
        ] {
            if !theirs.is_empty() {
                *mine = theirs;
            }
        }
    }

    fn overlay_assigned(&mut self, dir: &str) {
        let cache = Assignment::cache(dir).unwrap_or_default();
        self.queued = !cache.episodes.is_empty() || !cache.last_task.is_empty();
        let unspent: Vec<Assignment> = cache.episodes.into_iter().filter(|a| !a.spent).collect();
        for a in &unspent {
            match self.queue.iter_mut().find(|(task, _)| *task == a.task) {
                Some((_, left)) => *left += 1,
                None => self.queue.push((a.task.clone(), 1)),
            }
        }
        let picking =
            self.operator_picks_task && !self.standalone && !self.selected_task.is_empty();
        // A pick with nothing queued waits for its copy (`poll::sync_once`)
        // with no task, so the start is refused rather than recorded under a
        // task nobody picked.
        if picking && !unspent.is_empty() {
            self.assignment = unspent.into_iter().find(|a| a.task == self.selected_task);
            self.task = self.assignment.as_ref().map(|a| a.task.clone()).unwrap_or_default();
            return;
        }
        self.assignment = unspent.into_iter().next();
        // Nothing waiting still leaves a name: the operator keeps collecting
        // the task they were last given, as unbound takes. With no cached name
        // at all, standalone keeps the one its own list gave it.
        let cached = match &self.assignment {
            Some(next) => next.task.clone(),
            None => cache.last_task,
        };
        if !cached.is_empty() || !self.standalone {
            self.task = cached;
        }
    }

    pub fn document(dir: &str) -> serde_json::Value {
        std::fs::read_to_string(Path::new(dir).join(FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_else(|| serde_json::json!({}))
    }

    /// Apply a [`Config::merge`] patch to the file. The outer error is the
    /// merge's refusal, the inner one the write's.
    pub fn edit(dir: &str, patch: &serde_json::Value) -> Result<std::io::Result<()>, String> {
        let _held = WRITER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let doc = Self::merge(&Self::document(dir), patch)?;
        Ok(Self::write(dir, &doc))
    }

    pub fn save(dir: &str, doc: &serde_json::Value) -> std::io::Result<()> {
        let _held = WRITER.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        Self::write(dir, doc)
    }

    fn write(dir: &str, doc: &serde_json::Value) -> std::io::Result<()> {
        // `doc` is an edit on `document`, which reads a file it cannot parse
        // as `{}`, so writing it would keep the edit and drop every setting.
        let path = Path::new(dir).join(FILE);
        let refuse = |why: String| {
            let fix = "fix it over adb, or reset the configuration in Setup";
            Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{why} — {fix}")))
        };
        let parses = |text: &str| {
            serde_json::from_str::<serde_json::Value>(text).is_ok_and(|d| d.is_object())
        };
        match std::fs::read_to_string(&path) {
            Ok(text) if !parses(&text) => return refuse("config.json is not valid JSON".into()),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return refuse(format!("config.json is unreadable ({e})"));
            }
            _ => {}
        }
        let text = serde_json::to_vec_pretty(doc)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        crate::misc::save::replace(&path, &text)
    }

    pub fn task(&self) -> &str {
        match self.task.is_empty() {
            true => "untitled",
            false => &self.task,
        }
    }

    /// The steps of the take being recorded: the assignment's, or on a
    /// standalone rig the ones its own task declares. Empty means one step,
    /// named after the task.
    pub fn subtask_names(&self) -> Vec<String> {
        match &self.assignment {
            Some(a) => a.subtasks.iter().map(|s| s.name.clone()).collect(),
            None => self.local().map(|t| t.subtasks.clone()).unwrap_or_default(),
        }
    }

    pub fn advances_when_still(&self) -> bool {
        self.local().is_some_and(|t| t.advance_when_still)
    }

    fn local(&self) -> Option<&LocalTask> {
        match self.assignment {
            None if self.standalone => self.tasks.iter().find(|t| t.name == self.task),
            _ => None,
        }
    }

    /// Whether a take may start for want of a task. Standalone always may:
    /// there is no queue to be empty, and an unnamed take is "untitled".
    pub fn has_task(&self) -> bool {
        self.standalone || !self.task.is_empty()
    }

    /// The dashboard's queue ran dry. A rig that never had one has nothing
    /// to finish.
    pub fn task_completed(&self) -> bool {
        self.queued && self.assignment.is_none() && !self.task.is_empty()
    }

    pub fn production(&self) -> bool {
        !self.standalone
    }
}

/// Say once which keys in `config.json` nothing reads, such as a pre-release
/// build's `upload` block: a setting that does nothing is otherwise found by its
/// absence. Once per change, because `load` runs on every backend poll.
fn warn_unknown(doc: &serde_json::Value) {
    static SAID: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    let unknown: Vec<String> = doc
        .as_object()
        .into_iter()
        .flat_map(|o| o.keys())
        .filter(|k| !EDITABLE.contains(&k.as_str()) && !RENAMED.iter().any(|(old, _)| old == k))
        .cloned()
        .collect();
    let mut said = SAID.lock().unwrap_or_else(|e| e.into_inner());
    if !unknown.is_empty() && *said != unknown {
        log::warn!("config.json: ignoring {}, which no setting reads", unknown.join(", "));
    }
    *said = unknown;
}

/// Pre-release builds' `camera_roles` read the other way round: the slot each
/// camera (by USB rank) feeds, with its headset camera in slot 3. This build
/// lists the rank each slot reads. A 3 can only be a pre-release build's, so those are
/// translated and everything else passes through.
fn from_slot_per_camera(slots: Vec<usize>) -> Vec<usize> {
    const DEV_FRONT: usize = 3;
    let mut want = slots.clone();
    want.sort_unstable();
    if want != [0, 1, DEV_FRONT] {
        return slots;
    }
    crate::hub::USB_CAM_SLOTS
        .iter()
        .map(|&slot| {
            let dev = if slot == crate::hub::SLOT_HEAD { DEV_FRONT } else { slot };
            slots.iter().position(|&s| s == dev).unwrap()
        })
        .collect()
}

/// The backend URL and key, or `None` for a rig nobody has provisioned yet:
/// it has nothing to report to and nothing to ask.
pub fn creds(dir: &str) -> Option<(String, String)> {
    let config = Config::load(dir);
    if config.standalone {
        return None;
    }
    let backend = config.backend;
    Some((backend.base_url, backend.api_key))
}
