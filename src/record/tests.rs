use super::*;

const NO_CAMERAS: [bool; 3] = [false; 3];

const JPEG: &[u8] = b"\xff\xd8\xff\xd9";

fn temp(tag: &str) -> String {
    // Every episode's meta.json comes from airoa_core, and `start` is
    // idempotent, so the one place every test already passes through is
    // where the payload gets brought up.
    let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("airoa-sdk/build");
    airoa_sdk::start(&payload)
        .expect("the payload and its cwasm must both be built — see the error");
    let base = std::env::temp_dir().join(format!("yubigo_rec_{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    base.to_str().unwrap().to_string()
}

fn queue(base: &str, task: &str) {
    fs::create_dir_all(base).unwrap();
    let row = crate::misc::config::Assignment {
        id: "ep-0".into(),
        task: task.into(),
        task_id: "task-0".into(),
        task_version_id: "ver-0".into(),
        subtasks: Vec::new(),
        spent: false,
    };
    crate::misc::config::Assignment::save_all(base, &[row]).unwrap();
}

fn signals(dir: &Path) -> Vec<(String, u64, Vec<u8>)> {
    crate::record::mcap::read_all(&dir.join(SIGNALS)).unwrap()
}

#[test]
fn an_episode_a_camera_never_reached_is_not_marked_complete() {
    let base = temp("no_video");
    let mut r = Recorder::start(&base, Default::default(), [true; 3], None).unwrap();
    let dir = r.dir.clone();

    r.pose(
        crate::misc::clock::boottime_ns(),
        Capture::default(),
        0,
        Located::tracked([1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0]),
    );
    r.gripper(crate::misc::clock::boottime_ns(), None, 0, 1.53, Some(0.25));
    assert!(!r.finish(true, None), "an episode with no frames must not be kept");
    assert!(!dir.exists(), "a take that did not close cleanly deletes itself");

    let mut r = Recorder::start(&base, Default::default(), [true; 3], None).unwrap();
    let dir = r.dir.clone();
    for slot in 0..4 {
        r.frame(crate::misc::clock::boottime_ns(), Capture::default(), slot, JPEG);
    }
    r.gripper(crate::misc::clock::boottime_ns(), None, 0, 1.53, Some(0.25));
    assert!(r.finish(true, None));
    assert!(dir.join("meta.json").exists());
}

#[test]
fn a_take_the_app_going_away_cut_short_is_thrown_away() {
    let base = temp("interrupted");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    for slot in 0..4 {
        r.frame(crate::misc::clock::boottime_ns(), Capture::default(), slot, JPEG);
    }
    r.pose(
        crate::misc::clock::boottime_ns(),
        Capture::default(),
        0,
        Located::tracked([1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0]),
    );
    r.gripper(crate::misc::clock::boottime_ns(), None, 0, 1.53, Some(0.25));

    assert!(!r.interrupted(), "an interrupted take must not be kept");
    assert!(!dir.exists(), "it deletes itself rather than waiting for a sweep");
    assert_eq!(sweep_incomplete(&base), 0, "nothing left to sweep");
}

#[test]
fn a_camera_that_owed_nothing_is_not_missed() {
    let base = temp("owed_nothing");
    let mut r = Recorder::start(&base, Default::default(), [true, false, false], None).unwrap();

    let dir = r.dir.clone();
    r.frame(crate::misc::clock::boottime_ns(), Capture::default(), 0, JPEG);
    assert!(r.finish(true, None));
    assert!(dir.join("meta.json").exists());
}

#[test]
fn records_the_signals_and_keeps_only_a_take_that_closed() {
    let base = temp("signals");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    let name = r.name.clone();

    r.gripper(crate::misc::clock::boottime_ns(), None, 1, 1.53, Some(0.25));
    r.frame(crate::misc::clock::boottime_ns(), Capture::default(), 0, JPEG);
    r.frame(crate::misc::clock::boottime_ns(), Capture::default(), 0, JPEG);
    r.pose(
        crate::misc::clock::boottime_ns(),
        Capture::default(),
        1,
        Located::tracked([1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0]),
    );
    r.pose(
        crate::misc::clock::boottime_ns(),
        Capture::default(),
        2,
        Located::inferred([0.0, 0.0, 1.5, 0.0, 0.0, 0.0, 1.0]),
    );
    assert!(r.finish(true, None));

    assert_eq!(list(&base), vec![name]);
    assert!(dir.join("meta.json").exists());

    let msgs = signals(&dir);
    let count = |topic: &str| msgs.iter().filter(|m| m.0 == topic).count();
    assert_eq!(count("/right_camera/image_raw/compressed"), 0, "slot 0 is the left camera");
    assert_eq!(count("/left_camera/image_raw/compressed"), 2);
    assert_eq!(count("/tf"), 2);
    assert_eq!(count("/joint_states"), 1);
    assert_eq!(count("/yubi/gripper/right/joint_state"), 1);
    assert_eq!(count("/quest/controller/right/tracked"), 1);
    assert_eq!(count("/quest/hmd/tracked"), 1);

    let tf = String::from_utf8_lossy(&msgs.iter().find(|m| m.0 == "/tf").unwrap().2).to_string();
    for want in ["right_hand_root", "right_left_finger_link"] {
        assert!(tf.contains(want), "/tf is missing {want}");
    }

    let statics =
        String::from_utf8_lossy(&msgs.iter().find(|m| m.0 == "/tf_static").unwrap().2).to_string();
    for want in ["quest_right_controller", "right_hand_cam_link", "right_left_finger_tip_link"] {
        assert!(statics.contains(want), "/tf_static is missing {want}");
    }

    let meta = fs::read_to_string(dir.join("meta.json")).unwrap();
    assert!(meta.contains("\"schema_version\": \"2.0\""));
    assert!(meta.contains("\"name\": \"signals.mcap\""), "{meta}");
}

#[test]
fn meta_json_pins_both_clocks_and_the_build() {
    let base = temp("anchor");
    let before = crate::misc::clock::realtime_ns();
    let r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    assert!(r.finish(true, None));

    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();

    let start_time = meta["episode"]["start_time"].as_f64().unwrap();
    assert!(
        (start_time - before as f64 / 1e9).abs() < 5.0,
        "episode.start_time must stay wall-clock epoch seconds, got {start_time}"
    );

    for role in ["interface", "data_collection"] {
        let ours = meta["programs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["role"] == role)
            .unwrap_or_else(|| panic!("no {role} row in {}", meta["programs"]));
        assert_eq!(
            ours["name"], "yubi-go",
            "no version: the git hash beside it is the identity, as in yubi"
        );
        assert_eq!(ours["source"]["git"]["hash"], env!("YUBIGO_GIT_HASH"));
    }
}

#[test]
fn meta_json_carries_a_software_manifest_beside_the_declared_program() {
    let base = with_head_camera("manifest");
    // A manifest that is not there is the normal case and must cost nothing.
    let meta = meta_of(&base, Default::default());
    let programs = meta["programs"].as_array().unwrap();

    let roles: Vec<&str> = programs.iter().map(|p| p["role"].as_str().unwrap()).collect();
    assert_eq!(roles.iter().filter(|r| **r == "data_collection").count(), 1, "{roles:?}");
    assert_eq!(roles.iter().filter(|r| **r == "interface").count(), 1, "{roles:?}");
    let discovered: Vec<&str> = programs
        .iter()
        .filter(|p| p["role"] == "discovered_software")
        .map(|p| p["name"].as_str().unwrap_or_default())
        .collect();
    let core = programs
        .iter()
        .find(|p| p["name"] == "airoa_core")
        .unwrap_or_else(|| panic!("the running payload answers for itself: {discovered:?}"));
    let hash = core["source"]["git"]["hash"].as_str().unwrap();
    assert!(hash.len() == 40 && hash.chars().all(|c| c.is_ascii_hexdigit()), "{core}");
    assert_eq!(core["source"]["git"]["uri"], "https://github.com/airoa-org/airoa_data_collection");
    // Discovery adds programs; it never turns devices into something else.
    assert_eq!(meta["devices"].as_array().unwrap().len(), 1, "nothing is plugged in on the host");

    // A manifest that is there must not break the episode either, even when
    // the guest cannot reach it (no preopen on the host).
    fs::write(
        Path::new(&base).join(crate::record::system_info::MANIFEST),
        "software:\n  - name: gripper_firmware\n    version: '1.0'\n",
    )
    .unwrap();
    let meta = meta_of(&base, Default::default());
    assert_eq!(meta["schema_version"], "2.0");
    assert!(meta["programs"].as_array().unwrap().iter().any(|p| p["role"] == "data_collection"));
}

#[test]
fn a_manifest_row_keeps_the_role_it_declares() {
    let dir = temp("manifest-role");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        Path::new(&dir).join("m.yaml"),
        "software:\n  - name: yubi-go\n    role: interface\n",
    )
    .unwrap();
    airoa_sdk::add_preopen(Path::new(&dir), "/manifest_probe", airoa_sdk::Access::ReadOnly);
    let env = [("GIT_URI", "https://example/yubi-go"), ("GIT_HASH", "abc"), ("GIT_BRANCH", "main")];
    let paths = airoa_sdk::system_info::ManifestPaths {
        combined: "/manifest_probe/m.yaml",
        ..Default::default()
    };
    let m = airoa_sdk::system_info::load_manifest(&paths, &env).unwrap();
    assert_eq!(m.software.len(), 1, "{m:?}");
    assert_eq!(m.software[0].role, "interface");
    assert_eq!(m.software[0].git_hash, "abc", "blank git fields inherit the build's");
}

#[test]
fn an_episode_records_the_calibration_its_angles_came_from() {
    let uncalibrated = meta_of(&temp("uncal"), Default::default());
    let devices = uncalibrated["devices"].as_array().unwrap();
    assert!(devices.is_empty(), "{devices:?}");

    let mut session = crate::misc::calib::Session::default();
    session.toggle([true; 2]);
    let mut now = 0;
    let feed = |session: &mut crate::misc::calib::Session, at: f64, ms: u64, now: &mut u64| {
        for _ in 0..(ms / 5) {
            for side in 0..2 {
                session.sample(side, "x", at, *now, 1_700_000_000_000_000_000);
            }
            *now += 5_000_000;
        }
    };
    feed(&mut session, 1.8, 200, &mut now);
    for i in 1..=10 {
        feed(&mut session, 1.8 - i as f64 * 0.08, 10, &mut now);
    }
    feed(&mut session, 1.0, 600, &mut now);
    for i in 1..=10 {
        feed(&mut session, 1.0 + i as f64 * 0.08, 10, &mut now);
    }
    feed(&mut session, 1.8, 1_200, &mut now);
    assert!(session.take_finished().is_some(), "the sweep must have completed");

    let meta = meta_of(&temp("cal"), session.active().clone());
    let devices = meta["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 2, "one entry per calibrated encoder");
    assert_eq!(devices[0]["role"], "gripper_left");
    assert_eq!(devices[0]["type"], "as5601_encoder");
    assert_eq!(devices[0]["id"], "x");
}

/// A folder whose config declares the headset camera, which is off by default.
fn with_head_camera(name: &str) -> String {
    let base = temp(name);
    fs::create_dir_all(&base).unwrap();
    fs::write(Path::new(&base).join("config.json"), r#"{"head_camera": true}"#).unwrap();
    base
}

fn meta_of(base: &str, calibration: crate::misc::calib::Calibration) -> serde_json::Value {
    let r = Recorder::start(base, calibration, NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    assert!(r.finish(true, None));
    serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap()
}

#[test]
fn a_frame_reaches_the_file_as_the_bytes_the_camera_gave() {
    let base = temp("verbatim");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    let jpeg = b"\xff\xd8not really a picture\xff\xd9";
    r.frame(1_000, Capture::default(), 0, jpeg);
    assert!(r.finish(true, None));

    let msgs = signals(&dir);
    let image = &msgs.iter().find(|m| m.0 == "/left_camera/image_raw/compressed").unwrap().2;
    assert!(
        image.windows(jpeg.len()).any(|w| w == jpeg),
        "the JPEG did not reach the message intact"
    );
}

#[test]
fn a_cameras_video_starts_at_its_first_key_frame() {
    let base = temp("video_keyed");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    r.video(1_000, Capture::default(), 0, b"delta before any key", false);
    r.video(2_000, Capture::default(), 0, b"key", true);
    r.video(3_000, Capture::default(), 0, b"delta", false);
    assert!(r.finish(true, None));

    let on = |topic: &str| signals(&dir).iter().filter(|m| m.0 == topic).count();
    assert_eq!(on("/left_camera/video"), 2);
    assert_eq!(on("/left_camera/uvc_frame"), 2);
    assert_eq!(on("/left_camera/image_raw/compressed"), 0);
}

#[test]
fn an_episode_says_the_head_video_came_from_a_usb_camera() {
    let meta = meta_of(&with_head_camera("head_camera_device"), Default::default());
    let devices = meta["devices"].as_array().unwrap();
    assert_eq!(devices.len(), 1, "no encoders calibrated here, one camera");
    assert_eq!(devices[0]["role"], "head_camera");
    assert_eq!(devices[0]["type"], "uvc_mjpeg");
    assert_eq!(devices[0]["id"], "head");
}

/// The stereo module's frames are two eyes side by side, so the episode says so;
/// and a rig with no headset camera claims none.
#[test]
fn the_head_camera_entry_says_stereo_and_is_absent_on_a_two_camera_rig() {
    let base = with_head_camera("head_stereo");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    r.set_head_stereo(true);
    let dir = r.dir.clone();
    assert!(r.finish(true, None));
    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["devices"][0]["type"], "uvc_mjpeg_stereo");

    let base = temp("no_head");
    fs::create_dir_all(&base).unwrap();
    fs::write(Path::new(&base).join("config.json"), r#"{"front_camera": false}"#).unwrap();
    let meta = meta_of(&base, Default::default());
    assert!(meta["devices"].as_array().unwrap().is_empty(), "{}", meta["devices"]);
}

#[test]
fn every_episode_gets_its_own_random_uuid() {
    let base = temp("uuid");
    let mut seen = std::collections::HashSet::new();
    for _ in 0..8 {
        let r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
        let dir = r.dir.clone();
        assert!(r.finish(true, None));
        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
        let uuid = meta["uuid"].as_str().unwrap().to_string();

        let groups: Vec<&str> = uuid.split('-').collect();
        assert_eq!(groups.iter().map(|g| g.len()).collect::<Vec<_>>(), [8, 4, 4, 4, 12], "{uuid}");
        assert!(uuid.chars().all(|c| c.is_ascii_hexdigit() || c == '-'), "{uuid}");
        assert!(groups[2].starts_with('4'), "not a v4: {uuid}");
        assert!("89ab".contains(&groups[3][..1]), "wrong variant: {uuid}");

        assert!(seen.insert(uuid.clone()), "uuid {uuid} was issued twice");
        fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn a_provisioned_config_becomes_the_episodes_identity() {
    let base = temp("provisioned");
    queue(&base, "pick up the cup");
    fs::write(
        Path::new(&base).join("config.json"),
        r#"{"identity": {"organization": "airoa", "site": "trc", "location": "room_a",
                             "robot_type": "YubiGo", "robot_id": "yubi-go-001",
                             "operator": "TarouTanaka"},
                "backend": {"base_url": "http://rig:8000/api", "api_key": "secret"}}"#,
    )
    .unwrap();

    let r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    assert!(r.finish(true, None));

    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["runner"]["organization"], "airoa");
    assert_eq!(meta["environment"]["site"], "trc");
    assert_eq!(meta["environment"]["location"], "room_a");
    assert_eq!(meta["robot"]["type"], "YubiGo");
    assert_eq!(meta["robot"]["id"], "yubi-go-001");
    assert_eq!(meta["runner"]["name"], "TarouTanaka");
    assert_eq!(meta["episode"]["label"], "pick up the cup");
    let raw = fs::read_to_string(dir.join("meta.json")).unwrap();
    assert!(!raw.contains("secret"), "the api key leaked into meta.json");
}

#[test]
fn the_segment_carries_the_instruction_and_the_episode_the_task() {
    let base = temp("instruction");
    queue(&base, "fold the towel");

    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    r.config.assignment = Some(crate::misc::config::Assignment {
        id: "ep-0".into(),
        task: "fold the towel".into(),
        task_id: "task-0".into(),
        task_version_id: "ver-0".into(),
        spent: false,
        subtasks: vec![crate::misc::config::Subtask {
            id: "sub-0".into(),
            name: "pick and place ECU".into(),
            status: 0,
        }],
    });
    assert!(r.finish(true, None));

    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["episode"]["label"], "fold the towel");
    assert_eq!(meta["labels"][0], "pick and place ECU");
    assert_eq!(meta["segments"][0]["label_idx"], 0);
}

#[test]
fn a_spent_assignment_keeps_its_labels_and_drops_its_episode_id() {
    let base = temp("spent");
    queue(&base, "fold the towel");

    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    r.config.assignment =
        Some(crate::misc::config::Assignment { spent: true, ..decomposed(&["uncap it", "write"]) });
    let now = crate::misc::clock::boottime_ns();
    r.advance(gesture(now, now));

    assert!(r.finish(true, None));

    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["episode"]["label"], "fold the towel");
    assert_eq!(meta["labels"][0], "uncap it");
    assert_eq!(meta["labels"][1], "write");
    assert_eq!(meta["segments"].as_array().unwrap().len(), 2);
}

#[test]
fn only_a_waiting_row_binds_a_take() {
    assert!(!binding(&Config::default()), "an empty queue owes a binding it cannot get");

    let assigned = Config { assignment: Some(decomposed(&["uncap it"])), ..Default::default() };
    assert!(binding(&assigned));

    let spent = crate::misc::config::Assignment { spent: true, ..decomposed(&["uncap it"]) };
    let spent = Config { assignment: Some(spent), ..Default::default() };
    assert!(!binding(&spent), "a row that already has its recording");
}

fn decomposed(names: &[&str]) -> crate::misc::config::Assignment {
    crate::misc::config::Assignment {
        id: "ep-0".into(),
        task: "fold the towel".into(),
        task_id: "task-0".into(),
        task_version_id: "ver-0".into(),
        spent: false,
        subtasks: names
            .iter()
            .enumerate()
            .map(|(i, name)| crate::misc::config::Subtask {
                id: format!("sub-{i}"),
                name: (*name).to_string(),
                status: 0,
            })
            .collect(),
    }
}

fn started(base: &str) -> Recorder {
    queue(base, "fold the towel");
    Recorder::start(base, Default::default(), NO_CAMERAS, None).unwrap()
}

fn written_meta(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap()
}

fn gesture(task_end_ns: u64, task_start_ns: u64) -> crate::record::click::Gesture {
    crate::record::click::Gesture {
        side: 0,
        started_ns: task_end_ns + 40_000_000,
        ended_ns: task_start_ns + 40_000_000,
        task_end_ns,
        task_start_ns,
        held: false,
    }
}

fn seam_at(r: &Recorder, offset_ns: u64) -> crate::record::click::Gesture {
    gesture(r.started.boottime_ns + offset_ns, r.started.boottime_ns + offset_ns + 250_000_000)
}

fn backdate(r: &mut Recorder, sec: u64) {
    r.started.boottime_ns -= sec * 1_000_000_000;
    r.started.realtime_ns -= sec * 1_000_000_000;
}

#[test]
fn each_advance_cuts_a_segment_named_for_the_step_it_ends() {
    let base = temp("advance");
    let mut r = started(&base);
    let dir = r.dir.clone();
    r.config.assignment = Some(decomposed(&["uncap the marker", "write AIRoA", "recap it"]));
    backdate(&mut r, 8);
    let t0 = r.started.boottime_ns;

    r.advance(seam_at(&r, 1_000_000_000));
    r.advance(seam_at(&r, 3_000_000_000));
    assert!(r.finish(true, Some(gesture(t0 + 5_000_000_000, t0 + 5_250_000_000))));

    let meta = written_meta(&dir);
    assert_eq!(meta["episode"]["label"], "fold the towel");
    let labels = meta["labels"].as_array().unwrap();
    assert_eq!(labels, &["uncap the marker", "write AIRoA", "recap it"]);
    let segments = meta["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 3, "two seams cut three segments");
    for (i, segment) in segments.iter().enumerate() {
        assert_eq!(segment["label_idx"], i, "label_idx indexes labels in order");
    }
    let at = |i: usize, k: &str| segments[i][k].as_f64().unwrap();
    let start = meta["episode"]["start_time"].as_f64().unwrap();
    assert!((at(0, "end_time") - (start + 1.0)).abs() < 1e-6, "step one ends at the onset");
    assert!((at(1, "start_time") - (start + 1.25)).abs() < 1e-6, "step two begins after it");
    assert!((at(1, "end_time") - (start + 3.0)).abs() < 1e-6);
    assert!((at(2, "start_time") - (start + 3.25)).abs() < 1e-6);
    for i in 0..3 {
        assert!(at(i, "start_time") < at(i, "end_time"), "segment {i} runs backwards");
    }
    assert!(at(0, "end_time") < at(1, "start_time"));
    assert!(at(1, "end_time") < at(2, "start_time"));
    assert!(
        at(2, "end_time") < meta["episode"]["end_time"].as_f64().unwrap(),
        "the stopping gesture must fall outside the last segment"
    );
    assert!((at(2, "end_time") - (start + 5.0)).abs() < 1e-6);
}

#[test]
fn a_step_a_hold_started_begins_at_its_first_motion() {
    let base = temp("onset");
    let mut r = started(&base);
    let dir = r.dir.clone();
    r.config.assignment = Some(decomposed(&["pick", "place", "wipe"]));
    backdate(&mut r, 8);
    let t0 = r.started.boottime_ns;
    let hand = |linear: f64| Located {
        velocity: crate::misc::pose::Velocity {
            linear: Some([linear, 0.0, 0.0]),
            angular: Some([0.0; 3]),
        },
        ..Located::tracked([0.0; 7])
    };

    r.advance(crate::record::click::Gesture { held: true, ..seam_at(&r, 1_000_000_000) });
    for (offset_ms, linear) in [(1300, 0.0), (1600, 0.01), (2000, 0.2), (2100, 0.2)] {
        r.pose(t0 + offset_ms * 1_000_000, Capture::default(), 1, hand(linear));
    }
    r.advance(seam_at(&r, 3_000_000_000));
    for (offset_ms, linear) in [(3400, 0.0), (4000, 0.2)] {
        r.pose(t0 + offset_ms * 1_000_000, Capture::default(), 1, hand(linear));
    }
    assert!(r.finish(true, Some(gesture(t0 + 5_000_000_000, t0 + 5_250_000_000))));

    let meta = written_meta(&dir);
    let segments = meta["segments"].as_array().unwrap();
    let start = meta["episode"]["start_time"].as_f64().unwrap();
    let at = |i: usize| segments[i]["start_time"].as_f64().unwrap() - start;
    assert!((at(1) - 2.0).abs() < 1e-6, "the frozen 0.75 s is left out: {}", at(1));
    assert!(
        (at(2) - 3.25).abs() < 1e-6,
        "a double-click's step starts as the jaw opens: {}",
        at(2)
    );
}

#[test]
fn the_declared_start_covers_the_rows_that_precede_it() {
    let base = temp("preroll-start");
    let mut r = started(&base);
    let dir = r.dir.clone();
    let opened = r.started.boottime_ns;
    let anchor = r.started.realtime_ns as f64 / 1e9;
    r.gripper(opened - 300_000_000, None, 0, 0.4, Some(0.4));
    r.gripper(opened + 10_000_000, None, 0, 0.4, Some(0.4));
    assert!(r.finish(true, None));

    let meta = written_meta(&dir);
    let start = meta["episode"]["start_time"].as_f64().unwrap();
    assert!(
        (start - (anchor - 0.3)).abs() < 1e-6,
        "start_time {start} should reach back to the oldest row, not to {anchor}"
    );
    assert!(anchor > start);
}

#[test]
fn stopping_early_writes_only_the_steps_that_were_performed() {
    let base = temp("early");
    let mut r = started(&base);
    let dir = r.dir.clone();
    r.config.assignment = Some(decomposed(&["one", "two", "three", "four"]));
    backdate(&mut r, 4);
    r.advance(seam_at(&r, 1_000_000_000));

    assert!(r.finish(true, None));

    let meta = written_meta(&dir);
    assert_eq!(meta["labels"].as_array().unwrap(), &["one", "two"]);
    assert_eq!(meta["segments"].as_array().unwrap().len(), 2);
}

#[test]
fn a_failure_condemns_the_last_segment_and_leaves_the_earlier_ones_alone() {
    let base = temp("failed-step");
    let mut r = started(&base);
    let dir = r.dir.clone();
    r.config.assignment = Some(decomposed(&["one", "two", "three"]));
    backdate(&mut r, 4);
    r.advance(seam_at(&r, 1_000_000_000));
    r.advance(seam_at(&r, 2_000_000_000));

    assert!(r.finish(false, None));

    let meta = written_meta(&dir);
    assert_eq!(meta["episode"]["success"], false);
    let segments = meta["segments"].as_array().unwrap();
    assert_eq!(segments[0]["success"], true);
    assert_eq!(segments[1]["success"], true);
    assert_eq!(segments[2]["success"], false, "the step they were on when it went wrong");
}

#[test]
fn a_decomposed_task_never_advanced_is_the_first_step_alone() {
    let base = temp("decomposed");
    let mut r = started(&base);
    let dir = r.dir.clone();
    r.config.assignment = Some(decomposed(&["pick and place ECU", "close the lid"]));
    assert!(r.finish(true, None));

    let meta = written_meta(&dir);
    assert_eq!(meta["labels"].as_array().unwrap(), &["pick and place ECU"]);
    assert_eq!(meta["segments"].as_array().unwrap().len(), 1);
    assert_eq!(meta["episode"]["label"], "fold the towel");
}

#[test]
fn an_unprovisioned_device_records_a_valid_episode() {
    let base = temp("unprovisioned");
    let r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    assert!(r.finish(true, None));

    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["episode"]["label"], "untitled");
    assert_eq!(meta["runner"]["organization"], "");
    assert_eq!(meta["environment"]["site"], "");
    assert_eq!(meta["robot"]["type"], "YubiGo");
    assert_eq!(meta["uuid"].as_str().unwrap().len(), 36);
}

#[test]
fn identity_text_is_escaped_rather_than_pasted() {
    let base = temp("escaping");
    fs::create_dir_all(&base).unwrap();
    fs::write(
        Path::new(&base).join("config.json"),
        r#"{"identity": {"site": "the \"back\" room\\lab"}}"#,
    )
    .unwrap();

    let r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    assert!(r.finish(true, None));

    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["environment"]["site"], "the \"back\" room\\lab");
}

#[test]
fn an_episode_nobody_vouched_for_is_not_a_success() {
    let base = temp("success");
    let r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    assert!(r.finish(false, None));

    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    assert_eq!(meta["episode"]["success"], false);
}

#[test]
fn an_undecomposed_episode_is_one_segment_spanning_itself() {
    let base = temp("segments");
    queue(&base, "pick up the cup");

    let r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    assert!(r.finish(true, None));

    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    let ep = &meta["episode"];
    let segments = meta["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0]["start_time"], ep["start_time"]);
    assert_eq!(segments[0]["end_time"], ep["end_time"]);
    assert_eq!(segments[0]["success"], ep["success"]);

    let labels = meta["labels"].as_array().unwrap();
    let idx = segments[0]["label_idx"].as_u64().unwrap() as usize;
    assert_eq!(labels.get(idx), Some(&ep["label"]));
    assert_eq!(ep["label"], "pick up the cup");
}

#[test]
fn a_capture_time_is_recorded_when_the_camera_offers_one() {
    let base = temp("capture");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    r.frame(1_000, Capture { at_ns: Some(900), ..Default::default() }, 2, JPEG);
    r.frame(2_000, Capture { seq: Some(7), ..Default::default() }, 0, JPEG);
    assert!(r.finish(true, None));

    let data = fs::read(dir.join(SIGNALS)).unwrap();
    let stamps: Vec<(u64, u64, u32)> = mcap::MessageStream::new(&data)
        .unwrap()
        .filter_map(|m| m.ok())
        .filter(|m| m.channel.topic.ends_with("/compressed"))
        .map(|m| (m.log_time, m.publish_time, m.sequence))
        .collect();
    let first = stamps[0].0 as i128;
    let shape: Vec<(i128, i128, u32)> =
        stamps.iter().map(|&(l, p, s)| (l as i128 - first, p as i128 - first, s)).collect();
    assert_eq!(shape, [(0, -100, 0), (1_000, 1_000, 7)]);
}

#[test]
fn poses_record_whether_they_were_observed_or_extrapolated() {
    let base = temp("pose_tracked");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    for (part, tracked) in [(0, true), (1, false), (2, true)] {
        let seen = if tracked { Located::tracked([0.0; 7]) } else { Located::inferred([0.0; 7]) };
        r.pose(crate::misc::clock::boottime_ns(), Capture::default(), part, seen);
    }
    assert!(r.finish(true, None));

    let flag = |topic: &str| {
        let msgs = signals(&dir);
        let m = msgs.iter().find(|m| m.0 == topic).unwrap_or_else(|| panic!("no {topic}"));
        m.2[4] != 0
    };
    assert!(flag("/quest/controller/left/tracked"));
    assert!(!flag("/quest/controller/right/tracked"));
    assert!(flag("/quest/hmd/tracked"));
}

#[test]
fn poses_from_one_tick_share_a_capture_time() {
    let base = temp("pose_capture");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    for part in 0..3 {
        r.pose(
            2_000 + part as u64,
            Capture { at_ns: Some(1_500), ..Default::default() },
            part,
            Located::tracked([0.0; 7]),
        );
    }
    assert!(r.finish(true, None));

    let data = fs::read(dir.join(SIGNALS)).unwrap();
    let published: Vec<u64> = mcap::MessageStream::new(&data)
        .unwrap()
        .filter_map(|m| m.ok())
        .filter(|m| m.channel.topic == "/tf")
        .map(|m| m.publish_time)
        .collect();
    assert_eq!(published.len(), 3);
    assert!(published.windows(2).all(|w| w[0] == w[1]), "one tick, one capture instant");
}

#[test]
fn capture_times_running_backwards_fail_the_episode() {
    let base = temp("capture_backwards");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    r.frame(1_000, Capture { at_ns: Some(9_000), ..Default::default() }, 2, JPEG);
    r.frame(2_000, Capture { at_ns: Some(8_000), ..Default::default() }, 2, JPEG);
    assert!(!r.finish(true, None), "a backwards capture timeline must cost the marker");
}

#[test]
fn a_source_running_backwards_fails_the_episode() {
    let base = temp("backwards");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    r.gripper(2_000, None, 0, 0.1, None);
    r.gripper(1_000, None, 0, 0.2, None);
    assert!(!r.finish(true, None));
    assert!(!dir.exists(), "a take whose signals ran backwards deletes itself");
}

#[test]
fn a_rig_that_keeps_faulted_takes_keeps_one_that_ran_backwards() {
    let base = temp("backwards_kept");
    fs::create_dir_all(&base).unwrap();
    fs::write(Path::new(&base).join("config.json"), r#"{"keep_faulted_episodes": true}"#).unwrap();
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    r.gripper(2_000, None, 0, 0.1, None);
    r.gripper(1_000, None, 0, 0.2, None);
    assert!(r.finish(true, None));
    assert!(is_complete(&dir));

    // Any other close fault still costs the take.
    let mut r = Recorder::start(&base, Default::default(), [true, false, false], None).unwrap();
    let dir = r.dir.clone();
    r.gripper(2_000, None, 0, 0.1, None);
    r.gripper(1_000, None, 0, 0.2, None);
    assert!(!r.finish(true, None), "a camera that owed frames and sent none");
    assert!(!dir.exists());
}

#[test]
fn a_take_whose_marker_cannot_be_written_is_kept_unmarked() {
    let base = temp("marker_fails");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    r.gripper(1_000, None, 0, 0.1, None);
    // A directory where the temporary file wants to go makes the write fail.
    fs::create_dir(dir.join(".meta.json.tmp")).unwrap();
    assert!(!r.finish(true, None), "no marker, so not complete");
    assert!(!is_complete(&dir));
    assert!(
        signals(&dir).iter().any(|m| m.0.contains("gripper")),
        "the closed mcap is still there"
    );
}

#[test]
fn cameras_interleaving_out_of_order_is_not_a_violation() {
    let base = temp("interleave");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    r.frame(2_000, Capture::default(), 0, JPEG);
    r.frame(1_000, Capture::default(), 1, JPEG);
    r.frame(3_000, Capture::default(), 0, JPEG);
    r.frame(2_500, Capture::default(), 1, JPEG);
    assert!(r.finish(true, None), "cross-camera interleave must not fail the episode");
}

#[test]
fn sweep_deletes_only_the_unmarked() {
    let base = temp("sweep");
    let good = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let good_name = good.name.clone();
    assert!(good.finish(true, None));

    std::thread::sleep(std::time::Duration::from_millis(2));
    drop(Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap());

    assert_eq!(sweep_incomplete(&base), 1);
    assert_eq!(list(&base), vec![good_name]);
    assert_eq!(sweep_incomplete(&base), 0, "sweeping twice must be a no-op");
}

#[test]
fn a_fault_opening_and_closing_mid_take_lands_in_the_episode() {
    use crate::record::gate::Fault;
    let base = temp("faults");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    let t = crate::misc::clock::boottime_ns();
    r.frame(t, Capture::default(), 0, JPEG);
    r.fault(t, "left wrist cam", Fault::Starved, true);
    r.fault(t + 1_000, "left wrist cam", Fault::Starved, false);
    assert!(r.finish(true, None));

    let events: Vec<_> = signals(&dir).into_iter().filter(|m| m.0 == "/yubi_go/events").collect();
    assert_eq!(events.len(), 2, "one event as it opened, one as it closed");
    let text = |m: &(String, u64, Vec<u8>)| String::from_utf8_lossy(&m.2).to_string();
    assert!(text(&events[0]).contains("fault/left wrist cam") && text(&events[0]).contains("open"));
    assert!(text(&events[1]).contains("closed"));
    assert!(events[1].1 > events[0].1, "each on the take's own clock");
}

#[test]
fn messages_the_writer_dropped_are_counted_in_the_episode() {
    let base = temp("dropped");
    let mut r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    let dir = r.dir.clone();
    let t = crate::misc::clock::boottime_ns();
    r.frame(t, Capture::default(), 0, JPEG);
    r.dropped(t, 7);
    assert!(r.finish(true, None));

    let events: Vec<_> = signals(&dir).into_iter().filter(|m| m.0 == "/yubi_go/events").collect();
    let text = String::from_utf8_lossy(&events[0].2).to_string();
    assert!(text.contains("writer/dropped") && text.contains("7 messages"), "{text}");
}

#[test]
fn a_folder_is_names_between_slashes_and_nothing_that_climbs_out() {
    assert_eq!(clean_folder(" cups / day 1/ ").unwrap(), "cups/day 1");
    assert_eq!(clean_folder("//").unwrap(), "");
    assert_eq!(clean_folder("カップ/1日目").unwrap(), "カップ/1日目");
    assert!(clean_folder("cups/../etc").is_err());
    assert!(clean_folder("a\nb").is_err());
    assert!(clean_folder(&"x/".repeat(9)).is_err());
}

#[test]
fn a_take_is_filed_under_the_configured_folder() {
    let base = temp("folder");
    fs::create_dir_all(&base).unwrap();
    Config::save(&base, &serde_json::json!({ "folder": "cups/ day1" })).unwrap();
    let r = Recorder::start(&base, Default::default(), NO_CAMERAS, None).unwrap();
    assert_eq!(folder_of(&r.dir), "cups/day1");
    set_folder(&r.dir, "").unwrap();
    assert_eq!(folder_of(&r.dir), "", "the root has no file");
    assert!(!r.dir.join(FOLDER).exists());
}

#[test]
fn a_failed_try_and_its_retry_are_separate_segments_and_the_reset_is_neither() {
    let base = temp("retry");
    let mut r = started(&base);
    let dir = r.dir.clone();
    r.config.assignment = Some(decomposed(&["uncap the marker", "write AIRoA"]));
    backdate(&mut r, 8);
    let t0 = r.started.boottime_ns;

    r.advance(seam_at(&r, 1_000_000_000));
    r.fail(t0 + 3_000_000_000);
    r.resume(seam_at(&r, 5_000_000_000));
    assert!(r.finish(true, Some(gesture(t0 + 7_000_000_000, t0 + 7_250_000_000))));

    let meta = written_meta(&dir);
    assert_eq!(meta["labels"], serde_json::json!(["uncap the marker", "write AIRoA"]));
    let segments = meta["segments"].as_array().unwrap();
    let field = |k: &str| segments.iter().map(|s| s[k].clone()).collect::<Vec<_>>();
    assert_eq!(field("label_idx"), [0, 1, 1]);
    assert_eq!(field("success"), [true, false, true]);
    let at = |i: usize, k: &str| segments[i][k].as_f64().unwrap();
    let start = meta["episode"]["start_time"].as_f64().unwrap();
    assert!((at(1, "end_time") - (start + 3.0)).abs() < 1e-6, "the try ends at the B press");
    assert!(
        (at(2, "start_time") - (start + 5.25)).abs() < 1e-6,
        "the retry begins after the click"
    );
    assert!((at(2, "end_time") - (start + 7.0)).abs() < 1e-6);
}

#[test]
fn a_take_ended_while_paused_keeps_only_its_closed_tries() {
    let base = temp("paused-end");
    let mut r = started(&base);
    let dir = r.dir.clone();
    r.config.assignment = Some(decomposed(&["uncap the marker", "write AIRoA"]));
    backdate(&mut r, 4);

    r.fail(r.started.boottime_ns + 2_000_000_000);
    assert!(r.finish(false, None));

    let meta = written_meta(&dir);
    assert_eq!(meta["labels"], serde_json::json!(["uncap the marker"]));
    let segments = meta["segments"].as_array().unwrap();
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0]["success"], false);
    assert_eq!(meta["episode"]["success"], false);
}

#[test]
fn a_task_without_subtasks_labels_every_try_with_the_task() {
    let base = temp("retry-whole");
    let mut r = started(&base);
    let dir = r.dir.clone();
    backdate(&mut r, 6);

    r.fail(r.started.boottime_ns + 2_000_000_000);
    r.resume(seam_at(&r, 3_000_000_000));
    assert!(r.finish(true, None));

    let meta = written_meta(&dir);
    assert_eq!(meta["labels"], serde_json::json!(["fold the towel"]));
    let segments = meta["segments"].as_array().unwrap();
    assert_eq!(segments.iter().map(|s| s["label_idx"].clone()).collect::<Vec<_>>(), [0, 0]);
    assert_eq!(segments.iter().map(|s| s["success"].clone()).collect::<Vec<_>>(), [false, true]);
}
