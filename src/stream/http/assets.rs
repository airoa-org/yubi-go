//! What this server hands out unchanged: the two pages and the two scripts,
//! inlined at build time, and the files the app already carries — the
//! meshes and the spoken names.

use std::net::TcpStream;

use super::response::send;
use crate::hub::EPISODE_DIR;

/// The side interface.
///
/// One self-contained document — no CDN, no framework fetched at load. The rig
/// network routinely has no internet and the page is served *by a headset on
/// it*, so anything not in this file is not going to arrive.
pub(super) const PAGE: &str = include_str!("../../../assets/web/page.html");

/// `curl -s http://<headset>:8080/pull.py | python3 - http://<headset>:8080`
pub(super) const PULL: &str = include_str!("../../../assets/web/pull.py");

/// Opens a LeRobot export in Rerun; the viewer prints the command for each export.
pub(super) const RERUN_VIEW: &str = include_str!("../../../assets/web/rerun_view.py");

/// The episode viewer, self-contained for the same reason [`PAGE`] is.
pub(super) const VIEWER: &str = include_str!("../../../assets/web/viewer.html");

/// Under the files dir, as `android/build.sh` puts it in the APK's assets.
pub const LICENSES: &str = "LICENSES.txt";

pub(super) fn licenses(sock: &mut TcpStream) -> std::io::Result<()> {
    match std::fs::read(std::path::Path::new(EPISODE_DIR).join(LICENSES)) {
        Ok(text) => send(sock, 200, "text/plain; charset=utf-8", &text),
        Err(_) => send(sock, 404, "text/plain", b"this build carries no LICENSES.txt\n"),
    }
}

/// The meshes the headset renders, already in the binary, so the viewer costs
/// the APK nothing for them.
pub(super) fn mesh(sock: &mut TcpStream, file: &str) -> std::io::Result<()> {
    match crate::misc::mount::MESHES
        .iter()
        .chain(crate::misc::mount::CONTROLLER_MESHES)
        .find(|(name, _)| *name == file)
    {
        Some((_, bytes)) => send(sock, 200, "model/stl", bytes),
        None => send(sock, 404, "text/plain", b"no such mesh\n"),
    }
}

pub(super) fn speech(sock: &mut TcpStream, id: &str) -> std::io::Result<()> {
    match crate::misc::speech::wav(EPISODE_DIR, id) {
        Some(wav) => send(sock, 200, "audio/wav", &wav),
        None => send(sock, 404, "text/plain", b"not made yet"),
    }
}
