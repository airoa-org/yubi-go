//! The Setup tab's writes: `config.json`, `storage.yaml`, the gripper
//! sweep, the two resets, and the loopback-only start and stop.

use crate::hub::{Hub, EPISODE_DIR};

/// One edit to `config.json`, which the running rig then adopts.
pub(super) fn config(hub: &Hub, body: &[u8]) -> (u16, String) {
    let (status, reply) = config_edit(EPISODE_DIR, hub.recording(), &hub.cam_ports(), body);
    if status == 200 {
        adopt(hub, &crate::misc::config::Config::load(EPISODE_DIR));
        // Picked by eye from the live streams, so as sure as the motion check.
        let patch = serde_json::from_slice::<serde_json::Value>(body).ok();
        let picked = ["camera_roles", "camera_ports"]
            .iter()
            .any(|key| patch.as_ref().is_some_and(|p| p.get(key).is_some()));
        if picked {
            hub.set_cameras_matched(true);
        }
    }
    (status, reply)
}

pub(super) fn storage(hub: &Hub, body: &[u8]) -> (u16, String) {
    let saved = match hub.recording() {
        true => Err("recording — configuration is latched until the recording ends".to_string()),
        false => crate::hub::upload::save(EPISODE_DIR, body),
    };
    match saved {
        Ok(()) => {
            log::info!("storage.yaml written over http");
            (200, r#"{"saved": true}"#.to_string())
        }
        Err(why) => {
            log::warn!("storage edit refused: {why}");
            (400, serde_json::json!({"error": why}).to_string())
        }
    }
}

/// Start or stop the gripper sweep, and the prompt it shows now.
pub(super) fn calibrate(hub: &Hub) -> (u16, serde_json::Value) {
    // A sweep mid-take changes the calibration under a take that stored
    // the old one, and turns off the stop gesture while it runs.
    if hub.busy() {
        return (409, serde_json::json!({"error": "recording — stop the episode first"}));
    }
    hub.toggle_calibration();
    let (prompt, _, message) = hub.calibration_ui();
    let body = serde_json::json!({
        "prompt": prompt.map(|p| p.text()),
        "message": message.iter().map(|m| m.text()).collect::<Vec<_>>(),
    });
    (200, body)
}

/// Start or stop a take through the gate the gesture uses, and why it would
/// refuse a start now.
pub(super) fn record(hub: &Hub, on: bool) -> serde_json::Value {
    let ok = hub.set_recording(on);
    let reasons: Vec<String> =
        hub.ready().err().unwrap_or_default().iter().map(|r| r.text()).collect();
    serde_json::json!({ "ok": ok, "reasons": reasons })
}

/// What a factory reset removes: the settings, the upload target, the
/// backend's cached profile and assignment, the gripper and camera
/// calibrations, and the camera port ranks.
const FACTORY: [&str; 7] = [
    "config.json",
    "storage.yaml",
    "robot_me.json",
    "assigned.json",
    "calibration.json",
    crate::hub::camera_calib::DIR,
    "camera_ports.json",
];

/// The Setup tab's reset buttons. The body must say `{"confirm": "delete"}`, as
/// the page makes the person type it, so a stray request wipes nothing.
pub(super) fn reset(hub: &Hub, data: bool, body: &[u8]) -> (u16, String) {
    use serde_json::json;
    let refuse = |status: u16, why: &str| (status, json!({"error": why}).to_string());
    let confirmed =
        serde_json::from_slice::<serde_json::Value>(body).is_ok_and(|b| b["confirm"] == "delete");
    if !confirmed {
        return refuse(400, "type delete to confirm");
    }
    if hub.busy() {
        return refuse(409, "recording — stop the episode first");
    }
    // It reads the takes.
    if crate::hub::export::running() {
        return refuse(409, "an export is running — wait for it to finish");
    }
    let base = std::path::Path::new(EPISODE_DIR);
    let paths: Vec<std::path::PathBuf> = if data {
        [crate::hub::episodes::takes(EPISODE_DIR).into(), base.join(crate::hub::export::EXPORTS)]
            .iter()
            .flat_map(|dir| {
                std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path())
            })
            .collect()
    } else {
        FACTORY.iter().map(|name| base.join(name)).filter(|p| p.exists()).collect()
    };
    let mut failed = Vec::new();
    for path in &paths {
        let gone =
            if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        if let Err(e) = gone {
            log::warn!("reset: cannot remove {path:?}: {e}");
            failed.push(path.display().to_string());
        }
    }
    if !data {
        hub.reload_calibration();
        adopt(hub, &crate::misc::config::Config::load(EPISODE_DIR));
    }
    log::info!(
        "reset over http: {} {} removed",
        paths.len() - failed.len(),
        if data { "takes and exports" } else { "files" }
    );
    match failed.is_empty() {
        true => (200, json!({"removed": paths.len()}).to_string()),
        false => refuse(500, &format!("could not remove {}", failed.join(", "))),
    }
}

/// Make the running rig match the configuration just written, rather than at
/// the next launch: nothing else re-reads the camera assignment, and the gate
/// asks the rig's shape on the very next gesture.
fn adopt(hub: &Hub, config: &crate::misc::config::Config) {
    hub.adopt_cameras(config);
    hub.set_has_task(config.has_task());
    hub.set_head_camera(config.head_camera);
    hub.set_video(config.video);
}

/// The page picks cameras by the number they opened as (`camera_roles`),
/// which a replug reshuffles, so the choice is saved as the ports those
/// cameras sit in. A camera whose port is unknown keeps the number.
fn by_port(patch: &mut serde_json::Value, open: &crate::hub::Ports) {
    let Some(fields) = patch.as_object_mut() else { return };
    let Some(ranks) = fields.get("camera_roles").and_then(|r| r.as_array()) else { return };
    let ranks: Vec<usize> = ranks.iter().filter_map(|r| r.as_u64()).map(|r| r as usize).collect();
    let Some(roles) = crate::hub::Roles::from_ranks(&ranks) else { return };
    let ports = roles.ports(open);
    if ports.iter().any(Option::is_none) {
        return;
    }
    let ports: serde_json::Map<String, serde_json::Value> = crate::misc::config::CAMERA_KEYS
        .iter()
        .zip(ports)
        .map(|(key, port)| (key.to_string(), port.into()))
        .collect();
    fields.remove("camera_roles");
    fields.insert("camera_ports".into(), ports.into());
}

/// One edit from the page, or the reason it was refused in words that name the
/// fix. The field names are logged, never the values: an API key in logcat is
/// an API key on every laptop that ever ran `adb logcat` against this headset.
fn config_edit(dir: &str, recording: bool, open: &crate::hub::Ports, body: &[u8]) -> (u16, String) {
    use crate::misc::config::Config;
    let refuse = |status: u16, why: String| {
        log::warn!("config edit refused: {why}");
        (status, serde_json::json!({"error": why}).to_string())
    };
    // The running take snapshotted its config at start, so a write now could
    // not reach it, and "saved" would be untrue for that take.
    if recording {
        return refuse(409, "recording — configuration is latched until the recording ends".into());
    }
    let mut patch = match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(patch) => patch,
        Err(e) => return refuse(400, format!("not valid JSON: {e}")),
    };
    by_port(&mut patch, open);
    match Config::edit(dir, &patch) {
        Err(why) => return refuse(400, why),
        Ok(Err(e)) => return refuse(500, format!("cannot write config.json: {e}")),
        Ok(Ok(())) => {}
    }
    log::info!(
        "config written over http: {}",
        patch
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default()
    );
    (200, r#"{"saved": true}"#.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config.json that does not parse is refused rather than replaced by
    /// the one field the page sent.
    #[test]
    fn an_edit_over_a_broken_config_is_refused() {
        let dir =
            std::env::temp_dir().join(format!("yubigo_http_broken_config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.json"), r#"{"backend": {"#).unwrap();
        let patch = br#"{"video": "jpeg"}"#;
        let (status, body) = config_edit(dir.to_str().unwrap(), false, &Default::default(), patch);
        assert_eq!(status, 500);
        assert!(body.contains("not valid JSON"), "{body}");
        assert_eq!(std::fs::read_to_string(dir.join("config.json")).unwrap(), r#"{"backend": {"#);
    }

    /// The endpoint end to end against a real directory: a save, and the
    /// refusals a person will meet, in words that name the fix.
    #[test]
    fn an_edit_lands_on_disk_and_a_refusal_says_why() {
        let dir = std::env::temp_dir().join(format!("yubigo_http_config-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_str().unwrap();
        use crate::misc::config::Config;
        let none = Default::default();
        let config_edit = |dir, recording, body: &[u8]| config_edit(dir, recording, &none, body);

        let (status, body) = config_edit(
            dir,
            false,
            br#"{"identity": {"site": "trc"}, "tasks": ["pick up the cup"]}"#,
        );
        assert_eq!((status, body.as_str()), (200, r#"{"saved": true}"#));
        assert_eq!(Config::load(dir).identity.site, "trc");
        assert_eq!(Config::load(dir).task(), "pick up the cup");

        let (status, body) = config_edit(dir, true, br#"{"identity": {"site": "osaka"}}"#);
        assert_eq!(status, 409);
        assert!(body.contains("latched"), "{body}");
        assert_eq!(Config::load(dir).identity.site, "trc", "unchanged");

        let (status, body) = config_edit(dir, false, br#"{"mode": "production"}"#);
        assert_eq!(status, 400);
        assert!(body.contains("nothing to talk to"), "{body}");

        let (status, body) = config_edit(dir, false, b"not json at all");
        assert!(status == 400 && body.contains("not valid JSON"), "{body}");

        let (status, body) = config_edit(
            dir,
            false,
            br#"{"backend": {"base_url": "rig:8000/api", "api_key": "hunter2"}}"#,
        );
        assert_eq!(status, 400, "no scheme");
        assert!(!body.contains("hunter2"), "a refusal does not quote the key: {body}");
    }

    /// The page's camera numbers are saved as the ports those cameras sit in,
    /// so the choice survives the replug that renumbers them.
    #[test]
    fn a_camera_choice_from_the_page_is_saved_by_port() {
        let dir =
            std::env::temp_dir().join(format!("yubigo_http_camera_ports-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.to_str().unwrap();
        let open = ["1-1.4", "1-1.2", "1-1.3"].map(|p| Some(p.to_string()));

        let (status, _) = config_edit(dir, false, &open, br#"{"camera_roles": [1, 2, 0]}"#);
        assert_eq!(status, 200);
        let config = crate::misc::config::Config::load(dir);
        assert_eq!(config.camera_ports, Some(["1-1.2", "1-1.3", "1-1.4"].map(|p| Some(p.into()))));
        assert_eq!(config.camera_roles, None, "the port list replaces the rank list");

        // A camera whose port is unknown keeps the number it opened as.
        let open = [Some("1-1.4".to_string()), None, None];
        let (status, _) = config_edit(dir, false, &open, br#"{"camera_roles": [2, 0, 1]}"#);
        assert_eq!(status, 200);
        let config = crate::misc::config::Config::load(dir);
        assert_eq!(config.camera_roles.unwrap().ranks(), [2, 0, 1]);
        assert_eq!(config.camera_ports, None, "the newer choice wins");
    }
}
