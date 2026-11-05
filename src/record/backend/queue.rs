//! The rig's one backlog of what the backend has not heard yet.
//!
//! It sits beside the two conversations rather than inside either, because
//! both put entries in it: a take's start or finish through `episode`, and
//! the FSM's own subtask reports through [`with_queue`]. That shared order is
//! the point — a subtask completion must not overtake the episode finish it
//! belongs to.
//!
//! The file is upstream's: its `OfflineQueue` opens `report-queue.db` in the
//! files dir through the sandbox mount, exactly as the YUBI ROS stack's
//! (yubi-sw) node does. A
//! force-stop can strand the `.lock` directory its SQLite locks with, which
//! `misc::core::start` clears before airoa_core comes up.

use std::sync::Mutex;

use log::{info, warn};

use crate::misc::config::creds;
use airoa_sdk::backend::Client;

/// Serializes every use of the queue. Each call opens the file from its own
/// component instance, and the sandbox's SQLite locks with a directory: a
/// second opener does not wait its turn, it burns the busy timeout and fails.
static QUEUE: Mutex<()> = Mutex::new(());

/// Run one errand that uses the backlog. The FSM shares this queue with
/// [`with_client`], so both go through here: one rig has one backlog, and a
/// subtask completion must not overtake the episode finish it belongs to.
pub(crate) fn with_queue<T>(errand: impl FnOnce() -> T) -> T {
    let _held = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    errand()
}

/// Run one errand against a client on the rig's backlog. `None` is a rig with
/// no URL and key yet, which has nothing to report to.
pub(crate) fn with_client<T>(dir: &str, errand: impl FnOnce(&Client) -> T) -> Option<T> {
    let (base_url, api_key) = creds(dir)?;
    let _held = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    Some(errand(&Client::new(&base_url, &api_key, &crate::misc::core::queue_guest_path())))
}

/// One replay pass over the backlog, if there is one. Upstream replays at most
/// five per call, so a long backlog drains over several beats rather than
/// parking this thread for a request timeout apiece.
pub(crate) fn flush_once(dir: &str) {
    let before_and_after = |c: &Client| c.pending().and_then(|before| Ok((before, c.flush()?)));
    match with_client(dir, before_and_after) {
        Some(Ok((0, 0))) => {}
        Some(Ok((_, 0))) => info!("backend: queued episode reports all delivered"),
        Some(Ok((_, left))) => info!("backend: {left} episode reports still queued"),
        Some(Err(e)) => warn!("backend: queued reports not flushed: {e}"),
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::super::episode::{report_to, Boundary};
    use super::*;
    use crate::test_http::{files_root, point_at, serve, up, QUEUE_TURN};

    /// The point of the queue: a boundary the backend misses is not lost. The
    /// two halves have to be checked together, because a queue that never
    /// drains is as bad as one that never fills.
    #[test]
    fn a_report_the_backend_missed_waits_on_disk_and_lands_on_the_next_flush() {
        up();
        let _turn = QUEUE_TURN.lock().unwrap_or_else(|e| e.into_inner());
        let queue = files_root().join(crate::misc::core::DB_DIR).join("report-queue.db");
        let _ = std::fs::remove_file(&queue);
        let dir = std::env::temp_dir().join(format!("yubigo_report_queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_str().unwrap().to_string();

        // Port 1 rather than a freed ephemeral one: a port learned by binding
        // and dropping goes back in the pool, and another test's server taking
        // it makes this "unreachable" backend answer.
        point_at(&dir, "http://127.0.0.1:1");
        assert_eq!(
            report_to(&dir, Boundary::Finished, "ep-0"),
            Some(false),
            "an unreachable backend queues"
        );
        assert!(queue.exists(), "the report must survive on disk, not in the process");

        let (base, rx) = serve("{}");
        point_at(&dir, &base);
        flush_once(&dir);
        let request = rx.recv().unwrap();
        assert!(request.starts_with("POST /robot/episodes/ep-0/finish"), "{request}");
        let left = with_client(&dir, Client::pending).unwrap().unwrap();
        assert_eq!(left, 0, "a drained queue holds nothing");
    }
}
