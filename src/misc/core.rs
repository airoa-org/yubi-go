//! airoa_core's lifecycle: bring it up, and say whether it is up.
//!
//! Every thread that needs the core asks [`up`] here, so "the core is not
//! ready yet" is written once. `app` and `hub` have no other business with
//! the SDK, so they do not learn how the boundary starts.
//!
//! Everything the core *does* has an owner elsewhere:
//! `record::gate` for the verdict, `record::task` for the FSM,
//! `record::upload` for storage, `record::backend::episode` and `record::backend::poll` for the
//! backend, `record::system_info` for the manifest. This module owns only
//! whether the thing is running.

use std::io::Read;
use std::path::Path;

use log::{info, warn};

/// Where airoa_core keeps its SQLite files (the offline queue and the upload
/// state), under the files dir. A subdirectory rather than the files dir
/// itself because upstream opens each with `os.makedirs(dirname)`, and
/// inside the sandbox `os.path.exists("/")` is false — so a file directly
/// under the mount makes makedirs walk up to `/` and fail.
pub(crate) const DB_DIR: &str = "db";

/// The files dir as the guest sees it; takes are one level below.
pub(crate) const GUEST_ROOT: &str = "/episodes";

/// The offline queue as the guest sees it. In the files dir rather than in
/// a take, because a backlog belongs to the rig rather than to any one
/// episode, and the GC's sweep is confined to `episodes/`.
pub(crate) fn queue_guest_path() -> String {
    format!("{GUEST_ROOT}/{DB_DIR}/report-queue.db")
}

/// The upload state as the guest sees it: which takes were sent, and where.
/// Outside `episodes/` because a take is a thing the GC reclaims and the
/// uploader sends — the record of what was sent has to outlive both.
pub(crate) fn upload_state_guest_path() -> String {
    format!("{GUEST_ROOT}/{DB_DIR}/upload_state.db")
}

/// The payload, as the APK bundles it under `assets/airoa/` and the runtime
/// reads it from `files/airoa/`: only the cwasm reaches a device. The stamp is
/// the builder's record of what the cwasm was built from, so it is what says
/// two copies are the same payload.
const PAYLOAD: [&str; 1] = ["airoa-core.cwasm"];
const STAMP: &str = "airoa-core.cwasm.src";

/// Copy the payload this APK carries into `files_dir/airoa`, unless the copy
/// there is already this one. `open` reads one bundled file by name.
///
/// **This is what makes a fresh install record.** The gate refuses a start
/// while airoa_core is down, and a payload pushed by hand with `adb push`
/// lands owned by the shell user and unreadable to the app until someone
/// thinks to `chmod` it. Copied by the app, it is the app's own file.
///
/// Files go through a temporary name and the stamp is written last, so a copy
/// cut short by the app dying is simply made again at the next launch. Returns
/// whether it copied; an APK built without a payload copies nothing.
pub fn unpack<R: Read>(
    files_dir: &Path,
    open: impl Fn(&str) -> Option<R>,
) -> std::io::Result<bool> {
    unpack_bundle(&files_dir.join("airoa"), &PAYLOAD, STAMP, open)
}

/// The copy [`unpack`] makes, for any bundle the APK carries: `payload` are
/// paths under `dir`, and `stamp` is the file that says which bundle it is.
pub fn unpack_bundle<R: Read>(
    dir: &Path,
    payload: &[&str],
    stamp: &str,
    open: impl Fn(&str) -> Option<R>,
) -> std::io::Result<bool> {
    let Some(mut bundled) = open(stamp) else { return Ok(false) };
    let mut wanted = Vec::new();
    bundled.read_to_end(&mut wanted)?;
    let current = std::fs::read(dir.join(stamp)).ok();
    if current.as_deref() == Some(&wanted[..]) && payload.iter().all(|f| dir.join(f).exists()) {
        return Ok(false);
    }
    for name in payload {
        let mut src = open(name).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, format!("APK has no {name}"))
        })?;
        let to = dir.join(name);
        std::fs::create_dir_all(to.parent().unwrap_or(dir))?;
        let tmp = dir.join(format!("{name}.tmp"));
        std::io::copy(&mut src, &mut std::fs::File::create(&tmp)?)?;
        std::fs::rename(&tmp, to)?;
    }
    std::fs::write(dir.join(stamp), &wanted)?;
    Ok(true)
}

/// Start the component in the background, with the episodes directory
/// preopened so the storage ops can see it.
///
/// Failure is logged and dropped on purpose: a rig whose payload is missing
/// still records, still previews and still serves `:8080`. What waits is the
/// backend and the canonical upload paths, and each says so for itself.
pub fn start(files_dir: &'static str, episodes_guest: &'static str) {
    std::thread::spawn(move || {
        let payload = Path::new(files_dir).join("airoa");
        let episodes = Path::new(files_dir);
        if !airoa_sdk::ready() {
            adopt_old_databases(episodes);
            clear_stale_locks(&episodes.join(DB_DIR));
        }
        match airoa_sdk::start_with(
            &payload,
            &[(episodes, episodes_guest, airoa_sdk::Access::ReadWrite)],
        ) {
            Ok(()) => info!("airoa_core running from {}", payload.display()),
            Err(e) => warn!(
                "airoa_core unavailable ({e}) — the gate refuses a start until it runs; \
                 the APK carries the payload, so rebuild with android/build.sh"
            ),
        }
    });
}

/// The offline queue and the upload state: where pre-release builds kept them,
/// and where upstream opens them now, under `db/`.
const MOVED_DATABASES: [(&str, &str); 2] =
    [("report-queue.db", "report-queue.db"), (".upload_state.db", "upload_state.db")];

/// Move each database from its pre-release place into `db/` on the first
/// launch of this build. Left behind, the upload state would make every take already
/// sent look unsent, so each would be uploaded again, and a backend rig would
/// lose its queued reports. A database already at the new place is never
/// overwritten. SQLite's journal sidecars move with their database.
fn adopt_old_databases(files_dir: &Path) {
    for (old, new) in MOVED_DATABASES {
        let (from, to) = (files_dir.join(old), files_dir.join(DB_DIR).join(new));
        if !from.exists() || to.exists() {
            continue;
        }
        let moved = std::fs::create_dir_all(files_dir.join(DB_DIR)).and_then(|()| {
            for suffix in ["", "-journal", "-wal", "-shm"] {
                let from = files_dir.join(format!("{old}{suffix}"));
                if from.exists() {
                    std::fs::rename(from, files_dir.join(DB_DIR).join(format!("{new}{suffix}")))?;
                }
            }
            Ok(())
        });
        match moved {
            Ok(()) => info!("airoa_core: {old} moved to {DB_DIR}/{new}"),
            Err(e) => warn!("airoa_core: {old} not moved to {DB_DIR}/{new}: {e}"),
        }
    }
}

/// Remove the `<db>.lock` directories a force-stop strands.
///
/// The sandbox's SQLite has no fcntl locks: its `unix-dotfile` VFS takes a
/// lock by creating `<db>.lock` and releases it by removing the directory, so
/// a process killed mid-transaction leaves one behind, and every later open of
/// `report-queue.db` or `upload_state.db` fails with "database is locked".
/// Nothing but this process opens those files, so a lock that exists before
/// airoa_core is up has no owner. Only an empty directory is removed: a
/// dotfile lock never holds anything.
fn clear_stale_locks(files_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(files_dir) else { return };
    for path in entries.flatten().map(|e| e.path()) {
        if !path.is_dir() || path.extension().is_none_or(|e| e != "lock") {
            continue;
        }
        match std::fs::remove_dir(&path) {
            Ok(()) => {
                warn!("airoa_core: stale lock {} removed — a force-stop left it", path.display())
            }
            Err(e) => warn!("airoa_core: stale lock {} not removed: {e}", path.display()),
        }
    }
}

/// Whether the core has finished starting. Every thread that needs it waits
/// on this rather than on a copy of the same question.
pub fn up() -> bool {
    airoa_sdk::ready()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(stamp: &'static str) -> impl Fn(&str) -> Option<&'static [u8]> {
        move |name| match name {
            STAMP => Some(stamp.as_bytes()),
            "airoa-core.cwasm" => Some(b"cwasm".as_slice()),
            _ => None,
        }
    }

    /// Copied once, not again while it matches, and again for a new build.
    #[test]
    fn the_bundled_payload_is_copied_when_it_differs() {
        let dir = std::env::temp_dir().join(format!("yubigo_unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(unpack(&dir, bundle("a b")).unwrap(), "a fresh install copies");
        assert_eq!(std::fs::read(dir.join("airoa/airoa-core.cwasm")).unwrap(), b"cwasm");
        assert!(!unpack(&dir, bundle("a b")).unwrap(), "the same payload is left alone");
        std::fs::remove_file(dir.join("airoa/airoa-core.cwasm")).unwrap();
        assert!(unpack(&dir, bundle("a b")).unwrap(), "a missing file is restored");
        assert!(unpack(&dir, bundle("c d")).unwrap(), "a new build replaces it");
        assert_eq!(std::fs::read(dir.join("airoa").join(STAMP)).unwrap(), b"c d");
        let none = |_: &str| None::<&[u8]>;
        assert!(!unpack(&dir, none).unwrap(), "an APK without a payload copies nothing");
    }

    #[test]
    fn old_databases_move_into_db_once_and_never_overwrite() {
        let base =
            std::env::temp_dir().join(format!("yubigo_old_databases-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join(".upload_state.db"), "sent").unwrap();
        std::fs::write(base.join(".upload_state.db-journal"), "j").unwrap();
        std::fs::write(base.join("report-queue.db"), "old queue").unwrap();
        std::fs::create_dir_all(base.join(DB_DIR)).unwrap();
        std::fs::write(base.join(DB_DIR).join("report-queue.db"), "new queue").unwrap();

        adopt_old_databases(&base);

        let read = |p: &str| std::fs::read_to_string(base.join(DB_DIR).join(p)).unwrap();
        assert_eq!(read("upload_state.db"), "sent", "the record of what was sent survives");
        assert_eq!(read("upload_state.db-journal"), "j", "with its journal");
        assert!(!base.join(".upload_state.db").exists());
        assert_eq!(read("report-queue.db"), "new queue", "a database already there wins");
        assert!(base.join("report-queue.db").exists(), "and the old one is left alone");
    }

    #[test]
    fn only_an_empty_lock_directory_is_swept() {
        let base = std::env::temp_dir().join(format!("yubigo_stale_locks-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("report-queue.db.lock")).unwrap();
        std::fs::create_dir_all(base.join("upload_state.db.lock")).unwrap();
        std::fs::create_dir_all(base.join("episodes")).unwrap();
        std::fs::create_dir_all(base.join("full.lock")).unwrap();
        std::fs::write(base.join("full.lock/x"), "").unwrap();
        std::fs::write(base.join("notes.lock"), "").unwrap();
        std::fs::write(base.join("report-queue.db"), "").unwrap();

        clear_stale_locks(&base);

        assert!(!base.join("report-queue.db.lock").exists());
        assert!(!base.join("upload_state.db.lock").exists());
        assert!(base.join("episodes").exists(), "a directory that is not a lock is left alone");
        assert!(base.join("full.lock").exists(), "a lock that holds something is not SQLite's");
        assert!(base.join("notes.lock").exists(), "a file is not a lock directory");
        assert!(base.join("report-queue.db").exists(), "the database itself is untouched");
    }
}
