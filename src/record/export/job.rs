//! One export at a time, in the background, for the viewer to start and poll.
//!
//! A dataset is written to `exports/<id>.partial` and renamed when whole, so a
//! download never finds half of one. `exports/<id>.json` says what went in.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{json, Value};

use super::Video;

pub const EXPORTS: &str = "exports";

/// The longest id. A tar path is `<id>/` and then up to 61 bytes of
/// `videos/chunk-000/observation.image.center/episode_000000.mp4`, and a
/// ustar name holds 99.
const MAX_ID: usize = 32;

enum Job {
    Running { id: String, name: String, takes: usize, step: String },
    Failed { id: String, name: String, error: String },
}

static JOB: Mutex<Option<Job>> = Mutex::new(None);

fn lock() -> std::sync::MutexGuard<'static, Option<Job>> {
    JOB.lock().unwrap_or_else(|e| e.into_inner())
}

fn root(base: &str) -> PathBuf {
    Path::new(base).join(EXPORTS)
}

/// The dataset `id`, once it is whole.
pub fn dir(base: &str, id: &str) -> PathBuf {
    root(base).join(id)
}

fn summary(base: &str, id: &str) -> PathBuf {
    root(base).join(format!("{id}.json"))
}

/// An id as the routes accept it: what [`id_for`] makes.
pub fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A file name for `name`: its ASCII letters and digits, the rest as `-`, and
/// a number after it when that is taken.
fn id_for(base: &str, name: &str) -> String {
    let mut slug: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    slug.truncate(MAX_ID - 4);
    if slug.is_empty() {
        slug = "dataset".into();
    }
    let taken = |id: &str| dir(base, id).exists() || summary(base, id).exists();
    let mut id = slug.clone();
    for n in 2.. {
        if !taken(&id) {
            break;
        }
        id = format!("{slug}-{n}");
    }
    id
}

pub fn running() -> bool {
    matches!(&*lock(), Some(Job::Running { .. }))
}

/// Every finished export, newest first, and the one running or last failed.
pub fn list(base: &str) -> Value {
    let job = match &*lock() {
        Some(Job::Running { id, name, takes, step }) => {
            json!({ "id": id, "name": name, "episodes": takes, "state": "running", "step": step })
        }
        Some(Job::Failed { id, name, error }) => {
            json!({ "id": id, "name": name, "state": "failed", "error": error })
        }
        None => Value::Null,
    };
    let mut done: Vec<Value> = std::fs::read_dir(root(base))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let id = e.file_name().to_str()?.strip_suffix(".json")?.to_string();
            let summary: Value = serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok()?;
            dir(base, &id).is_dir().then(|| renamed(summary))
        })
        .collect();
    done.sort_by_key(|s| std::cmp::Reverse(s["created"].as_u64().unwrap_or(0)));
    json!({ "job": job, "exports": done })
}

/// A summary in today's keys. Pre-release builds wrote the source
/// episodes as `takes` and the LeRobot episode count as `episodes`.
fn renamed(mut summary: Value) -> Value {
    if let Some(sources) = summary.as_object_mut().and_then(|s| s.remove("takes")) {
        summary["lerobot_episodes"] = summary["episodes"].take();
        summary["episodes"] = sources;
    }
    summary
}

/// Starts exporting `takes` as one dataset called `name`, unless another
/// export is running. Answers the new export's id.
pub fn start(
    base: &str,
    name: &str,
    takes: Vec<String>,
    leave: super::Leave,
    video: Box<dyn Video + Send>,
) -> Result<String, String> {
    let mut job = lock();
    if let Some(Job::Running { name, .. }) = &*job {
        return Err(format!("{name} is exporting"));
    }
    let id = id_for(base, name);
    *job = Some(Job::Running {
        id: id.clone(),
        name: name.to_string(),
        takes: takes.len(),
        step: "starting".into(),
    });
    drop(job);
    let (base, run_id, name) = (base.to_string(), id.clone(), name.to_string());
    let spawned = std::thread::Builder::new()
        .name("export".into())
        .spawn(move || run(&base, &run_id, &name, &takes, leave, video));
    if let Err(e) = spawned {
        *lock() = None;
        return Err(format!("no export thread: {e}"));
    }
    Ok(id)
}

fn run(
    base: &str,
    id: &str,
    name: &str,
    takes: &[String],
    leave: super::Leave,
    video: Box<dyn Video + Send>,
) {
    // Below the capture and render threads: an export may take minutes and
    // has nobody waiting on its frame times.
    #[cfg(target_os = "android")]
    // SAFETY: a plain syscall on this thread.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10);
    }
    let since = Instant::now();
    let out = dir(base, id);
    let part = root(base).join(format!("{id}.partial"));
    let _ = std::fs::remove_dir_all(&part);
    let sources: Vec<PathBuf> =
        takes.iter().map(|t| Path::new(&crate::record::record::takes(base)).join(t)).collect();
    let result = super::export(&sources, &part, leave, video.as_ref(), &mut |step| {
        if let Some(Job::Running { step: s, .. }) = &mut *lock() {
            *s = step;
        }
    })
    .and_then(|s| {
        std::fs::rename(&part, &out)?;
        let done = json!({
            "id": id,
            "name": name,
            "state": "done",
            "episodes": takes,
            "lerobot_episodes": s.episodes,
            "frames": s.frames,
            "warnings": s.warnings,
            "bytes": bytes(&out),
            "seconds": since.elapsed().as_secs(),
            "created": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
        });
        std::fs::write(summary(base, id), done.to_string())?;
        Ok(s)
    });
    match result {
        Ok(s) => {
            log::info!(
                "exported {} takes as LeRobot {id}: {} episodes, {} frames in {:.0?}",
                takes.len(),
                s.episodes,
                s.frames,
                since.elapsed()
            );
            *lock() = None;
        }
        Err(e) => {
            log::warn!("cannot export {id}: {e:#}");
            let _ = std::fs::remove_dir_all(&part);
            let (id, name) = (id.to_string(), name.to_string());
            *lock() = Some(Job::Failed { id, name, error: format!("{e:#}") });
        }
    }
}

fn bytes(dir: &Path) -> u64 {
    files(dir, "").iter().map(|f| f.2).sum()
}

/// Every file under `dir`, as `(path inside it, path on disk, bytes)`.
pub fn files(dir: &Path, prefix: &str) -> Vec<(String, PathBuf, u64)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = format!("{prefix}{}", e.file_name().to_string_lossy());
        let path = e.path();
        if path.is_dir() {
            out.extend(files(&path, &format!("{name}/")));
        } else if let Ok(meta) = e.metadata() {
            out.push((name, path, meta.len()));
        }
    }
    out
}

/// Whether `id` is a finished export.
pub fn is_done(base: &str, id: &str) -> bool {
    dir(base, id).is_dir() && summary(base, id).exists()
}

pub fn remove(base: &str, id: &str) -> std::io::Result<()> {
    let mut job = lock();
    if matches!(&*job, Some(Job::Failed { id: failed, .. }) if failed == id) {
        *job = None;
    }
    drop(job);
    let _ = std::fs::remove_file(summary(base, id));
    std::fs::remove_dir_all(dir(base, id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_listed_with_their_paths_inside_the_dataset() {
        let root = std::env::temp_dir().join(format!("yubigo_export_files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("meta")).unwrap();
        std::fs::create_dir_all(root.join("data/chunk-000")).unwrap();
        std::fs::write(root.join("meta/info.json"), b"{}").unwrap();
        std::fs::write(root.join("data/chunk-000/episode_000000.parquet"), b"PAR1").unwrap();
        let listed: Vec<(String, u64)> =
            files(&root, "x/").into_iter().map(|(n, _, b)| (n, b)).collect();
        assert_eq!(
            listed,
            [
                ("x/data/chunk-000/episode_000000.parquet".to_string(), 4),
                ("x/meta/info.json".into(), 2)
            ]
        );
    }

    #[test]
    fn an_id_is_the_name_in_plain_letters_and_never_reused() {
        let base = std::env::temp_dir().join(format!("yubigo_export_ids-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let base = base.to_str().unwrap();
        assert_eq!(id_for(base, "cups / day 1"), "cups-day-1");
        assert_eq!(id_for(base, "カップ"), "dataset");
        std::fs::create_dir_all(dir(base, "cups")).unwrap();
        assert_eq!(id_for(base, "cups"), "cups-2");
        let long = id_for(base, &"x".repeat(80));
        assert!(safe_id(&long) && long.len() <= MAX_ID - 4, "{long}");
        assert!(!safe_id("../x") && !safe_id(""));
    }

    #[test]
    fn a_summary_from_an_older_build_reads_in_todays_keys() {
        let old = json!({"takes": ["episode_1", "episode_2"], "episodes": 5});
        assert_eq!(
            renamed(old),
            json!({"episodes": ["episode_1", "episode_2"], "lerobot_episodes": 5})
        );
        let new = json!({"episodes": ["episode_1"], "lerobot_episodes": 2});
        assert_eq!(renamed(new.clone()), new);
    }
}
