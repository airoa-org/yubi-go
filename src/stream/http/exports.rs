//! A LeRobot export, started here and run in the background, and the
//! datasets it leaves.

use std::net::TcpStream;

use super::response::{send, send_tar};
use super::route::safe_episode;
use crate::hub::{Hub, EPISODE_DIR};

/// One dataset as a tar, once it is finished.
pub(super) fn serve_tar(sock: &mut TcpStream, id: &str) -> std::io::Result<()> {
    use crate::hub::export::{dir, files, is_done};
    if !is_done(EPISODE_DIR, id) {
        return send(sock, 404, "text/plain", b"no such export\n");
    }
    let entries = files(&dir(EPISODE_DIR, id), &format!("{id}/"));
    send_tar(sock, &format!("{id}.tar"), &entries)
}

pub(super) fn delete(sock: &mut TcpStream, id: &str) -> std::io::Result<()> {
    match crate::hub::export::remove(EPISODE_DIR, id) {
        Ok(()) => send(sock, 200, "application/json", br#"{"deleted": true}"#),
        Err(_) => send(sock, 404, "application/json", br#"{"error": "no such export"}"#),
    }
}

/// `{"name": "...", "episodes": ["episode_...", ...], "skip_failed": bool,
/// "skip_untracked": bool}`:
/// checked here, so a mistyped take is a 400 now rather than a failed export
/// later.
pub(super) fn start_export(hub: &Hub, takes: &str, body: &[u8]) -> (u16, serde_json::Value) {
    use serde_json::json;
    let Ok(req) = serde_json::from_slice::<serde_json::Value>(body) else {
        return (400, json!({ "error": "expected {\"name\": ..., \"episodes\": [...]}" }));
    };
    let names: Vec<String> = req["episodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| Some(t.as_str()?.to_string()))
        .collect();
    if names.is_empty() {
        return (400, json!({ "error": "no episodes chosen" }));
    }
    let root = std::path::Path::new(takes);
    if let Some(bad) =
        names.iter().find(|n| !safe_episode(n) || !crate::hub::episodes::is_complete(&root.join(n)))
    {
        return (400, json!({ "error": format!("no such episode: {bad}") }));
    }
    // An export takes every core, so it waits for anything else that works
    // the CPU or the codecs to finish or be stopped.
    if hub.capturing() {
        return (409, json!({ "error": "the rig is armed or recording: stop it first" }));
    }
    let Some(video) = export_video() else {
        return (501, json!({ "error": "this build has no encoder" }));
    };
    let name = req["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or("dataset");
    let leave = crate::hub::export::Leave {
        failed: req["skip_failed"].as_bool().unwrap_or(false),
        untracked: req["skip_untracked"].as_bool().unwrap_or(false),
    };
    match crate::hub::export::start(EPISODE_DIR, name, names, leave, video) {
        Ok(id) => (202, json!({ "id": id })),
        Err(e) => (409, json!({ "error": e })),
    }
}

/// The headset's codecs for an export, which only an Android build has.
fn export_video() -> Option<Box<dyn crate::hub::export::Video + Send>> {
    #[cfg(target_os = "android")]
    return Some(Box::new(crate::hub::export::Transcoder { stop: || crate::app::hub().busy() }));
    #[cfg(not(target_os = "android"))]
    None
}
