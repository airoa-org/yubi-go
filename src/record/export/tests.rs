use std::path::{Path, PathBuf};

use parquet::file::reader::{FileReader, SerializedFileReader};

use super::*;
use crate::misc::capture::Capture;
use crate::misc::pose::Located;
use crate::record::mcap::SignalsMcap;

/// Records which frames each row asked for, by file, and writes no video.
#[derive(Default)]
struct Picks(std::sync::Mutex<Vec<(PathBuf, usize, Vec<usize>)>>);

impl Video for Picks {
    fn write(&self, _: &Index, slot: usize, picks: &[usize], out: &Path) -> anyhow::Result<Rgb> {
        std::fs::write(out, b"")?;
        self.0.lock().unwrap().push((out.to_path_buf(), slot, picks.to_vec()));
        Ok(Rgb { frames: picks.len() as u64, pixels: 1, ..Rgb::default() })
    }
}

const T0: u64 = 1_793_004_310_000_000_000;

fn at(t_ns: u64) -> Capture {
    Capture { at_ns: Some(t_ns), ..Capture::default() }
}

/// A 2 s take: hands and headset at 100 Hz, grippers at 200 Hz, the left wrist
/// camera at 30 Hz, and two subtasks splitting it at 1 s.
fn take(name: &str) -> PathBuf {
    take_losing(name, None)
}

/// [`take`], with the right hand untracked over `lost` ns after its start.
fn take_losing(name: &str, lost: Option<std::ops::Range<u64>>) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("yubigo_export_{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("take")).unwrap();
    let take = dir.join("take");
    let mut out = SignalsMcap::create(&take.join("signals.mcap"), [false; 3]).unwrap();
    for k in 0..400u64 {
        let t = T0 + k * 5_000_000;
        out.gripper(t, None, (k % 2) as usize, 1.0, Some(0.1 + k as f64 * 1e-3)).unwrap();
        if k % 2 == 0 {
            for part in 0..3 {
                let x = part as f64 + k as f64 * 1e-3;
                let pose = [x, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];
                let seen = match &lost {
                    Some(lost) if part == 1 && lost.contains(&(t - T0)) => Located::inferred(pose),
                    _ => Located::tracked(pose),
                };
                out.pose(t, at(t), part, seen).unwrap();
            }
        }
    }
    for k in 0..60u64 {
        let t = T0 + 3_000_000 + k * 33_000_000;
        out.frame(t, at(t), 0, b"\xff\xd8\xff\xd9").unwrap();
    }
    out.close().unwrap();
    let s = |ns: u64| ns as f64 / 1e9;
    let meta = json!({
        "uuid": "4b6ffdd0-f3a1-4c18-a60d-b4143b5de999",
        "robot": {"type": "sample-robot", "id": "rig-1"},
        "files": [{"type": "mcap", "name": "signals.mcap"}],
        "environment": {"site": "sample-site"},
        "programs": [{"role": "data_collection", "name": "yubi-go 0.1.0",
                      "source": {"git": {"hash": "abc", "branch": "main"}}}],
        "episode": {"start_time": s(T0), "end_time": s(T0 + 2_000_000_000),
                    "success": true, "label": "stack the cups"},
        "labels": ["pick", "place"],
        "segments": [
            {"start_time": s(T0 + 100_000_000), "end_time": s(T0 + 1_000_000_000), "label_idx": 0, "success": true},
            {"start_time": s(T0 + 1_000_000_000), "end_time": s(T0 + 1_900_000_000), "label_idx": 1, "success": false},
        ],
        "trigger": {"usable": {}},
    });
    std::fs::write(take.join("meta.json"), meta.to_string()).unwrap();
    take
}

fn lines(path: &Path) -> Vec<Value> {
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn a_take_exports_one_episode_per_subtask() {
    let take = take("subtasks");
    let out = take.parent().unwrap().join("lerobot");
    let video = Picks::default();
    let summary =
        export(std::slice::from_ref(&take), &out, Leave::default(), &video, &mut |_| {}).unwrap();
    // In episode order: the clips are written several at once.
    let mut video = video.0.into_inner().unwrap();
    video.sort();
    assert_eq!(summary.episodes, 2);

    let info: Value =
        serde_json::from_slice(&std::fs::read(out.join("meta/info.json")).unwrap()).unwrap();
    assert_eq!(info["codebase_version"], "v2.1");
    assert_eq!(info["robot_type"], "sample-robot");
    assert_eq!(info["total_episodes"], 2);
    assert_eq!(info["total_tasks"], 2, "the labels the frames carry, not the take's own");
    assert_eq!(info["total_videos"], 2, "the one camera, twice");
    let features = info["features"].as_object().unwrap();
    for (key, dtype, shape) in [
        ("observation.pose.left_hand_root.absolute", "float64", 7),
        ("observation.pose.left_hand_root_to_right_hand_root.absolute", "float64", 7),
        ("observation.pose.left_hand_to_right_hand.relative", "float64", 7),
        ("action.pose.right_hand_root.relative", "float64", 7),
        ("observation.joint_states", "float64", 2),
        ("action.joint_states", "float64", 2),
        ("observation.image.left", "video", 480),
        ("observation.image.left.is_fresh", "bool", 1),
        ("timestamp", "float64", 1),
        ("task_index", "int64", 1),
    ] {
        assert_eq!(features[key]["dtype"], dtype, "{key}");
        assert_eq!(features[key]["shape"][0], shape, "{key}");
    }
    assert_eq!(features["observation.state"]["shape"][0], 11);
    assert_eq!(features["action"]["shape"][0], 20);
    assert_eq!(
        features.len(),
        63 - 4 + 2 - 1 + 1,
        "AIRoA's internal pipeline's three-camera set less two videos and their flags, plus the state and \
         action, less short_horizon_task_index, plus synched_timestamp_ns"
    );
    assert_eq!(features["synched_timestamp_ns"]["dtype"], "int64", "listed, for LeRobot v3.0");

    let tasks = lines(&out.join("meta/tasks.jsonl"));
    let names: Vec<&str> = tasks.iter().map(|t| t["task"].as_str().unwrap()).collect();
    assert_eq!(names, ["pick", "place"]);

    let episodes = lines(&out.join("meta/episodes.jsonl"));
    assert_eq!(episodes[1]["episode_id"], "4b6ffdd0-f3a1-4c18-a60d-b4143b5de999:1");
    assert_eq!(episodes[1]["tasks"], json!(["place"]));
    assert_eq!(episodes[1]["short_horizon_task"], json!(["stack the cups"]));
    assert_eq!(episodes[1]["task_success"], false);
    assert_eq!(
        episodes[0]["interface_git_hash"], "abc",
        "the recorder stands in for the interface"
    );
    assert!(episodes[0]["metadata"].get("trigger").is_none());

    let rows: Vec<i64> = (0..2)
        .map(|e| {
            let file =
                std::fs::File::open(out.join(format!("data/chunk-000/episode_{e:06}.parquet")));
            SerializedFileReader::new(file.unwrap()).unwrap().metadata().file_metadata().num_rows()
        })
        .collect();
    assert_eq!(rows.iter().sum::<i64>(), info["total_frames"].as_i64().unwrap());
    // 0.1..1.0 s and 1.0..1.9 s of ticks: 27 each, give or take where the grid starts.
    for (e, n) in rows.iter().enumerate() {
        assert!((26..=28).contains(n), "episode {e}: {n} rows");
        assert_eq!(episodes[e]["length"], *n);
        assert_eq!(video[e].2.len(), *n as usize, "one picture per row");
    }
    let picks = &video[0].2;
    assert!(picks.windows(2).all(|w| w[0] <= w[1]), "frames never go backwards");

    let stats = lines(&out.join("meta/episodes_stats.jsonl"));
    assert_eq!(stats[0]["stats"]["observation.image.left"]["count"], json!([rows[0]]));
    assert_eq!(stats[0]["stats"]["next.done"]["max"], json!([true]));
    assert_eq!(
        stats[1]["stats"]["index"]["min"],
        json!([rows[0] as f64]),
        "index runs on across episodes"
    );
    assert_eq!(stats[1]["stats"]["frame_index"]["min"], json!([0.0]));
}

/// The state ([`super::STATE`]) is the right hand root in the left one's frame, and its
/// action is both hand roots and the next tick's fingers, every rotation as
/// the first two columns of its matrix.
#[test]
fn state_and_action_hold_the_hand_roots_as_rot6d() {
    use glam::{DMat3, DQuat, DVec3};
    let n = 4;
    let pose = |p: DVec3, q: DQuat| [p.x, p.y, p.z, q.x, q.y, q.z, q.w];
    let left: Vec<(DVec3, DQuat)> = (0..n)
        .map(|k| {
            (
                DVec3::new(0.1 * k as f64, -0.2, 0.9),
                DQuat::from_euler(glam::EulerRot::XYZ, 0.3, -0.5 + 0.1 * k as f64, 1.1),
            )
        })
        .collect();
    let right: Vec<(DVec3, DQuat)> = (0..n)
        .map(|k| {
            (
                DVec3::new(-0.3, 0.05 * k as f64, 1.0),
                DQuat::from_euler(glam::EulerRot::ZYX, -0.7, 0.2, 0.4 * k as f64),
            )
        })
        .collect();
    let track = |name, absolute: Vec<[f64; 7]>| signals::Track {
        name,
        relative: vec![[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0]; n],
        fresh: vec![true; n],
        absolute,
    };
    let abs = |v: &[(DVec3, DQuat)]| v.iter().map(|&(p, q)| pose(p, q)).collect::<Vec<_>>();
    let inter = left
        .iter()
        .zip(&right)
        .map(|(&(pl, ql), &(pr, qr))| pose(ql.inverse() * (pr - pl), ql.inverse() * qr))
        .collect();
    let grid = signals::Grid {
        t_ns: (0..n as u64).map(|k| k * 33_333_333).collect(),
        tracks: vec![
            track("left_controller", abs(&left)),
            track("right_controller", abs(&right)),
            track("left_hand_root", abs(&left)),
            track("right_hand_root", abs(&right)),
            track("left_hand_root_to_right_hand_root", inter),
        ],
        joints: (0..n).map(|k| [0.1 * k as f64, 0.5 - 0.1 * k as f64]).collect(),
        joints_fresh: vec![true; n],
        frames: [None, None, None],
        warnings: vec![],
    };
    let segment = Segment { start_ns: 0, end_ns: u64::MAX, label_idx: 0, success: true };
    let cols = columns(&grid, 1..n, 0, 0, &segment);
    let get = |name: &str| match &cols.iter().find(|c| c.name == name).unwrap().values {
        Values::List(w, v) => v.chunks(*w).map(<[f64]>::to_vec).collect::<Vec<_>>(),
        _ => panic!("{name} is not a vector column"),
    };
    let (state, action) = (get("observation.state"), get("action"));
    assert_eq!((state.len(), action.len()), (n - 1, n - 1));
    let decode = |v: &[f64]| {
        let (a, b) = (DVec3::from_slice(&v[3..6]), DVec3::from_slice(&v[6..9]));
        (DVec3::from_slice(&v[..3]), DMat3::from_cols(a, b, a.cross(b)))
    };
    for (row, k) in (1..n).enumerate() {
        let (pl, rl) = decode(&action[row][..9]);
        let (pr, rr) = decode(&action[row][9..18]);
        assert!((pl - left[k].0).length() < 1e-12 && (pr - right[k].0).length() < 1e-12);
        assert!(rl.abs_diff_eq(DMat3::from_quat(left[k].1), 1e-12), "row {row}: left rotation");
        assert!(rr.abs_diff_eq(DMat3::from_quat(right[k].1), 1e-12), "row {row}: right rotation");
        let (p, r) = decode(&state[row]);
        assert!((p - rl.transpose() * (pr - pl)).length() < 1e-12, "row {row}: state position");
        assert!(r.abs_diff_eq(rl.transpose() * rr, 1e-12), "row {row}: state rotation");
        assert_eq!(state[row][9..], grid.joints[k], "the fingers now");
        let next = grid.joints[(k + 1).min(n - 1)];
        assert_eq!(action[row][18..], next, "the fingers at the next tick, as action.joint_states");
    }
}

#[test]
fn a_take_without_subtasks_is_one_episode() {
    let take = take("whole");
    let meta_path = take.join("meta.json");
    let mut meta: Value = serde_json::from_slice(&std::fs::read(&meta_path).unwrap()).unwrap();
    meta["segments"] = json!([]);
    meta["labels"] = json!([]);
    std::fs::write(&meta_path, meta.to_string()).unwrap();
    let out = take.parent().unwrap().join("lerobot");
    let summary =
        export(std::slice::from_ref(&take), &out, Leave::default(), &Picks::default(), &mut |_| {})
            .unwrap();
    assert_eq!(summary.episodes, 1);
    let tasks = lines(&out.join("meta/tasks.jsonl"));
    assert_eq!(tasks, [json!({"task_index": 0, "task": "stack the cups"})]);
    let episodes = lines(&out.join("meta/episodes.jsonl"));
    assert_eq!(episodes[0]["short_horizon_task"], json!([]));
}

#[test]
fn a_take_with_an_uncalibrated_gripper_is_refused() {
    let dir =
        std::env::temp_dir().join(format!("yubigo_export_uncalibrated-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut out = SignalsMcap::create(&dir.join("signals.mcap"), [false; 3]).unwrap();
    out.gripper(T0, None, 0, 1.0, Some(0.2)).unwrap();
    out.pose(T0, at(T0), 0, Located::tracked([0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0])).unwrap();
    out.close().unwrap();
    std::fs::write(dir.join("meta.json"), json!({"segments": []}).to_string()).unwrap();
    let err = export(
        std::slice::from_ref(&dir),
        &dir.join("out"),
        Leave::default(),
        &Picks::default(),
        &mut |_| {},
    )
    .err()
    .unwrap();
    assert!(err.to_string().contains("no samples"), "{err}");
}

fn relabel(take: &Path, labels: Value, label: &str) {
    let path = take.join("meta.json");
    let mut meta: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    meta["labels"] = labels;
    meta["episode"]["label"] = label.into();
    meta["uuid"] = format!("uuid-{label}").into();
    std::fs::write(&path, meta.to_string()).unwrap();
}

#[test]
fn takes_bundle_into_one_dataset_with_one_task_list() {
    let (a, b) = (take("bundle_a"), take("bundle_b"));
    relabel(&b, json!(["place", "wipe"]), "wipe the table");
    let out = a.parent().unwrap().join("lerobot");
    let summary = export(&[a, b], &out, Leave::default(), &Picks::default(), &mut |_| {}).unwrap();
    assert_eq!(summary.episodes, 4);

    let names: Vec<String> = lines(&out.join("meta/tasks.jsonl"))
        .iter()
        .map(|t| t["task"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, ["pick", "place", "wipe"]);
    let episodes = lines(&out.join("meta/episodes.jsonl"));
    assert_eq!(episodes[2]["episode_id"], "uuid-wipe the table:0");
    assert_eq!(episodes[3]["tasks"], json!(["wipe"]));
    assert_eq!(episodes[3]["short_horizon_task"], json!(["wipe the table"]));

    let stats = lines(&out.join("meta/episodes_stats.jsonl"));
    let task = |e: usize| stats[e]["stats"]["task_index"]["min"][0].as_f64().unwrap();
    assert_eq!((task(2), task(3)), (1.0, 2.0), "the second take's labels, numbered globally");
    let first_index = |e: usize| stats[e]["stats"]["index"]["min"][0].as_f64().unwrap();
    let length = |e: usize| episodes[e]["length"].as_f64().unwrap();
    assert_eq!(first_index(3), first_index(2) + length(2), "index runs on across takes");
    let info: Value =
        serde_json::from_slice(&std::fs::read(out.join("meta/info.json")).unwrap()).unwrap();
    assert_eq!(info["total_episodes"], 4);
    assert_eq!(info["total_tasks"], 3);
}

#[test]
fn takes_with_different_cameras_are_refused_together() {
    let a = take("cams_a");
    let b = std::env::temp_dir().join(format!("yubigo_export_cams_b-{}/take", std::process::id()));
    let _ = std::fs::remove_dir_all(b.parent().unwrap());
    std::fs::create_dir_all(&b).unwrap();
    std::fs::copy(a.join("meta.json"), b.join("meta.json")).unwrap();
    let mut out = SignalsMcap::create(&b.join("signals.mcap"), [false; 3]).unwrap();
    for k in 0..400u64 {
        let t = T0 + k * 5_000_000;
        out.gripper(t, None, (k % 2) as usize, 1.0, Some(0.2)).unwrap();
        for part in 0..3 {
            out.pose(t, at(t), part, Located::tracked([0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]))
                .unwrap();
        }
        if k % 7 == 0 {
            out.frame(t, at(t), 1, b"\xff\xd8\xff\xd9").unwrap();
        }
    }
    out.close().unwrap();
    let dest = a.parent().unwrap().join("lerobot");
    let err =
        export(&[a, b], &dest, Leave::default(), &Picks::default(), &mut |_| {}).err().unwrap();
    assert!(err.to_string().contains("has cameras"), "{err}");
}

#[test]
fn skip_failed_leaves_out_the_failed_tries_and_keeps_the_rest_of_the_take() {
    let take = take("skip-failed");
    let out = take.parent().unwrap().join("lerobot");
    let summary = export(
        std::slice::from_ref(&take),
        &out,
        Leave { failed: true, ..Leave::default() },
        &Picks::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(summary.episodes, 1, "the failed place is left out");
    let episodes = lines(&out.join("meta/episodes.jsonl"));
    assert_eq!(episodes.len(), 1);
    assert_eq!(episodes[0]["tasks"], json!(["pick"]));
    assert_eq!(episodes[0]["task_success"], true);
}

#[test]
fn skip_untracked_leaves_out_a_subtask_that_lost_a_hand() {
    let take = take_losing("skip-untracked", Some(1_300_000_000..1_500_000_000));
    let out = take.parent().unwrap().join("lerobot");
    let leave = Leave { untracked: true, ..Leave::default() };
    let summary =
        export(std::slice::from_ref(&take), &out, leave, &Picks::default(), &mut |_| {}).unwrap();
    assert_eq!(summary.episodes, 1, "place lost the right hand");
    assert_eq!(lines(&out.join("meta/episodes.jsonl"))[0]["tasks"], json!(["pick"]));
    assert!(
        summary.warnings.iter().any(|w| w.contains("right hand lost tracking")),
        "{:?}",
        summary.warnings
    );

    let kept = take.parent().unwrap().join("lerobot-kept");
    let summary = export(
        std::slice::from_ref(&take),
        &kept,
        Leave::default(),
        &Picks::default(),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(summary.episodes, 2, "off, the subtask stays");
}

#[test]
fn the_dialog_counts_what_each_option_leaves_out() {
    let take = take_losing("subtask-flags", Some(1_300_000_000..1_500_000_000));
    assert_eq!(
        subtasks(&take).unwrap(),
        [(false, false), (true, true)],
        "place failed and lost a hand"
    );
}
