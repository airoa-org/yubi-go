//! Which handler a request goes to, decided from its method and path alone,
//! and the allowlists that keep a path inside the episode directory.

use super::live::LIVE_FPS_RECORDING;
use super::request::Request;

/// What a request resolves to, before anything touches the socket.
///
/// Split out from [`super::handle`] so routing is testable without a TCP connection.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// The side interface itself.
    Page,
    /// MJPEG, by camera slot — what a monitoring client watches.
    Stream(usize),
    /// Device state as JSON, for the page to poll.
    State,
    /// Every finished episode, with its files.
    Episodes,
    /// One file of one episode, by range if asked.
    File(String, String),
    /// One episode as a tar, so a browser saves it as one file.
    Tar(String),
    /// Several episodes as one tar, from `?episodes=a,b`, and the file to save it as.
    Tars(Vec<String>, String),
    /// What the viewer plays from: frame times and the gripper traces.
    View(String),
    /// Each subtask of an episode: failed, and lost tracking.
    Subtasks(String),
    /// One H.264 camera of an episode as an mp4, by range.
    Video(String, usize),
    /// One JPEG frame of an episode: camera slot and frame number.
    Jpeg(String, usize, usize),
    /// The script that copies episodes to a computer.
    Pull,
    RerunView,
    /// The episode viewer.
    Viewer,
    /// Every licence text the APK carries.
    Licenses,
    /// One camera as MJPEG at up to the frame rate asked for, for the
    /// viewer's live view.
    Live(usize, u32),
    /// The poses and gripper angles as they are now, as server-sent events.
    LivePoses,
    /// One of the meshes the app draws, for the viewer's 3D view.
    Mesh(String),
    /// A spoken task or subtask name, by `misc::speech::id`.
    Speech(String),
    /// File an episode under a folder.
    Folder(String),
    /// The LeRobot datasets made here, and the one being made.
    Exports,
    /// Start making one from the episodes named in the body.
    Export,
    /// One dataset as a tar.
    ExportTar(String),
    DeleteExport(String),
    /// Every saved camera calibration.
    CameraCalibrations,
    /// One gripper pair's camera calibration, or `latest`; the built-in
    /// default when there is none.
    CameraCalibration(String),
    Delete(String),
    Config,
    /// The upload target the Setup tab edits: `storage.yaml`.
    Storage,
    Calibrate,
    /// Setup's reset buttons: back to factory settings, or every take and
    /// export gone.
    ResetConfig,
    DeleteData,
    /// Start or stop a take, through the same gate as the gesture: bench
    /// testing over `adb forward`, answered for loopback alone.
    Record(bool),
    NotFound,
}

pub(super) fn route(req: &Request) -> Route {
    let path = req.path.split('?').next().unwrap_or("/");
    let query = req.path.split_once('?').map(|(_, q)| q).unwrap_or("");
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    match (req.method.as_str(), segments.as_slice()) {
        ("GET", []) => Route::Page,
        // web_video_server compatibility: /stream?topic=/left_camera/...
        //
        // An unrecognised topic lands on the head slot rather than 404ing,
        // because web_video_server-style `/stream?topic=` URLs rely on that.
        ("GET", ["stream"]) => Route::Stream(if query.contains("left") {
            0
        } else if query.contains("right") {
            1
        } else {
            crate::hub::SLOT_HEAD
        }),
        ("GET", ["state"]) => Route::State,
        ("GET", ["episodes"]) => Route::Episodes,
        ("GET", ["pull.py"]) => Route::Pull,
        ("GET", ["rerun_view.py"]) => Route::RerunView,
        ("GET", ["viewer"]) => Route::Viewer,
        ("GET", ["licenses"]) => Route::Licenses,
        ("GET", ["live", "poses"]) => Route::LivePoses,
        ("GET", ["live", file]) => match file.strip_suffix(".mjpeg").and_then(camera) {
            Some(slot) => Route::Live(
                slot,
                query
                    .split('&')
                    .find_map(|kv| kv.strip_prefix("fps=")?.parse().ok())
                    .unwrap_or(LIVE_FPS_RECORDING),
            ),
            None => Route::NotFound,
        },
        ("GET", ["meshes", file]) => Route::Mesh(file.to_string()),
        ("GET", ["speech", file]) => match file.strip_suffix(".wav") {
            Some(id) => Route::Speech(id.to_string()),
            None => Route::NotFound,
        },
        ("GET", ["episodes.tar"]) => {
            let mut takes = Vec::new();
            let mut name = "episodes".to_string();
            for (key, value) in query.split('&').filter_map(|kv| kv.split_once('=')) {
                match key {
                    "episodes" => takes.extend(value.split(',').map(str::to_string)),
                    "name"
                        if !value.is_empty()
                            && value
                                .chars()
                                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)) =>
                    {
                        name = value.to_string()
                    }
                    _ => {}
                }
            }
            match !takes.is_empty() && takes.iter().all(|t| safe_episode(t)) {
                true => Route::Tars(takes, format!("{name}.tar")),
                false => Route::NotFound,
            }
        }
        ("GET", ["episodes", tar]) if tar.strip_suffix(".tar").is_some_and(safe_episode) => {
            Route::Tar(tar.trim_end_matches(".tar").to_string())
        }
        ("GET", ["episodes", name, "view"]) if safe_episode(name) => Route::View(name.to_string()),
        ("GET", ["episodes", name, "subtasks"]) if safe_episode(name) => {
            Route::Subtasks(name.to_string())
        }
        ("GET", ["episodes", name, "view", file]) if safe_episode(name) => {
            match file.strip_suffix(".mp4").and_then(camera) {
                Some(slot) => Route::Video(name.to_string(), slot),
                None => Route::NotFound,
            }
        }
        ("GET", ["episodes", name, "view", cam, frame]) if safe_episode(name) => {
            match (camera(cam), frame.strip_suffix(".jpg").and_then(|f| f.parse().ok())) {
                (Some(slot), Some(i)) => Route::Jpeg(name.to_string(), slot, i),
                _ => Route::NotFound,
            }
        }
        ("POST", ["episodes", name, "folder"]) if safe_episode(name) => {
            Route::Folder(name.to_string())
        }
        ("GET", ["exports"]) => Route::Exports,
        ("POST", ["exports"]) => Route::Export,
        ("GET", ["camera-calibration"]) => Route::CameraCalibrations,
        ("GET", ["camera-calibration", id]) if crate::hub::camera_calib::safe_id(id) => {
            Route::CameraCalibration(id.to_string())
        }
        ("GET", ["exports", tar]) => match tar.strip_suffix(".tar") {
            Some(id) if crate::hub::export::safe_id(id) => Route::ExportTar(id.into()),
            _ => Route::NotFound,
        },
        ("DELETE", ["exports", id]) if crate::hub::export::safe_id(id) => {
            Route::DeleteExport(id.to_string())
        }
        ("GET", ["episodes", name, file]) if safe_episode(name) && safe_file(file) => {
            Route::File(name.to_string(), file.to_string())
        }
        ("DELETE", ["episodes", name]) if safe_episode(name) => Route::Delete(name.to_string()),
        ("POST", ["config"]) => Route::Config,
        ("POST", ["storage"]) => Route::Storage,
        ("POST", ["calibrate"]) => Route::Calibrate,
        ("POST", ["reset", "config"]) => Route::ResetConfig,
        ("POST", ["reset", "data"]) => Route::DeleteData,
        ("POST", ["record", "start"]) => Route::Record(true),
        ("POST", ["record", "stop"]) => Route::Record(false),
        _ => Route::NotFound,
    }
}

/// The name of an episode directory and nothing else: an allowlist of the one
/// shape the recorder produces, which cannot be argued past the way a `..`
/// search can.
pub(super) fn safe_episode(name: &str) -> bool {
    name.starts_with("episode_")
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn camera(name: &str) -> Option<usize> {
    crate::hub::episodes::CAM_NAMES.iter().position(|c| *c == name)
}

/// A plain file name inside an episode; `.` and `..` are not.
fn safe_file(file: &str) -> bool {
    !file.is_empty()
        && file.len() <= 64
        && file != "."
        && file != ".."
        && file.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::http::request::parse_request;

    fn req(method: &str, path: &str) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            range: None,
            origin: None,
            host: None,
            body: vec![],
        }
    }

    #[test]
    fn routes_the_surfaces_apart() {
        assert_eq!(route(&req("GET", "/")), Route::Page);
        assert_eq!(route(&req("GET", "/state")), Route::State);
    }

    /// web_video_server-style `/stream?topic=` URLs, which this server may
    /// not break.
    #[test]
    fn the_existing_mjpeg_urls_still_resolve() {
        assert_eq!(route(&req("GET", "/stream?topic=/left_camera/image_raw")), Route::Stream(0));
        assert_eq!(route(&req("GET", "/stream?topic=/right_camera/image_raw")), Route::Stream(1));
        assert_eq!(
            route(&req("GET", "/stream?topic=/head_camera/image_raw")),
            Route::Stream(crate::hub::SLOT_HEAD)
        );
    }

    /// Writing config is never a GET a link can make.
    #[test]
    fn a_change_is_never_a_get() {
        assert_eq!(route(&req("GET", "/config")), Route::NotFound);
        assert_eq!(route(&req("DELETE", "/state")), Route::NotFound);
    }

    #[test]
    fn a_path_may_not_escape_the_episode_directory() {
        for path in [
            "/episodes/../../etc/passwd",
            "/episodes/episode_1/../../../etc/passwd",
            "/episodes/episode_1/..",
            "/episodes/episode_1/.",
            "/episodes/config.json/x",
            "/episodes/episode_1/sub/dir",
            "/../etc/passwd",
        ] {
            assert_eq!(route(&req("GET", path)), Route::NotFound, "{path}");
        }
        assert_eq!(route(&req("DELETE", "/episodes/..")), Route::NotFound);
        assert_eq!(route(&req("DELETE", "/episodes/config.json")), Route::NotFound);
    }

    #[test]
    fn the_viewer_and_the_pull_routes_resolve() {
        let ep = "episode_1792585604624";
        assert_eq!(route(&req("GET", &format!("/episodes/{ep}.tar"))), Route::Tar(ep.into()));
        assert_eq!(
            route(&req("GET", &format!("/episodes.tar?episodes={ep},{ep}&name=cups"))),
            Route::Tars(vec![ep.into(), ep.into()], "cups.tar".into())
        );
        assert_eq!(
            route(&req("GET", &format!("/episodes.tar?episodes={ep}&name=a%22b"))),
            Route::Tars(vec![ep.into()], "episodes.tar".into()),
            "a name that is not plain keeps the default"
        );
        for bad in ["/episodes.tar", "/episodes.tar?episodes=", "/episodes.tar?episodes=../x"] {
            assert_eq!(route(&req("GET", bad)), Route::NotFound, "{bad}");
        }
        assert_eq!(route(&req("GET", &format!("/episodes/{ep}/view"))), Route::View(ep.into()));
        assert_eq!(
            route(&req("GET", &format!("/episodes/{ep}/subtasks"))),
            Route::Subtasks(ep.into())
        );
        assert_eq!(
            route(&req("GET", &format!("/episodes/{ep}/view/right.mp4"))),
            Route::Video(ep.into(), 1)
        );
        assert_eq!(
            route(&req("GET", &format!("/episodes/{ep}/view/head/12.jpg"))),
            Route::Jpeg(ep.into(), 2, 12)
        );
        let folder = format!("/episodes/{ep}/folder");
        assert_eq!(route(&req("POST", &folder)), Route::Folder(ep.into()));
        assert_eq!(route(&req("GET", "/exports")), Route::Exports);
        assert_eq!(
            route(&req("GET", "/camera-calibration/PL028-PR028")),
            Route::CameraCalibration("PL028-PR028".into())
        );
        assert_eq!(route(&req("GET", "/camera-calibration/..%2Fx")), Route::NotFound);
        assert_eq!(route(&req("POST", "/exports")), Route::Export);
        assert_eq!(route(&req("POST", "/storage")), Route::Storage);
        assert_eq!(route(&req("GET", "/exports/cups-2.tar")), Route::ExportTar("cups-2".into()));
        assert_eq!(route(&req("GET", "/exports/..%2Fx.tar")), Route::NotFound);
        assert_eq!(route(&req("DELETE", "/exports/cups")), Route::DeleteExport("cups".into()));
        assert_eq!(route(&req("GET", "/pull.py")), Route::Pull);
        assert_eq!(route(&req("GET", "/rerun_view.py")), Route::RerunView);
        assert_eq!(route(&req("GET", "/viewer")), Route::Viewer);
        assert_eq!(route(&req("GET", "/licenses")), Route::Licenses);
        assert_eq!(route(&req("GET", "/speech/0123abcd.wav")), Route::Speech("0123abcd".into()));
        assert_eq!(route(&req("GET", "/speech/x.mp3")), Route::NotFound);
        assert_eq!(route(&req("GET", "/live/poses")), Route::LivePoses);
        assert_eq!(route(&req("GET", "/live/head.mjpeg?fps=30")), Route::Live(2, 30));
        assert_eq!(route(&req("GET", "/live/left.mjpeg")), Route::Live(0, LIVE_FPS_RECORDING));
        assert_eq!(route(&req("GET", "/live/wrist.mjpeg")), Route::NotFound);
        assert_eq!(route(&req("GET", "/meshes/left.stl")), Route::Mesh("left.stl".into()));
        for path in [
            "/episodes/../x.tar",
            "/episodes/config.tar",
            &format!("/episodes/{ep}/view/wrist.mp4"),
            &format!("/episodes/{ep}/view/left/-1.jpg"),
            &format!("/episodes/{ep}/view/left/x.jpg"),
        ] {
            assert_eq!(route(&req("GET", path)), Route::NotFound, "{path}");
        }
        let raw = "GET /episodes/x HTTP/1.1\r\nRange: bytes=10-\r\n\r\n";
        let req = parse_request(&mut std::io::BufReader::new(raw.as_bytes())).unwrap();
        assert_eq!(req.range.as_deref(), Some("bytes=10-"));
    }
}
