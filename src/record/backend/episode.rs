//! Telling the backend an episode began, ended or was abandoned.
//!
//! A boundary the backend misses is queued rather than lost, and the queue
//! is shared with the FSM's own reports so one rig keeps one backlog in one
//! order.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use log::{info, warn};

use super::queue::with_client;
use crate::hub::EPISODE_DIR;
use crate::misc::config::{creds, Assignment};

/// How many takes in a row were recorded without an assignment behind them.
/// A fact about the takes, so it is counted here and only displayed by the
/// panel and `/state`.
static UNBOUND: AtomicUsize = AtomicUsize::new(0);

pub fn mark_bound(bound: bool) {
    match bound {
        true => UNBOUND.store(0, Ordering::Relaxed),
        false => {
            UNBOUND.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub fn unbound() -> usize {
    UNBOUND.load(Ordering::Relaxed)
}

pub fn consumed(dir: String, id: String) {
    if id.is_empty() {
        return;
    }
    if let Err(e) = Assignment::consume(&dir, &id) {
        warn!("backend: episode {id} not marked consumed: {e}");
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Boundary {
    Started,
    Finished,
    Cancelled,
}

/// The episode the backend still believes is recording. Written before the
/// `Start` goes out and cleared once the take is closed, so a process that
/// dies mid-take leaves the id behind for [`close_stale`] to find.
const OPEN_FILE: &str = "open-episode";

fn open_path(dir: &str) -> PathBuf {
    Path::new(dir).join(OPEN_FILE)
}

fn note_open(dir: &str, boundary: Boundary, episode_id: &str) {
    let path = open_path(dir);
    match boundary {
        Boundary::Started => {
            if let Err(e) = crate::misc::save::replace(&path, episode_id.as_bytes()) {
                warn!("backend: {episode_id} not noted as open ({e}) — a crash will strand it");
            }
        }
        // Only our own id: a late cancel from `close_stale` must not clear
        // the episode started after it.
        _ => {
            if std::fs::read_to_string(&path).is_ok_and(|open| open.trim() == episode_id) {
                drop(std::fs::remove_file(&path));
            }
        }
    }
}

/// Close whatever take the last process died inside of. YUBI Go deletes an
/// interrupted recording at startup (it has no `meta.json`, so there is
/// nothing to resume), which leaves upstream's `cancel_stale` as the only
/// reachable outcome — so the planner's other branches are not wired.
///
/// The cancel goes through the queue like any other, so a rig that restarts
/// with no network still closes the episode once the network returns. It
/// runs before `misc::core::start`, so it waits for airoa_core, and the file
/// stays until the cancel was sent or queued.
pub fn close_stale(dir: &str) {
    let Ok(id) = std::fs::read_to_string(open_path(dir)) else { return };
    let id = id.trim().to_string();
    if id.is_empty() {
        drop(std::fs::remove_file(open_path(dir)));
        return;
    }
    warn!("backend: episode {id} was left open by a previous run — cancelling it");
    let dir = dir.to_string();
    std::thread::spawn(move || {
        while !crate::misc::core::up() {
            std::thread::sleep(CORE_POLL);
        }
        // Queued counts as closed: the entry outlives this process.
        if report_to(&dir, Boundary::Cancelled, &id).is_some() {
            note_open(&dir, Boundary::Cancelled, &id);
        }
    });
}

/// How often [`close_stale`] looks for airoa_core, which takes seconds to
/// start. Chosen by hand.
const CORE_POLL: std::time::Duration = std::time::Duration::from_millis(500);

/// Tell the backend an episode began, ended or was abandoned. A boundary the
/// backend misses is queued rather than lost, because the rig's network is
/// the thing most likely to be down. `Some(true)` is sent, `Some(false)` is
/// queued, and `None` is neither.
pub(super) fn report_to(dir: &str, boundary: Boundary, episode_id: &str) -> Option<bool> {
    let action = match boundary {
        Boundary::Started => airoa_sdk::backend::Report::Start,
        Boundary::Finished => airoa_sdk::backend::Report::Finish,
        Boundary::Cancelled => airoa_sdk::backend::Report::Cancel,
    };
    match with_client(dir, |c| c.report(action, episode_id)) {
        Some(Ok(true)) => Some(true),
        Some(Ok(false)) => {
            warn!("backend: episode {episode_id} {boundary:?} did not land — queued");
            Some(false)
        }
        Some(Err(e)) => {
            warn!("backend: episode {episode_id} {boundary:?} not reported: {e}");
            None
        }
        None => None,
    }
}

/// Set when a finish has landed, so the backend thread knows to ask what to
/// record next without this module reaching across for it.
static REFRESH: AtomicBool = AtomicBool::new(false);

/// Whether a finish has landed since the last time anyone asked.
pub fn refresh_wanted() -> bool {
    REFRESH.swap(false, Ordering::Relaxed)
}

/// The task of the last bound take to end, finished or discarded, until the
/// backend thread has made sure that task still has an episode queued.
static ENDED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn take_ended(task: &str) {
    *ENDED.lock().unwrap() = Some(task.to_string());
}

pub fn ended_task() -> Option<String> {
    ENDED.lock().unwrap().clone()
}

pub fn ended_task_handled() {
    *ENDED.lock().unwrap() = None;
}

pub fn report(boundary: Boundary, episode_id: String) {
    let dir = EPISODE_DIR.to_string();
    if episode_id.is_empty() || creds(&dir).is_none() {
        return;
    }
    note_open(&dir, boundary, &episode_id);
    std::thread::spawn(move || {
        if report_to(&dir, boundary, &episode_id) != Some(true) {
            return;
        }
        info!("backend: episode {episode_id} {boundary:?}");
        if matches!(boundary, Boundary::Finished) {
            // Ask the backend thread (`poll`) for the next episode rather
            // than fetching it here, so the queue cache has one writer.
            REFRESH.store(true, Ordering::Relaxed);
        }
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::test_http::{files_root, point_at, serve, up, QUEUE_TURN};

    /// The gap that corrupts shared state: a crash mid-take leaves the backend
    /// showing an episode that will never end, blocking the operator's queue.
    #[test]
    fn a_take_the_process_died_inside_of_is_cancelled_on_the_next_start() {
        let _turn = QUEUE_TURN.lock().unwrap_or_else(|e| e.into_inner());
        files_root();
        let dir = std::env::temp_dir().join(format!("yubigo_stale_episode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_str().unwrap().to_string();
        let (base, rx) = serve("{}");
        point_at(&dir, &base);

        // A take that began and never ended, as a killed process leaves it.
        note_open(&dir, Boundary::Started, "ep-7");
        assert!(open_path(&dir).exists());

        // Before airoa_core is up, as on the device. Other tests may have
        // started it already, so this half is only exercised when this test
        // runs first.
        close_stale(&dir);
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            crate::misc::core::up() || open_path(&dir).exists(),
            "the file must outlive a cancel that could not be sent"
        );
        up();
        let request = rx.recv().unwrap();
        assert!(request.starts_with("POST /robot/episodes/ep-7/cancel"), "{request}");
        for _ in 0..50 {
            if !open_path(&dir).exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!open_path(&dir).exists(), "a closed episode must not be cancelled twice");

        // And a clean shutdown leaves nothing for the next start to find.
        note_open(&dir, Boundary::Started, "ep-8");
        note_open(&dir, Boundary::Finished, "ep-8");
        assert!(!open_path(&dir).exists());
    }
}
