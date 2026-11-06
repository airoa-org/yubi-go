//! The `:8080` side interface: MJPEG for a monitoring client, a page that
//! answers "is this rig all right?" without lifting the headset back onto a
//! face, and the setup a standalone rig has no dashboard for — its identity,
//! task list and camera roles, and its recorded episodes.
//!
//! **Anyone on the rig network may look and change.** The QR code points here,
//! and a PIN on every phone cost more than it protected on a rig network. Only
//! starting a take over HTTP is held back: it is for `adb forward` bench work,
//! so it answers loopback alone.
//!
//! **A person, not a web page.** Other sites' pages open on a phone on the
//! same network are refused any change ([`request::cross_site`]) and any
//! read through a name that rebinds to the headset ([`foreign_host`]), and only
//! `/stream` lets their scripts read a response, as web_video_server's MJPEG
//! does.
//!
//! Nothing here is on the recording path. A headset nobody has opened this
//! page against records exactly the same episodes.

mod assets;
mod episodes;
mod exports;
mod live;
mod request;
mod response;
mod route;
mod setup;
mod state;

pub use assets::LICENSES;
pub(crate) use request::foreign_host;

use std::net::TcpStream;

use crate::hub::{Hub, EPISODE_DIR};

use request::{cross_site, read_request};
use response::{send, send_json};
use route::{route, Route};

const HTML: &str = "text/html; charset=utf-8";
const PYTHON: &str = "text/x-python; charset=utf-8";

pub(crate) fn handle(hub: &Hub, mut sock: TcpStream) -> std::io::Result<()> {
    sock.set_nodelay(true)?;
    let req = read_request(&sock)?;
    if foreign_host(req.host.as_deref()) {
        log::warn!("refused {} {} for host {:?}", req.method, req.path, req.host);
        return send(&mut sock, 403, "text/plain", b"unknown host\n");
    }
    if cross_site(&req) {
        log::warn!("refused {} {} from {:?}", req.method, req.path, req.origin);
        return send(&mut sock, 403, "text/plain", b"cross-site request refused\n");
    }
    // `adb forward` and the device's own screen already imply the device is
    // yours. Canonical because the listener is dual-stack: IPv4 loopback
    // arrives as `::ffff:127.0.0.1`, which is not `is_loopback()`.
    let local = sock.peer_addr().is_ok_and(|a| a.ip().to_canonical().is_loopback());
    let takes = crate::hub::episodes::takes(EPISODE_DIR);
    let range = req.range.as_deref();
    let body = &req.body;
    let s = &mut sock;
    match route(&req) {
        Route::Page => send(s, 200, HTML, assets::PAGE.as_bytes()),
        Route::Viewer => send(s, 200, HTML, assets::VIEWER.as_bytes()),
        Route::Pull => send(s, 200, PYTHON, assets::PULL.as_bytes()),
        Route::RerunView => send(s, 200, PYTHON, assets::RERUN_VIEW.as_bytes()),
        Route::Licenses => assets::licenses(s),
        Route::Mesh(file) => assets::mesh(s, &file),
        Route::Speech(id) => assets::speech(s, &id),
        Route::Stream(slot) => live::serve_mjpeg(hub, sock, slot),
        Route::Live(slot, fps) => live::serve_live(hub, sock, slot, fps),
        Route::LivePoses => live::serve_live_poses(hub, sock),
        Route::State => send_json(s, 200, &state::state(hub)),
        Route::Episodes => send_json(s, 200, &episodes::list(&takes)),
        Route::File(name, file) => episodes::serve_file(s, &takes, &name, &file, range),
        Route::Tar(name) => episodes::serve_tar(s, &takes, &name),
        Route::Tars(names, file) => episodes::serve_tars(s, &takes, &names, &file),
        Route::View(name) => episodes::view(s, &takes, &name),
        Route::Subtasks(name) => episodes::subtasks(hub, s, &takes, &name),
        Route::Video(name, slot) => episodes::video(s, &takes, &name, slot, range),
        Route::Jpeg(name, slot, i) => episodes::jpeg(s, &takes, &name, slot, i),
        Route::Folder(name) => {
            let (status, reply) = episodes::set_folder(&takes, &name, body);
            send_json(s, status, &reply.to_string())
        }
        Route::Delete(name) => {
            let (status, reply) = episodes::delete_take(&takes, &name);
            send(s, status, "application/json", reply)
        }
        Route::Exports => send_json(s, 200, &crate::hub::export::list(EPISODE_DIR).to_string()),
        Route::Export => {
            let (status, reply) = exports::start_export(hub, &takes, body);
            send_json(s, status, &reply.to_string())
        }
        Route::ExportTar(id) => exports::serve_tar(s, &id),
        Route::DeleteExport(id) => exports::delete(s, &id),
        Route::CameraCalibrations => {
            send_json(s, 200, &crate::hub::camera_calib::status(EPISODE_DIR).to_string())
        }
        Route::CameraCalibration(id) => {
            send_json(s, 200, &crate::hub::camera_calib::load(EPISODE_DIR, &id).to_string())
        }
        Route::Config => {
            let (status, reply) = setup::config(hub, body);
            send_json(s, status, &reply)
        }
        Route::Storage => {
            let (status, reply) = setup::storage(hub, body);
            send_json(s, status, &reply)
        }
        Route::Calibrate => {
            let (status, reply) = setup::calibrate(hub);
            send_json(s, status, &reply.to_string())
        }
        Route::ResetConfig => {
            let (status, reply) = setup::reset(hub, false, body);
            send_json(s, status, &reply)
        }
        Route::DeleteData => {
            let (status, reply) = setup::reset(hub, true, body);
            send_json(s, status, &reply)
        }
        Route::Record(_) if !local => send(s, 403, "text/plain", b"loopback only\n"),
        Route::Record(on) => send_json(s, 200, &setup::record(hub, on).to_string()),
        Route::NotFound => send(s, 404, "text/plain", b"not found\n"),
    }
}
