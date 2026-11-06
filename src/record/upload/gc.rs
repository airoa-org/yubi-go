use std::time::{Duration, Instant};

use log::{info, warn};

use super::config::{storage_settings, StorageSettings, GUEST_ROOT};

/// How often the housekeeping below runs. Upstream's retention timer is 300 s
/// and this matches it; the errands are cheap and both are age-based, so the
/// exact cadence does not matter as long as it is not per-poll.
const HOUSEKEEPING: Duration = Duration::from_secs(300);

/// How often core's GC runs when the `gc:` block gives no
/// `check_interval_sec`: the same 300 s as the retention timer above.
const GC_DEFAULT_INTERVAL: Duration = Duration::from_secs(300);

/// Upstream's `retention_cleanup`, in its own terms: **directory mtime is the
/// age**, `delete_after_upload` turns it off, zero hours turns it off, and a
/// take that still owes an upload is kept past its limit. A take with no
/// `meta.json` is an orphan and goes on age alone, without consulting the
/// state.
///
/// Without this, a `storage.yaml` whose target carries no `gc:` block
/// reclaims nothing at all: `gc_tick` returns immediately, the disk fills,
/// and the gate refuses to start a take once free space falls under
/// `gate::SPACE_FLOOR`. That reads in the field as "it suddenly stopped
/// recording", which is why this is worth its own sweep rather than waiting
/// for everyone to configure a GC block.
fn retention_tick(dir: &str, settings: &StorageSettings, state: &airoa_sdk::storage::UploadState) {
    if settings.delete_after_upload {
        return;
    }
    let Some(retention) = settings.retention else { return };
    let takes = crate::record::record::takes(dir);
    let Ok(entries) = std::fs::read_dir(&takes) else { return };
    let mut reclaimed: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let too_old = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|at| at.elapsed().unwrap_or_default() > retention)
            .unwrap_or(false);
        if !too_old {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // An unsent take is kept however old it is: reclaiming it would lose
        // the recording outright. An orphan has no meta.json and no claim.
        if crate::record::record::is_complete(&path) && state.should_upload(&name).unwrap_or(true) {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => reclaimed.push(name),
            Err(e) => warn!("retention: {} not reclaimed: {e}", path.display()),
        }
    }
    if reclaimed.is_empty() {
        return;
    }
    info!("retention: reclaimed {} — past {:?}", reclaimed.join(", "), retention);
    let refs: Vec<&str> = reclaimed.iter().map(String::as_str).collect();
    if let Err(e) = state.gc_deleted(&refs) {
        warn!("retention: reclaim not recorded: {e} — those takes may be sent again");
    }
}

/// Drop upload-state rows that have nothing left to say. Runs after the sweep
/// above, so a row is only ever forgotten once the directory it describes has
/// already gone — forgetting it first would offer the take for upload again.
fn purge_tick(settings: &StorageSettings, state: &airoa_sdk::storage::UploadState) {
    match state.purge(settings.state_purge_age_hours) {
        Ok(0) => {}
        Ok(n) => info!("upload: {n} settled row(s) purged from the upload state"),
        Err(e) => warn!("upload: state not purged: {e}"),
    }
}

pub(super) fn gc_tick(
    dir: &str,
    last: &mut Option<Instant>,
    recording: &impl Fn() -> bool,
    state: &airoa_sdk::storage::UploadState,
) {
    let Some(gc) = storage_settings(dir, GUEST_ROOT).ok().and_then(|s| s.gc) else { return };
    if recording() || !crate::misc::core::up() {
        return;
    }
    let interval =
        gc["check_interval_sec"].as_u64().map(Duration::from_secs).unwrap_or(GC_DEFAULT_INTERVAL);
    if last.is_some_and(|t| t.elapsed() < interval) {
        return;
    }
    *last = Some(Instant::now());
    run_gc(&gc, state);
}

fn run_gc(gc: &serde_json::Value, state: &airoa_sdk::storage::UploadState) {
    let mut gc = gc.clone();
    if let Some(body) = gc.as_object_mut() {
        // meta.json is the completion marker: `finish` writes it last, and a
        // take that did not close cleanly deleted itself. There is no
        // storage_marker to name, because "already stored" is the upload
        // state's answer.
        body.entry("completion_marker").or_insert(crate::record::record::META.into());
    }
    match airoa_sdk::storage_gc(&crate::record::record::takes(GUEST_ROOT), &gc, Some(state)) {
        Ok(v) => {
            let deleted: Vec<&str> =
                v["deleted"].as_array().into_iter().flatten().filter_map(|d| d.as_str()).collect();
            if deleted.is_empty() {
                return;
            }
            let dry = v["dry_run"].as_bool() == Some(true);
            let would = match dry {
                true => "[dry run] would have ",
                false => "",
            };
            info!("gc: {would}deleted {}", deleted.join(", "));
            if dry {
                return;
            }
            if let Err(e) = state.gc_deleted(&deleted) {
                warn!("gc: {} reclaimed but not recorded: {e}", deleted.join(", "));
            }
        }
        Err(e) => warn!("gc: cycle failed: {e}"),
    }
}

/// Retention and the state purge, on one clock. Never while recording: both
/// walk the takes directory, and a take being written is the one thing that
/// must not be touched.
pub(super) fn housekeeping(
    dir: &str,
    last: &mut Option<Instant>,
    recording: &impl Fn() -> bool,
    state: &airoa_sdk::storage::UploadState,
) {
    if recording() || !crate::misc::core::up() {
        return;
    }
    if last.is_some_and(|t| t.elapsed() < HOUSEKEEPING) {
        return;
    }
    let Ok(settings) = storage_settings(dir, GUEST_ROOT) else { return };
    *last = Some(Instant::now());
    retention_tick(dir, &settings, state);
    purge_tick(&settings, state);
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yubigo_upload_{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const META: &str = r#"{"uuid":"abc-123"}"#;

    /// `complete` writes meta.json, which is what marks a take finished.
    fn episode(base: &Path, name: &str, complete: bool) -> PathBuf {
        let dir = base.join(crate::record::record::TAKES).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(crate::record::record::SIGNALS), [0u8; 16]).unwrap();
        if complete {
            std::fs::write(dir.join("meta.json"), META).unwrap();
        }
        dir
    }

    fn core() {
        let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("airoa-sdk/build");
        airoa_sdk::start(&payload)
            .expect("the payload and its cwasm must both be built — see the error");
    }

    fn settings(retention_hours: u32) -> StorageSettings {
        StorageSettings {
            name: "rig".into(),
            target: None,
            gc: None,
            retention: match retention_hours {
                0 => None,
                h => Some(Duration::from_secs(h as u64 * 3600)),
            },
            delete_after_upload: false,
            state_purge_age_hours: 720.0,
        }
    }

    /// Age a directory by moving its mtime back, which is the only thing
    /// upstream's retention looks at. `touch -t` rather than a crate: the
    /// format is POSIX, so it is the one way to do this without a dependency
    /// that exists solely for two tests.
    fn age(dir: &Path, hours: u64) {
        let then = crate::misc::clock::realtime_ns() - hours * 3_600_000_000_000;
        // "2026-09-10T01:23:45.000Z" -> "202609100123.45"
        let iso = crate::misc::clock::rfc3339(then);
        let stamp = format!(
            "{}{}{}{}{}.{}",
            &iso[0..4],
            &iso[5..7],
            &iso[8..10],
            &iso[11..13],
            &iso[14..16],
            &iso[17..19]
        );
        let out = std::process::Command::new("touch")
            .args(["-t", &stamp])
            .arg(dir)
            .output()
            .expect("touch must exist");
        assert!(out.status.success(), "touch -t {stamp}: {}", String::from_utf8_lossy(&out.stderr));
    }

    /// The gap that fills the headset: a `storage.yaml` with no `gc:` block
    /// reclaims nothing, so this sweep has to stand on its own.
    #[test]
    fn retention_reclaims_a_sent_take_and_keeps_one_still_owing_an_upload() {
        core();
        let base = temp("retention");
        let dir = base.to_str().unwrap();
        let sent = episode(&base, "episode_1", true);
        let unsent = episode(&base, "episode_2", true);
        let fresh = episode(&base, "episode_3", true);
        let orphan = episode(&base, "episode_4", false);
        airoa_sdk::add_preopen(&base, "/retention_probe", airoa_sdk::Access::ReadWrite);

        let state = airoa_sdk::storage::UploadState::new("rig", "/retention_probe/db/state.db");
        state.uploaded_etagged("episode_1", &serde_json::Value::Null).unwrap();
        for old in [&sent, &unsent, &orphan] {
            age(old, 48);
        }

        retention_tick(dir, &settings(24), &state);

        assert!(!sent.exists(), "a take past its limit that has landed is reclaimed");
        assert!(unsent.exists(), "a take that still owes an upload is kept however old");
        assert!(fresh.exists(), "a take inside its limit is untouched");
        assert!(!orphan.exists(), "no meta.json means no claim — age alone decides");

        // And the reclaim is recorded, so it is never offered for upload again.
        assert!(!state.should_upload("episode_1").unwrap());
    }

    #[test]
    fn retention_is_off_when_the_config_says_so() {
        core();
        let base = temp("retention-off");
        let dir = base.to_str().unwrap();
        let sent = episode(&base, "episode_1", true);
        airoa_sdk::add_preopen(&base, "/retention_off_probe", airoa_sdk::Access::ReadWrite);
        let state = airoa_sdk::storage::UploadState::new("rig", "/retention_off_probe/db/state.db");
        state.uploaded_etagged("episode_1", &serde_json::Value::Null).unwrap();
        age(&sent, 48);

        retention_tick(dir, &settings(0), &state);
        assert!(sent.exists(), "zero hours is upstream's way of disabling the sweep");

        retention_tick(dir, &StorageSettings { delete_after_upload: true, ..settings(24) }, &state);
        assert!(sent.exists(), "delete_after_upload owns the reclaim when it is on");
    }
}
