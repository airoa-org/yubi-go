use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use log::{info, warn};

use super::config::{GUEST_ROOT, NO_FILE};

pub(super) static DEPTH: AtomicUsize = AtomicUsize::new(0);

pub fn depth() -> usize {
    DEPTH.load(Ordering::Relaxed)
}

/// The episodes this process has seen land, for the panel's stage column.
/// The upload-state database is the truth, but it lives inside the component
/// and the panel redraws every frame, so the answer is cached here as the
/// uploader learns it.
static SENT: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

pub fn was_sent(name: &str) -> bool {
    SENT.lock().map(|sent| sent.contains(name)).unwrap_or(false)
}

pub(super) fn note_sent(names: impl IntoIterator<Item = String>) {
    if let Ok(mut sent) = SENT.lock() {
        sent.extend(names);
    }
}

/// The take the uploader is sending, so the page cannot delete it mid-send.
static SENDING: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Runs `f` unless `take` is being sent. The lock is held while `f` runs, so
/// the uploader cannot start on the take in between.
pub fn unless_sending<T>(take: &Path, f: impl FnOnce() -> T) -> Option<T> {
    let sending = SENDING.lock().unwrap_or_else(|e| e.into_inner());
    (sending.as_deref() != Some(take)).then(f)
}

/// Marks `take` as the one being sent until the guard drops, or answers
/// `None` when it was deleted while the queue was being read.
pub(crate) fn sending(take: &Path) -> Option<Sending> {
    let mut sending = SENDING.lock().unwrap_or_else(|e| e.into_inner());
    if !take.is_dir() {
        return None;
    }
    *sending = Some(take.to_path_buf());
    Some(Sending)
}

pub(crate) struct Sending;

impl Drop for Sending {
    fn drop(&mut self) {
        *SENDING.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    NoTarget(String),
    Idle(String),
    Unreachable(String),
    Sending { done: usize, total: usize },
    Retrying(String),
}

impl Default for State {
    fn default() -> Self {
        State::Idle(String::new())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Bad,
}

impl State {
    pub fn verdict(&self) -> (String, Level) {
        match self {
            State::NoTarget(why) if why == NO_FILE => ("uploads off".to_string(), Level::Ok),
            State::NoTarget(why) => (format!("NOT UPLOADING - {why}"), Level::Bad),
            State::Idle(who) if who.is_empty() => ("upload queue empty".to_string(), Level::Ok),
            State::Idle(who) => (format!("upload queue empty - {who}"), Level::Ok),
            State::Unreachable(why) => (format!("BUCKET UNREACHABLE - {why}"), Level::Warn),
            State::Sending { done, total } => {
                (format!("uploading, file {}/{total}", done + 1), Level::Ok)
            }
            State::Retrying(e) => (format!("upload retrying - {e}"), Level::Warn),
        }
    }
}

static STATE: std::sync::Mutex<State> = std::sync::Mutex::new(State::Idle(String::new()));

pub fn state() -> State {
    STATE.lock().map(|s| s.clone()).unwrap_or_default()
}

pub(super) fn set(next: State) {
    if let Ok(mut state) = STATE.lock() {
        *state = next;
    }
}

pub(super) fn stop(said: &mut Option<String>, why: &str, line: &str) {
    set(State::NoTarget(why.to_string()));
    say(said, line.to_string());
}

pub(super) fn say(said: &mut Option<String>, line: String) {
    if said.as_deref() != Some(line.as_str()) {
        info!("{line}");
        *said = Some(line);
    }
}

/// Point `state` at the target `storage.yaml` names. Does nothing until
/// airoa_core can read that file, and nothing again once the name matches.
pub(super) fn state_for(dir: &str, state: &mut airoa_sdk::storage::UploadState) {
    let name = super::config::storage_settings(dir, GUEST_ROOT).map(|s| s.name).unwrap_or_default();
    if name.is_empty() || state.target() == name {
        return;
    }
    *state =
        airoa_sdk::storage::UploadState::new(&name, &crate::misc::core::upload_state_guest_path());
}

pub(super) fn pending(dir: &str, state: &airoa_sdk::storage::UploadState) -> Vec<PathBuf> {
    let takes = crate::record::record::takes(dir);
    let Ok(entries) = std::fs::read_dir(&takes) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter(|e| crate::record::record::is_complete(&e.path()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    // One crossing for the whole directory, and the order asked in is the
    // order answered. A state that cannot be read sends nothing rather than
    // guessing: guessing wrong sends a take twice.
    match state.needing_upload(&refs) {
        Ok(waiting) => {
            // Whatever the state does not want sent has landed already —
            // including takes a previous process sent, which is the half the
            // panel's own memory cannot recover after a restart.
            note_sent(names.iter().filter(|n| !waiting.contains(n)).cloned());
            waiting.iter().map(|n| Path::new(&takes).join(n)).collect()
        }
        Err(e) => {
            warn!("upload: upload state unreadable: {e} — nothing sent this pass");
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yubigo_upload_{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const META: &str = r#"{"uuid":"abc-123"}"#;

    /// `complete` writes meta.json, which is what marks a take finished.
    fn episode(base: &Path, name: &str, complete: bool) -> PathBuf {
        let dir = base.join(crate::record::record::TAKES).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::record::record::SIGNALS), [0u8; 16]).unwrap();
        if complete {
            std::fs::write(dir.join("meta.json"), META).unwrap();
        }
        dir
    }

    fn core() {
        let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("airoa-sdk/build");
        airoa_sdk::start(&payload)
            .expect("the payload and its cwasm must both be built — see the error");
    }

    fn take(base: &Path, name: &str) -> PathBuf {
        base.join(crate::record::record::TAKES).join(name)
    }

    #[test]
    fn only_a_finished_unsent_episode_is_eligible() {
        core();
        let base = temp("eligible");
        episode(&base, "episode_2", true);
        episode(&base, "episode_3", false);
        episode(&base, "episode_1", true);
        airoa_sdk::add_preopen(&base, "/eligible_probe", airoa_sdk::Access::ReadWrite);

        let state = airoa_sdk::storage::UploadState::new("rig", "/eligible_probe/db/state.db");
        assert_eq!(
            pending(base.to_str().unwrap(), &state),
            vec![take(&base, "episode_1"), take(&base, "episode_2")],
            "a fresh take is pending; one without meta.json is not finished"
        );

        state.uploaded(&["episode_1"]).unwrap();
        assert_eq!(
            pending(base.to_str().unwrap(), &state),
            vec![take(&base, "episode_2")],
            "and a take the state calls uploaded is never offered again"
        );
    }

    #[test]
    fn the_backlog_goes_before_the_take_just_finished() {
        core();
        let base = temp("order");
        episode(&base, "episode_1787154123798", true);
        episode(&base, "episode_1787144815931", true);
        airoa_sdk::add_preopen(&base, "/order_probe", airoa_sdk::Access::ReadWrite);
        let state = airoa_sdk::storage::UploadState::new("rig", "/order_probe/db/state.db");
        assert_eq!(
            pending(base.to_str().unwrap(), &state),
            vec![take(&base, "episode_1787144815931"), take(&base, "episode_1787154123798"),]
        );
    }

    #[test]
    fn a_missing_target_is_a_fault() {
        let (line, level) = State::NoTarget("no upload target".into()).verdict();
        assert_eq!(level, Level::Bad, "{line}");
        assert!(line.contains("NOT UPLOADING"), "{line}");
    }

    #[test]
    fn no_storage_yaml_is_uploads_off_not_a_fault() {
        let (line, level) = State::NoTarget(NO_FILE.into()).verdict();
        assert_eq!((line.as_str(), level), ("uploads off", Level::Ok));
    }
}
