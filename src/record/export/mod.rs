//! A take as a LeRobot v2.1 dataset, in the shape AIRoA's data pipeline exports
//! through rebake: the same features under the same names, one episode per subtask,
//! and the LeRobot v2.1 meta files (`docs/lerobot-export.md`). rebake and
//! `umi_data_packaging` are AIRoA's internal converters and are not public, so the
//! comments that name them record what this export matches, not where to look.

pub mod job;
pub mod picture;
pub mod signals;
pub mod table;
#[cfg(target_os = "android")]
pub mod transcode;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::record::playback::Index;
use signals::{Grid, FPS};
use table::{Column, Rgb, Values};

/// The feature each camera slot becomes. The head camera takes the centre
/// slot, as rebake places it.
pub const CAMERAS: [&str; 3] = ["left", "right", "center"];

/// Videos written at once: two episodes' three cameras. That keeps the app's
/// three cores full: 3 takes ran at 270% of a core in
/// 44 s, where one episode at a time ran at 155% in 45-49 s (Quest 3S,
/// 2026-10-30).
const CLIPS_AT_ONCE: usize = 6;

/// LeRobot's episodes per chunk directory.
const CHUNK: usize = 1000;

const POSE: [&str; 7] = ["x", "y", "z", "qx", "qy", "qz", "qw"];
const STEP: [&str; 7] = ["dx", "dy", "dz", "dqx", "dqy", "dqz", "dqw"];
const JOINTS: [&str; 2] = ["left_right_finger_joint", "right_right_finger_joint"];

/// `observation.state` and `action`, one state and one action column for a
/// trainer that reads a single vector of each. A pose is xyz then the first
/// two columns of its rotation matrix.
const STATE: [&str; 11] =
    ["x", "y", "z", "r00", "r10", "r20", "r01", "r11", "r21", "left_finger", "right_finger"];
const ACTION: [&str; 20] = [
    "left_x",
    "left_y",
    "left_z",
    "left_r00",
    "left_r10",
    "left_r20",
    "left_r01",
    "left_r11",
    "left_r21",
    "right_x",
    "right_y",
    "right_z",
    "right_r00",
    "right_r10",
    "right_r20",
    "right_r01",
    "right_r11",
    "right_r21",
    "left_finger",
    "right_finger",
];

/// The keys of airoa-metadata v2.0, which is what rebake keeps of `meta.json`
/// in each episode's `metadata`.
const METADATA_KEYS: [&str; 12] = [
    "$schema",
    "schema_version",
    "uuid",
    "robot",
    "files",
    "environment",
    "runner",
    "devices",
    "programs",
    "episode",
    "labels",
    "segments",
];

/// Writes one camera's chosen frames as an episode's mp4. Several episodes'
/// cameras are written at once, each on its own thread ([`CLIPS_AT_ONCE`]).
pub trait Video: Sync {
    /// `picks` is the recorded frame to show on each row, in row order. The file is H.264
    /// at [`picture::WIDTH`]×[`picture::HEIGHT`] with frame `i` at `i / 30` s.
    fn write(&self, index: &Index, slot: usize, picks: &[usize], out: &Path)
        -> anyhow::Result<Rgb>;
}

#[derive(Clone, Copy)]
struct Segment {
    start_ns: u64,
    end_ns: u64,
    label_idx: usize,
    success: bool,
}

/// One camera of one episode, written once every table is.
struct Clip {
    take: usize,
    episode: usize,
    slot: usize,
    camera: &'static str,
    picks: Vec<usize>,
    file: PathBuf,
}

/// Writes every clip, [`CLIPS_AT_ONCE`] at a time, and answers each one's
/// colour stats in `clips` order. The head camera's go first, because it
/// takes about twice as long a frame as a wrist and the last clip left
/// running sets when the export ends.
fn clips(
    clips: &[Clip],
    indexes: &[Index],
    video: &dyn Video,
    progress: &mut (dyn FnMut(String) + Send),
) -> anyhow::Result<Vec<Rgb>> {
    let cost = |c: &Clip| c.picks.len() * if c.camera == "center" { 2 } else { 1 };
    let mut order: Vec<usize> = (0..clips.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(cost(&clips[i])));
    let taken = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let written: Mutex<Vec<Option<anyhow::Result<Rgb>>>> =
        Mutex::new(clips.iter().map(|_| None).collect());
    let progress = Mutex::new(progress);
    let done = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..CLIPS_AT_ONCE.min(clips.len()) {
            s.spawn(|| {
                while !failed.load(Ordering::Relaxed) {
                    let Some(&i) = order.get(taken.fetch_add(1, Ordering::Relaxed)) else {
                        return;
                    };
                    let c = &clips[i];
                    let rgb = video.write(&indexes[c.take], c.slot, &c.picks, &c.file);
                    failed.fetch_or(rgb.is_err(), Ordering::Relaxed);
                    written.lock().unwrap()[i] = Some(rgb);
                    let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                    (*progress.lock().unwrap())(format!("videos: {n} of {}", clips.len()));
                }
            });
        }
    });
    let written = written.into_inner().unwrap();
    if failed.into_inner() {
        return Err(written.into_iter().flatten().find_map(Result::err).unwrap());
    }
    Ok(written.into_iter().map(|w| w.expect("every clip ran").unwrap()).collect())
}

pub struct Summary {
    pub episodes: usize,
    pub frames: usize,
    pub warnings: Vec<String>,
}

fn ns(seconds: &Value) -> Option<u64> {
    Some((seconds.as_f64()? * 1e9) as u64)
}

/// The take's subtasks, or the whole take as one when it was not split.
fn segments(meta: &Value, labels: &mut Vec<String>) -> anyhow::Result<Vec<Segment>> {
    let listed: Vec<Segment> = meta["segments"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(Segment {
                start_ns: ns(&s["start_time"])?,
                end_ns: ns(&s["end_time"])?,
                label_idx: s["label_idx"].as_u64()? as usize,
                success: s["success"].as_bool().unwrap_or(true),
            })
        })
        .collect();
    if !listed.is_empty() {
        return Ok(listed);
    }
    let episode = &meta["episode"];
    let usable = &meta["trigger"]["usable"];
    let span = |key: &str| ns(&usable[key]).or_else(|| ns(&episode[key]));
    let (Some(start_ns), Some(end_ns)) = (span("start_time"), span("end_time")) else {
        anyhow::bail!("meta.json has neither segments nor an episode span");
    };
    let label = episode["label"].as_str().filter(|l| !l.is_empty()).unwrap_or("untitled");
    if !labels.iter().any(|l| l == label) {
        labels.push(label.to_string());
    }
    let label_idx = labels.iter().position(|l| l == label).unwrap();
    let success = episode["success"].as_bool().unwrap_or(true);
    Ok(vec![Segment { start_ns, end_ns, label_idx, success }])
}

fn flat(rows: &[[f64; 7]]) -> Values {
    Values::List(7, rows.iter().flatten().copied().collect())
}

/// `[x, y, z, qx, qy, qz, qw]` as xyz then rot6d.
fn rot6d(p: &[f64; 7]) -> [f64; 9] {
    let m = glam::DMat3::from_quat(glam::DQuat::from_xyzw(p[3], p[4], p[5], p[6]));
    let (a, b) = (m.x_axis, m.y_axis);
    [p[0], p[1], p[2], a.x, a.y, a.z, b.x, b.y, b.z]
}

/// One episode's columns, in rebake's order, over grid rows `rows`.
fn columns(
    grid: &Grid,
    rows: std::ops::Range<usize>,
    episode: usize,
    first_index: usize,
    segment: &Segment,
) -> Vec<Column> {
    let n = rows.len();
    let pick = |v: &[bool]| Values::Bool(v[rows.clone()].to_vec());
    let poses = |v: &[[f64; 7]]| flat(&v[rows.clone()]);
    let mut out = Vec::new();

    for (slot, name) in CAMERAS.iter().enumerate() {
        if let Some(frames) = &grid.frames[slot] {
            let fresh: Vec<bool> =
                rows.clone().map(|k| k == 0 || frames[k] != frames[k - 1]).collect();
            out.push(Column::new(
                format!("observation.image.{name}.is_fresh"),
                Values::Bool(fresh),
            ));
        }
    }

    let joints = |r: std::ops::Range<usize>| -> Values {
        Values::List(2, r.flat_map(|k| grid.joints[k]).collect())
    };
    // The next tick's angle, and the last tick's own at the end of the take.
    let last = grid.joints.len() - 1;
    let next = rows.clone().map(|k| (k + 1).min(last));
    out.push(Column::new("observation.joint_states", joints(rows.clone())).named(&JOINTS));
    out.push(Column::new("observation.joint_states.is_fresh", pick(&grid.joints_fresh)));
    out.push(
        Column::new(
            "action.joint_states",
            Values::List(2, next.flat_map(|k| grid.joints[k]).collect()),
        )
        .named(&JOINTS),
    );
    out.push(Column::new("action.joint_states.is_fresh", pick(&grid.joints_fresh)));

    for (feature, track) in [
        ("action.left_controller", "left_controller"),
        ("action.pose.left_hand_root", "left_hand_root"),
        ("action.pose.right_hand_root", "right_hand_root"),
        ("action.right_controller", "right_controller"),
    ] {
        let t = grid.track(track);
        out.push(Column::new(format!("{feature}.relative"), poses(&t.relative)).named(&STEP));
        out.push(Column::new(format!("{feature}.relative.is_fresh"), pick(&t.fresh)));
    }

    let mut tracks: Vec<&signals::Track> = grid.tracks.iter().collect();
    tracks.sort_by_key(|t| t.name);
    for t in tracks {
        let base = format!("observation.pose.{}", t.name);
        out.push(Column::new(format!("{base}.absolute"), poses(&t.absolute)).named(&POSE));
        out.push(Column::new(format!("{base}.absolute.is_fresh"), pick(&t.fresh)));
        // rebake's robot model names this one's step differently.
        let relative = match t.name {
            "left_hand_root_to_right_hand_root" => {
                "observation.pose.left_hand_to_right_hand.relative".to_string()
            }
            _ => format!("{base}.relative"),
        };
        out.push(Column::new(relative, poses(&t.relative)).named(&STEP));
    }

    let inter = &grid.track("left_hand_root_to_right_hand_root").absolute;
    let (left, right) =
        (&grid.track("left_hand_root").absolute, &grid.track("right_hand_root").absolute);
    let state = rows.clone().flat_map(|k| rot6d(&inter[k]).into_iter().chain(grid.joints[k]));
    out.push(Column::new("observation.state", Values::List(11, state.collect())).named(&STATE));
    // The fingers are `action.joint_states`: the next tick's angles.
    let action = rows.clone().flat_map(|k| {
        let j = grid.joints[(k + 1).min(last)];
        rot6d(&left[k]).into_iter().chain(rot6d(&right[k])).chain(j)
    });
    out.push(Column::new("action", Values::List(20, action.collect())).named(&ACTION));

    let ticks: Vec<i64> = grid.t_ns[rows.clone()].iter().map(|&t| t as i64).collect();
    out.push(Column::new("synched_timestamp_ns", Values::I64(ticks)));
    let label = segment.label_idx as i64;
    out.push(Column::new("next.done", Values::Bool((0..n).map(|i| i + 1 == n).collect())));
    out.push(Column::new("primitive_action_index", Values::I64(vec![label; n])));
    out.push(Column::new("success_primitive_action", Values::Bool(vec![segment.success; n])));
    out.push(Column::new("task_index", Values::I64(vec![label; n])));
    out.push(Column::new("index", Values::I64((0..n).map(|i| (first_index + i) as i64).collect())));
    out.push(Column::new("frame_index", Values::I64((0..n as i64).collect())));
    out.push(Column::new("episode_index", Values::I64(vec![episode as i64; n])));
    out.push(Column::new(
        "timestamp",
        Values::F64((0..n).map(|i| i as f64 / FPS as f64).collect()),
    ));
    out
}

/// The features after the images, which rebake lists last.
const STANDARD: [&str; 8] = [
    "next.done",
    "primitive_action_index",
    "success_primitive_action",
    "task_index",
    "index",
    "frame_index",
    "episode_index",
    "timestamp",
];

fn features(columns: &[Column], cameras: &[&str]) -> serde_json::Map<String, Value> {
    let entry = |c: &Column| {
        let (dtype, shape) = c.dtype();
        json!({ "dtype": dtype, "shape": [shape], "names": c.names })
    };
    let mut out = serde_json::Map::new();
    // Every column is listed, `synched_timestamp_ns` too, though rebake leaves
    // it out: LeRobot v3.0 refuses a parquet whose columns differ from the list.
    for c in columns.iter().filter(|c| !STANDARD.contains(&c.name.as_str())) {
        out.insert(c.name.clone(), entry(c));
    }
    for name in cameras {
        out.insert(
            format!("observation.image.{name}"),
            json!({
                "dtype": "video",
                "shape": [picture::HEIGHT, picture::WIDTH, 3],
                "names": ["height", "width", "channel"],
                "info": {
                    "video.fps": FPS,
                    "video.codec": "h264",
                    "video.pix_fmt": "yuv420p",
                    "video.is_depth_map": "false",
                    "has_audio": "false",
                },
            }),
        );
    }
    for c in columns.iter().filter(|c| STANDARD.contains(&c.name.as_str())) {
        out.insert(c.name.clone(), entry(c));
    }
    out
}

fn program<'a>(meta: &'a Value, roles: &[&str]) -> Option<&'a Value> {
    meta["programs"].as_array()?.iter().find(|p| roles.iter().any(|r| p["role"] == *r))
}

/// One line of `episodes.jsonl`, as rebake's PA mode writes it.
fn episode_line(
    meta: &Value,
    tasks: &[String],
    episode: usize,
    source: usize,
    length: usize,
    segment: &Segment,
) -> anyhow::Result<Value> {
    let recorder = program(meta, &["data_collection", "data_capture", "record_manager"])
        .ok_or_else(|| anyhow::anyhow!("meta.json names no data_collection program"))?;
    // On YUBI Go the app that teleoperates is the app that records, so a take
    // without a teleoperation entry has it under data_collection alone.
    let interface = program(meta, &["interface", "teleoperation"]).unwrap_or(recorder);
    let git = |p: &Value, key: &str| p["source"]["git"][key].as_str().unwrap_or("").to_string();
    let bag = meta["files"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|f| ["rosbag", "rosbag2", "mcap", "raw_streams"].iter().any(|t| f["type"] == *t))
        .and_then(|f| f["name"].as_str())
        .unwrap_or("signals.mcap");
    let task = &tasks[segment.label_idx];
    let sht: Vec<&String> =
        if tasks.len() > 1 { tasks.last().into_iter().collect() } else { vec![] };
    let uuid = meta["uuid"].as_str().unwrap_or("");
    let metadata: serde_json::Map<String, Value> =
        METADATA_KEYS.iter().filter_map(|k| Some((k.to_string(), meta.get(*k)?.clone()))).collect();
    Ok(json!({
        "episode_index": episode,
        "episode_id": format!("{uuid}:{source}"),
        "tasks": [task],
        "length": length,
        "bag_path": bag,
        "version": "1.0",
        "location_name": meta["environment"]["site"],
        "interface": interface["name"],
        "git_hash": git(recorder, "hash"),
        "git_branch": git(recorder, "branch"),
        "interface_git_hash": git(interface, "hash"),
        "interface_git_branch": git(interface, "branch"),
        "pipeline_git_hash": "",
        "pipeline_git_branch": "",
        "label": "",
        "hsr_id": meta["robot"]["id"],
        "task_type": "PA",
        "task_success": segment.success,
        "short_horizon_task": sht,
        "primitive_action": [task],
        "success_short_horizon_task": segment.success,
        "uuid": uuid,
        "metadata": metadata,
    }))
}

fn jsonl(path: &Path, lines: &[Value]) -> std::io::Result<()> {
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    std::fs::write(path, text)
}

/// One take's labels as rebake registers them: the subtask labels in order,
/// then the take's own label.
fn local_tasks(meta: &Value) -> anyhow::Result<(Vec<String>, Vec<Segment>)> {
    let mut tasks: Vec<String> = meta["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| Some(l.as_str()?.to_string()))
        .collect();
    let segments = segments(meta, &mut tasks)?;
    let take_label = meta["episode"]["label"].as_str().unwrap_or("");
    if !take_label.is_empty() && !tasks.iter().any(|t| t == take_label) {
        tasks.push(take_label.to_string());
    }
    if let Some(s) = segments.iter().find(|s| s.label_idx >= tasks.len()) {
        anyhow::bail!("a segment names label {} of {}", s.label_idx, tasks.len());
    }
    Ok((tasks, segments))
}

/// The part (left hand, right hand, headset) that lost tracking during
/// `segment`, if one did.
fn lost_during(index: &Index, segment: &Segment) -> Option<&'static str> {
    (0..3)
        .find(|&part| {
            index.untracked[part].iter().any(|&(a, b)| a < segment.end_ns && b > segment.start_ns)
        })
        .map(|part| ["left hand", "right hand", "headset"][part])
}

/// Each subtask of the take in `dir` as (failed, lost tracking), so the export
/// dialog can say what each [`Leave`] drops. Kept per take, because reading
/// the signals of a 58 MB take takes ~0.5 s, and a finished take never changes.
pub fn subtasks(dir: &Path) -> anyhow::Result<Vec<(bool, bool)>> {
    type Flags = Vec<(bool, bool)>;
    static KNOWN: Mutex<Vec<(PathBuf, Flags)>> = Mutex::new(Vec::new());
    if let Some((_, known)) = KNOWN.lock().unwrap().iter().find(|(d, _)| d == dir) {
        return Ok(known.clone());
    }
    let meta: Value =
        serde_json::from_slice(&std::fs::read(dir.join(crate::record::record::META))?)?;
    let index = Index::read(&dir.join(crate::record::record::SIGNALS))?;
    let (_, segments) = local_tasks(&meta)?;
    let found: Vec<(bool, bool)> =
        segments.iter().map(|s| (!s.success, lost_during(&index, s).is_some())).collect();
    KNOWN.lock().unwrap().push((dir.to_path_buf(), found.clone()));
    Ok(found)
}

/// Which subtasks an export leaves out.
#[derive(Clone, Copy, Debug, Default)]
pub struct Leave {
    /// The failed tries. A failed take keeps the subtasks it did finish.
    pub failed: bool,
    /// A subtask during which a hand or the headset lost tracking, as the
    /// viewer shades it.
    pub untracked: bool,
}

/// Writes one dataset of every take in `takes` into `out`, which must not
/// exist. The takes must share a robot type and a set of cameras, because a
/// dataset has one feature list.
pub fn export(
    takes: &[PathBuf],
    out: &Path,
    leave: Leave,
    video: &dyn Video,
    progress: &mut (dyn FnMut(String) + Send),
) -> anyhow::Result<Summary> {
    std::fs::create_dir_all(out.join("meta"))?;
    let mut dataset = Dataset::new(out, leave);
    for (n, take) in takes.iter().enumerate() {
        progress(format!("episode {} of {}: reading", n + 1, takes.len()));
        dataset.take(n, take)?;
    }
    let Dataset {
        tasks,
        warnings,
        episodes,
        mut stats,
        features,
        first,
        frames,
        indexes,
        clips,
        ..
    } = dataset;
    let images = self::clips(&clips, &indexes, video, progress)?;
    for (clip, rgb) in clips.iter().zip(images) {
        stats[clip.episode]["stats"][format!("observation.image.{}", clip.camera)] = rgb.stats();
    }
    let (Some(features), Some((_, robot, cameras))) = (features, first) else {
        anyhow::bail!("no subtask overlaps the recorded signals");
    };

    let info = json!({
        "codebase_version": "v2.1",
        "robot_type": robot,
        "total_episodes": episodes.len(),
        "total_frames": frames,
        "total_tasks": tasks.len(),
        "total_videos": episodes.len() * cameras.len(),
        "total_chunks": episodes.len().div_ceil(CHUNK),
        "chunks_size": CHUNK,
        "fps": FPS,
        "splits": { "train": format!("0:{}", episodes.len()) },
        "data_path": "data/chunk-{episode_chunk:03d}/episode_{episode_index:06d}.parquet",
        "video_path": "videos/chunk-{episode_chunk:03d}/{video_key}/episode_{episode_index:06d}.mp4",
        "features": features,
    });
    std::fs::write(out.join("meta/info.json"), serde_json::to_string_pretty(&info)?)?;
    let tasks_lines: Vec<Value> =
        tasks.iter().enumerate().map(|(i, t)| json!({ "task_index": i, "task": t })).collect();
    jsonl(&out.join("meta/tasks.jsonl"), &tasks_lines)?;
    jsonl(&out.join("meta/episodes.jsonl"), &episodes)?;
    jsonl(&out.join("meta/episodes_stats.jsonl"), &stats)?;
    Ok(Summary { episodes: episodes.len(), frames, warnings })
}

/// The dataset as the takes are added: each episode's table is written as it
/// comes, and its videos wait as [`Clip`]s until every table is.
struct Dataset<'a> {
    out: &'a Path,
    /// The labels the episodes carry, in the order they were first met.
    /// A trainer can read each one as a frame's label and refuse any that no
    /// frame carries, so a take's own label is not listed.
    tasks: Vec<String>,
    warnings: Vec<String>,
    /// `episodes.jsonl` and `episodes_stats.jsonl`, a line per episode.
    episodes: Vec<Value>,
    stats: Vec<Value>,
    features: Option<serde_json::Map<String, Value>>,
    /// The first take's name, robot type and cameras, which the others must
    /// match.
    first: Option<(String, Value, Vec<&'static str>)>,
    frames: usize,
    /// Each take's index, kept for its videos.
    indexes: Vec<Index>,
    leave: Leave,
    clips: Vec<Clip>,
}

/// One take, as its episodes read it.
struct Take<'a> {
    n: usize,
    name: &'a str,
    meta: &'a Value,
    grid: &'a Grid,
    /// The take's own labels, which its segments index.
    local: &'a [String],
    cameras: &'a [(usize, &'static str)],
    names: &'a [&'static str],
}

impl<'a> Dataset<'a> {
    fn new(out: &'a Path, leave: Leave) -> Dataset<'a> {
        Dataset {
            out,
            leave,
            tasks: Vec::new(),
            warnings: Vec::new(),
            episodes: Vec::new(),
            stats: Vec::new(),
            features: None,
            first: None,
            frames: 0,
            indexes: Vec::new(),
            clips: Vec::new(),
        }
    }

    /// Adds take `n`, in `dir`, as an episode per subtask.
    fn take(&mut self, n: usize, dir: &Path) -> anyhow::Result<()> {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("?").to_string();
        let meta: Value =
            serde_json::from_slice(&std::fs::read(dir.join(crate::record::record::META))?)?;
        let mut index = Index::read_all(&dir.join(crate::record::record::SIGNALS))?;
        let calib = crate::record::camera_calib::default();
        let cameras = std::array::from_fn(|slot| {
            crate::record::camera_calib::exposures(&index, slot, &calib)
        });
        // One delay for both jaws: the sides differ by 1.4 ms (2026-10-31).
        let encoder_late = (calib.encoder_late_ms.left + calib.encoder_late_ms.right) / 2.0;
        let grid = signals::resample(&index, cameras, (encoder_late * 1e6).max(0.0) as u64)
            .map_err(|e| anyhow::anyhow!("{name}: {e}"))?;
        let (local, segments) = local_tasks(&meta).map_err(|e| anyhow::anyhow!("{name}: {e}"))?;

        let cameras: Vec<(usize, &'static str)> = CAMERAS
            .iter()
            .enumerate()
            .filter(|(slot, _)| grid.frames[*slot].is_some())
            .map(|(s, n)| (s, *n))
            .collect();
        let names: Vec<&'static str> = cameras.iter().map(|c| c.1).collect();
        self.check_matches(&name, &meta, &names)?;
        self.warnings.extend(grid.warnings.iter().map(|w| format!("{name}: {w}")));

        let take = Take {
            n,
            name: &name,
            meta: &meta,
            grid: &grid,
            local: &local,
            cameras: &cameras,
            names: &names,
        };
        for (source, segment) in segments.iter().enumerate() {
            if self.leave.failed && !segment.success {
                continue;
            }
            if let Some(who) = lost_during(&index, segment).filter(|_| self.leave.untracked) {
                self.warnings
                    .push(format!("{name}: subtask {source} left out: {who} lost tracking"));
                continue;
            }
            self.episode(&take, source, segment)?;
        }
        // The videos need only the camera frames, and a take's poses are
        // megabytes a minute.
        index.poses = Default::default();
        index.joints = Vec::new();
        index.grippers = Default::default();
        self.indexes.push(index);
        Ok(())
    }

    /// A label's place in the dataset's task list, which it joins the first
    /// time an episode carries it.
    fn global(&mut self, label: &str) -> usize {
        if !self.tasks.iter().any(|g| g == label) {
            self.tasks.push(label.to_string());
        }
        self.tasks.iter().position(|g| g == label).unwrap()
    }

    /// Refuses a take whose robot type or cameras differ from the first take's.
    fn check_matches(
        &mut self,
        name: &str,
        meta: &Value,
        names: &[&'static str],
    ) -> anyhow::Result<()> {
        match &self.first {
            None => {
                self.first =
                    Some((name.to_string(), meta["robot"]["type"].clone(), names.to_vec()));
            }
            Some((other, robot, cams)) => {
                if *robot != meta["robot"]["type"] {
                    anyhow::bail!(
                        "{name} was recorded as robot {}, {other} as {robot}",
                        meta["robot"]["type"]
                    );
                }
                if cams != names {
                    anyhow::bail!("{name} has cameras {names:?}, {other} has {cams:?}");
                }
            }
        }
        Ok(())
    }

    /// Writes one subtask's table, and queues its videos.
    fn episode(&mut self, take: &Take, source: usize, segment: &Segment) -> anyhow::Result<()> {
        let grid = take.grid;
        let start = grid.t_ns.partition_point(|&t| t < segment.start_ns);
        let end = grid.t_ns.partition_point(|&t| t <= segment.end_ns);
        if start >= end {
            self.warnings.push(format!("{}: subtask {source} lies outside its signals", take.name));
            return Ok(());
        }
        let e = self.episodes.len();
        let labelled =
            Segment { label_idx: self.global(&take.local[segment.label_idx]), ..*segment };
        let columns = columns(grid, start..end, e, self.frames, &labelled);
        let file = self.out.join(format!("data/chunk-{:03}/episode_{e:06}.parquet", e / CHUNK));
        std::fs::create_dir_all(file.parent().unwrap())?;
        table::write(&file, &columns)?;
        let episode_stats: serde_json::Map<String, Value> =
            columns.iter().map(|c| (c.name.clone(), c.stats())).collect();
        for &(slot, camera) in take.cameras {
            let file = self.out.join(format!(
                "videos/chunk-{:03}/observation.image.{camera}/episode_{e:06}.mp4",
                e / CHUNK
            ));
            std::fs::create_dir_all(file.parent().unwrap())?;
            let picks = grid.frames[slot].as_ref().unwrap()[start..end].to_vec();
            self.clips.push(Clip { take: take.n, episode: e, slot, camera, picks, file });
        }
        self.features.get_or_insert_with(|| features(&columns, take.names));
        self.stats.push(json!({
            "episode_index": e,
            "episode_id": format!("{}:{source}", take.meta["uuid"].as_str().unwrap_or("")),
            "stats": episode_stats,
        }));
        self.episodes.push(episode_line(take.meta, take.local, e, source, end - start, segment)?);
        self.frames += end - start;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
