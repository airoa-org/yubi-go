//! Typed wrappers over the backend ops. The op names and args shapes live
//! here, next to the structs they answer with — the host never spells an op
//! string or assembles a backend payload by hand.

use serde::de::DeserializeOwned;
use serde::Deserialize;

use crate::Error;

/// `backend_me` — the robot's canonical profile from `/robot/me`.
/// Generic because the host keeps its own cache struct for the answer.
pub fn me<T: DeserializeOwned>(base_url: &str, api_key: &str) -> Result<T, Error> {
    call("backend_me", base_url, api_key, serde_json::json!({}))
}

/// Upstream's five-key profile shape. `model` and `active_operator` stay
/// untyped: the backend's own bodies land in them unchanged.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub id: String,
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub organization_name: Option<String>,
    pub location_name: Option<String>,
    pub site_name: Option<String>,
    pub active_operator: Option<serde_json::Value>,
    /// False when `/robot/me` could not be fetched at all, which upstream
    /// keeps separate from "fetched fine, nobody logged in".
    pub fetch_ok: bool,
}

/// `default_robot_profile` — the profile a caller stands in with when it has
/// nothing to ask the backend with (no API key, no network). `id` reads
/// `"unknown"` and every other field is absent. Touches no network.
pub fn default_profile() -> Result<Profile, Error> {
    let v = crate::call("default_robot_profile", serde_json::json!({}))?;
    serde_json::from_value(v).map_err(|e| Error::Python(format!("default_robot_profile: {e}")))
}

/// `operator_name_from_profile` — the active operator's display name from a
/// profile body, or `""` when nobody is logged in on the robot. Takes the raw
/// body so a cached `/robot/me` answer can be read without a round trip to the
/// backend; touches no network.
pub fn operator_name(profile: &serde_json::Value) -> Result<String, Error> {
    crate::runtime::call_str("operator_name_from_profile", serde_json::json!({"profile": profile}))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Report {
    Start,
    Finish,
    Cancel,
}

/// `backend_report` — an episode boundary, stamped by upstream's own clock.
pub fn report(
    base_url: &str,
    api_key: &str,
    action: Report,
    episode_id: &str,
) -> Result<(), Error> {
    let action = match action {
        Report::Start => "start",
        Report::Finish => "finish",
        Report::Cancel => "cancel",
    };
    let args = serde_json::json!({"action": action, "episode_id": episode_id});
    call::<serde_json::Value>("backend_report", base_url, api_key, args).map(|_| ())
}

/// `select_episode`'s answer: either the enriched episode row, or upstream's
/// reason there is none. Never both meaningful at once.
#[derive(Debug, Deserialize)]
pub struct Selection<T> {
    pub episode: Option<T>,
    #[serde(default)]
    pub reason: String,
}

/// `select_episode` — upstream's EpisodeSelector picks the next episode.
pub fn select_episode<T: DeserializeOwned>(
    base_url: &str,
    api_key: &str,
    auto_repeat: bool,
    operator_name: &str,
    completed_ids: &[String],
) -> Result<Selection<T>, Error> {
    let args = serde_json::json!({
        "auto_repeat": auto_repeat,
        "operator_name": operator_name,
        "completed_ids": completed_ids,
    });
    call("select_episode", base_url, api_key, args)
}

/// `list_episodes` — every ready episode, oldest first, minus `completed_ids`.
/// The next one of each task carries its subtasks; the others may not.
pub fn list_episodes<T: DeserializeOwned>(
    base_url: &str,
    api_key: &str,
    operator_name: &str,
    completed_ids: &[String],
) -> Result<Vec<T>, Error> {
    let args = serde_json::json!({
        "operator_name": operator_name,
        "completed_ids": completed_ids,
    });
    call("list_episodes", base_url, api_key, args)
}

/// `repeat_last` — a new episode copying the robot's last finished or
/// cancelled one. `None` while the backend still holds an episode open.
pub fn repeat_last<T: DeserializeOwned>(
    base_url: &str,
    api_key: &str,
    operator_name: &str,
) -> Result<Option<T>, Error> {
    call("repeat_last", base_url, api_key, serde_json::json!({"operator_name": operator_name}))
}

/// `backend_status` — the heartbeat.
pub fn status(base_url: &str, api_key: &str, inputs: &crate::StatusInputs) -> Result<(), Error> {
    let args = serde_json::json!({"inputs": inputs});
    match call::<serde_json::Value>("backend_status", base_url, api_key, args)? {
        serde_json::Value::Null => {
            Err(Error::Refused("PUT /robot/status was not accepted; see airoa_core's log".into()))
        }
        _ => Ok(()),
    }
}

fn call<T: DeserializeOwned>(
    op: &str,
    base_url: &str,
    api_key: &str,
    extra: serde_json::Value,
) -> Result<T, Error> {
    let mut args = serde_json::json!({
        "base_url": base_url.trim_end_matches('/'),
        "api_key": api_key,
    });
    if let (Some(into), Some(from)) = (args.as_object_mut(), extra.as_object()) {
        for (k, v) in from {
            into.insert(k.clone(), v.clone());
        }
    }
    let v = crate::call(op, args)?;
    serde_json::from_value(v).map_err(|e| Error::Python(format!("{op}: unreadable answer: {e}")))
}

/// A backend that keeps what it could not send.
///
/// [`report`] drops an episode boundary the moment the link is down. This one
/// hands it to upstream's `OfflineQueue` instead — a FIFO replayed in
/// insertion order by [`flush`](Client::flush) once the backend answers again.
///
/// The queue is upstream's SQLite file at `queue_path`, a guest path under a
/// read-write [`add_preopen`](crate::add_preopen), exactly as the node of the
/// YUBI ROS stack (yubi-sw)
/// passes `offline_queue_path`. The file outlives the process on its own.
/// Every call opens it afresh, so two callers at once contend on SQLite's
/// lock rather than on anything in memory: serialize them on the host.
///
/// This SQLite locks with a `<queue_path>.lock` directory (the WASI build's
/// `unix-dotfile` VFS); a process killed mid-write strands it, and every later
/// open fails with "database is locked". Remove it before starting the core.
/// `queue_path` must sit at least one directory below the mount: upstream
/// opens it with `os.makedirs(dirname)`, and a dirname that is the mount
/// itself makes makedirs look for `/`, which the sandbox does not have.
#[derive(Default, Clone, PartialEq, Eq)]
pub struct Client {
    base_url: String,
    api_key: String,
    queue_path: String,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("api_key", &crate::redacted(&self.api_key))
            .field("queue_path", &self.queue_path)
            .finish()
    }
}

impl Client {
    /// A client on the queue at `queue_path`, created on first use.
    pub fn new(base_url: &str, api_key: &str, queue_path: &str) -> Self {
        Self {
            // `call` trims the trailing slash; keep the caller's string as given.
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
            queue_path: queue_path.to_string(),
        }
    }

    /// `backend_report_queued` — an episode boundary that survives an outage.
    ///
    /// `Ok(true)` is the backend having accepted it. `Ok(false)` is upstream
    /// not having got it through: a connectivity failure or a transient
    /// 5xx/408/429 is now queued, while a permanent 4xx is dropped on purpose
    /// so it cannot block the backlog behind it — [`pending`](Self::pending)
    /// tells those two apart. `Err` is the boundary itself breaking.
    ///
    /// While a backlog exists a fresh report is appended rather than sent
    /// ahead of it, so the order the caller reported in is the order the
    /// backend sees.
    pub fn report(&self, action: Report, episode_id: &str) -> Result<bool, Error> {
        let action = match action {
            Report::Start => "start",
            Report::Finish => "finish",
            Report::Cancel => "cancel",
        };
        let reply = self.call(
            "backend_report_queued",
            serde_json::json!({"action": action, "episode_id": episode_id}),
        )?;
        Ok(reply["sent"].as_bool().unwrap_or_default())
    }

    /// `flush_offline_queue` — one replay pass, answering how many entries are
    /// still waiting after it.
    ///
    /// Upstream replays at most five per call so a long backlog cannot park
    /// the caller for a request timeout apiece; call again while the answer is
    /// non-zero. Its exponential backoff lives on a backend object that does
    /// not survive between calls here, so **pacing is the host's**: a
    /// still-unreachable backend is retried as fast as this is called.
    pub fn flush(&self) -> Result<usize, Error> {
        let reply = self.call("flush_offline_queue", serde_json::json!({}))?;
        pending(&reply)
    }

    /// `offline_pending` — how many entries the queue holds. Sends no report
    /// and touches no network.
    pub fn pending(&self) -> Result<usize, Error> {
        let v = crate::call(
            "offline_pending",
            serde_json::json!({"offline_queue_path": self.queue_path}),
        )?;
        pending(&v)
    }

    fn call(&self, op: &str, mut extra: serde_json::Value) -> Result<serde_json::Value, Error> {
        extra["offline_queue_path"] = serde_json::json!(self.queue_path);
        call(op, &self.base_url, &self.api_key, extra)
    }
}

fn pending(reply: &serde_json::Value) -> Result<usize, Error> {
    reply["pending"]
        .as_u64()
        .map(|n| n as usize)
        .ok_or_else(|| Error::Python(format!("offline queue: unreadable answer: {reply}")))
}

#[cfg(test)]
mod offline_queue_tests {
    use super::*;
    use crate::test_http::Server;

    fn up() {
        let payload = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build");
        crate::start(&payload).unwrap_or_else(|e| panic!("{e}\n"));
    }

    /// A fresh queue file for one test, on its own read-write mount. The
    /// directory is never removed: a worker spawned by another test opens
    /// every mount at once.
    fn queue(name: &str) -> (std::path::PathBuf, String) {
        up();
        let base = std::env::temp_dir().join(format!("airoa_sdk_queues-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        crate::add_preopen(&base, "/queues", crate::Access::ReadWrite);
        let file = base.join("db").join(format!("{name}.db"));
        let _ = std::fs::remove_file(&file);
        (file, format!("/queues/db/{name}.db"))
    }

    /// Nothing listens on port 1, so every request fails to connect at once —
    /// the outage this whole type exists for, without a timeout to wait out.
    const UNREACHABLE: &str = "http://127.0.0.1:1";

    fn paths(server: &Server) -> Vec<String> {
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter_map(|line| line.split(' ').nth(1).map(str::to_owned))
            .collect()
    }

    #[test]
    fn an_unreachable_backend_queues_the_report_instead_of_losing_it() {
        let (file, guest) = queue("unreachable");
        let client = Client::new(UNREACHABLE, "k", &guest);
        assert!(!client.report(Report::Start, "e1").unwrap(), "nothing reached the backend");
        assert!(file.exists(), "upstream wrote the queue to disk");
        assert_eq!(client.pending().unwrap(), 1);
    }

    #[test]
    fn a_flush_drains_the_backlog_in_the_order_it_was_reported() {
        let (_, guest) = queue("order");
        let client = Client::new(UNREACHABLE, "k", &guest);
        for (action, id) in [(Report::Start, "e1"), (Report::Finish, "e1"), (Report::Cancel, "e2")]
        {
            assert!(!client.report(action, id).unwrap());
        }
        assert_eq!(client.pending().unwrap(), 3);

        let server = Server::new();
        let client = Client::new(&server.url, "k", &guest);
        assert_eq!(client.flush().unwrap(), 0, "three is under upstream's batch of five");
        assert_eq!(client.pending().unwrap(), 0);
        assert_eq!(
            paths(&server),
            ["/robot/episodes/e1/start", "/robot/episodes/e1/finish", "/robot/episodes/e2/cancel",]
        );
    }

    #[test]
    fn a_transient_status_keeps_the_entry_and_a_permanent_one_drops_it() {
        let (_, guest) = queue("statuses");
        let server = Server::new();
        // 503: upstream queues it rather than losing it.
        *server.status.lock().unwrap() = 503;
        let client = Client::new(&server.url, "k", &guest);
        assert!(!client.report(Report::Start, "e1").unwrap());
        assert_eq!(client.pending().unwrap(), 1);

        // Still 503 on replay: the same backlog waits for another flush.
        assert_eq!(client.flush().unwrap(), 1, "a 5xx replay keeps the entry");

        // 422: permanent, so it is dropped rather than blocking what is behind it.
        *server.status.lock().unwrap() = 422;
        assert_eq!(client.flush().unwrap(), 0, "a permanent 4xx is dropped on replay");

        // And a fresh permanent failure never enters the queue at all.
        assert!(!client.report(Report::Finish, "e1").unwrap());
        assert_eq!(client.pending().unwrap(), 0);
    }

    #[test]
    fn the_queue_file_survives_a_client_that_does_not() {
        let (_, guest) = queue("survives");
        let offline = Client::new(UNREACHABLE, "k", &guest);
        assert!(!offline.report(Report::Start, "e9").unwrap());
        drop(offline);

        assert_eq!(Client::new(UNREACHABLE, "k", &guest).pending().unwrap(), 1);
        let server = Server::new();
        assert_eq!(Client::new(&server.url, "k", &guest).flush().unwrap(), 0);
        assert_eq!(paths(&server), ["/robot/episodes/e9/start"]);
    }

    #[test]
    fn a_reachable_backend_leaves_the_queue_empty() {
        let (_, guest) = queue("reachable");
        let server = Server::new();
        let client = Client::new(&server.url, "k", &guest);
        assert!(client.report(Report::Start, "e1").unwrap(), "the backend took it");
        assert_eq!(client.pending().unwrap(), 0);
        assert_eq!(client.flush().unwrap(), 0, "and a flush with no backlog sends nothing");
        assert_eq!(paths(&server), ["/robot/episodes/e1/start"]);
    }
}
