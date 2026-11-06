//! Upload state: which recordings have already been sent, and to where.
//!
//! [`crate::storage_gc`] and the prefix helpers in the crate root are
//! stateless one-liners. This is not: it names a database, so it lives
//! beside [`crate::backend::Client`] in shape as well as in spirit.

use crate::Error;

/// Upstream's `UploadStateDB`, keyed by recording name and target.
///
/// A recording is a take's directory name; a target is the storage.yaml
/// target it was sent to. Ask [`should_upload`](Self::should_upload) before
/// sending and record [`uploaded`](Self::uploaded) after, and a take is never
/// sent twice — no receipt file on disk, so nothing to lose track of when the
/// directory is reclaimed.
///
/// The database is upstream's SQLite file at `path`, a guest path under a
/// read-write [`add_preopen`](crate::add_preopen), as the node of the YUBI ROS
/// stack (yubi-sw) passes
/// `UploadStateDB(db_path)`. It outlives the process on its own. Every call
/// opens it afresh; see [`Client`](crate::backend::Client) for the lock
/// directory a force-stop can strand, and for why `path` must sit at least
/// one directory below the mount.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct UploadState {
    target: String,
    path: String,
}

/// Upstream's terminal statuses. The transient ones (`pending`, `uploading`,
/// `failed`) are not here: this host uploads one take at a time and re-asks
/// the disk on every poll, so a row that says "in progress" would only be a
/// second thing to keep true.
const COMPLETED: &str = "completed";
const GC_DELETED: &str = "gc_deleted";

impl UploadState {
    /// The state for `target` in the database at `path`, created on first use.
    pub fn new(target: &str, path: &str) -> Self {
        Self { target: target.to_string(), path: path.to_string() }
    }

    /// The target these answers are about.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// The database's guest path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// `upload_state_needing_upload` — whether one recording still wants
    /// sending. True for a recording the database has never heard of.
    pub fn should_upload(&self, recording: &str) -> Result<bool, Error> {
        Ok(!self.needing_upload(&[recording])?.is_empty())
    }

    /// The subset of `recordings` that still want sending, in the order given.
    /// One crossing however many are asked about.
    pub fn needing_upload(&self, recordings: &[&str]) -> Result<Vec<String>, Error> {
        let v = self.call("upload_state_needing_upload", serde_json::json!({}), recordings)?;
        serde_json::from_value(v["needs"].clone())
            .map_err(|e| Error::Python(format!("upload_state: unreadable answer: {e}")))
    }

    /// `upload_state_pending` — the recordings the database itself is holding
    /// open for this target. A recording it has no row for is not in here:
    /// what has never been tried is known from the disk, not from the state.
    pub fn pending(&self) -> Result<Vec<String>, Error> {
        let v = self.call("upload_state_pending", serde_json::json!({}), &[])?;
        serde_json::from_value(v["pending"].clone())
            .map_err(|e| Error::Python(format!("upload_state: unreadable answer: {e}")))
    }

    /// Record that these recordings reached the target. They are never
    /// offered again, whether or not their directories survive.
    pub fn uploaded(&self, recordings: &[&str]) -> Result<(), Error> {
        self.set(COMPLETED, recordings)
    }

    /// `upload_state_purge` — drop rows that have nothing left to say, and
    /// answer how many went. Upstream only removes `completed` and
    /// `gc_deleted`; a recording still owing an upload is never touched.
    pub fn purge(&self, max_age_hours: f64) -> Result<usize, Error> {
        let v = self.call(
            "upload_state_purge",
            serde_json::json!({"max_age_hours": max_age_hours}),
            &[],
        )?;
        Ok(v["deleted"].as_u64().unwrap_or_default() as usize)
    }

    /// Record that these recordings were reclaimed locally after upload.
    /// Upstream keeps this apart from `completed` so a directory reappearing
    /// under an old name is still not sent again.
    pub fn gc_deleted(&self, recordings: &[&str]) -> Result<(), Error> {
        self.set(GC_DELETED, recordings)
    }

    /// Record that this recording reached the target, keeping the ETag each
    /// of its objects came back with. Upstream keys them by object name.
    ///
    /// The ETags are the only record that ties what is in the bucket to what
    /// this rig believes it sent: without them a re-upload, a truncated
    /// object or a bucket-side edit all read the same from here.
    pub fn uploaded_etagged(
        &self,
        recording: &str,
        etags: &serde_json::Value,
    ) -> Result<(), Error> {
        self.write(COMPLETED, &[recording], etags)
    }

    fn set(&self, status: &str, recordings: &[&str]) -> Result<(), Error> {
        self.write(status, recordings, &serde_json::Value::Null)
    }

    fn write(
        &self,
        status: &str,
        recordings: &[&str],
        etags: &serde_json::Value,
    ) -> Result<(), Error> {
        if recordings.is_empty() {
            return Ok(());
        }
        self.call(
            "upload_state_set",
            serde_json::json!({"status": status, "etags": etags}),
            recordings,
        )
        .map(|_| ())
    }

    fn call(
        &self,
        op: &str,
        extra: serde_json::Value,
        recordings: &[&str],
    ) -> Result<serde_json::Value, Error> {
        let mut args = serde_json::json!({
            "target": self.target,
            "recordings": recordings,
            "upload_state_path": self.path,
        });
        let fields = args.as_object_mut().expect("built as an object");
        for (k, v) in extra.as_object().expect("built as an object") {
            fields.insert(k.clone(), v.clone());
        }
        crate::call(op, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh database for one test, on its own read-write mount. The
    /// directory is never removed: a worker spawned by another test opens
    /// every mount at once.
    fn state(name: &str, target: &str) -> UploadState {
        let payload = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("build");
        crate::start(&payload).unwrap_or_else(|e| panic!("{e}\n"));
        let base = std::env::temp_dir().join(format!("airoa_sdk_states-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        crate::add_preopen(&base, "/states", crate::Access::ReadWrite);
        let _ = std::fs::remove_file(base.join("db").join(format!("{name}.db")));
        UploadState::new(target, &format!("/states/db/{name}.db"))
    }

    #[test]
    fn a_recording_nobody_recorded_still_wants_uploading() {
        let state = state("nobody", "rig");
        assert!(state.should_upload("episode_1").unwrap());
        assert_eq!(
            state.needing_upload(&["episode_1", "episode_2"]).unwrap(),
            ["episode_1", "episode_2"]
        );
        assert!(state.pending().unwrap().is_empty());
    }

    #[test]
    fn a_recording_recorded_as_uploaded_is_not_offered_again() {
        let state = state("recorded", "rig");
        state.uploaded(&["episode_1"]).unwrap();
        assert!(!state.should_upload("episode_1").unwrap());
        assert_eq!(state.needing_upload(&["episode_1", "episode_2"]).unwrap(), ["episode_2"]);

        // Another target is another question entirely.
        let elsewhere = UploadState::new("backup", state.path());
        assert!(elsewhere.should_upload("episode_1").unwrap());

        state.gc_deleted(&["episode_2"]).unwrap();
        assert!(!state.should_upload("episode_2").unwrap(), "reclaimed is not re-sent");
        assert!(state.pending().unwrap().is_empty(), "neither status wants action");
    }

    #[test]
    fn the_gc_collects_what_the_database_calls_uploaded_with_no_marker_on_disk() {
        let state = state("gc", "rig");
        let base = std::env::temp_dir().join(format!("airoa_sdk_gc_state-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        for name in ["episode_1", "episode_2"] {
            let dir = base.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("meta.json"), "{}").unwrap();
        }
        crate::add_preopen(&base, "/gc_state_probe", crate::Access::ReadWrite);

        state.uploaded(&["episode_1"]).unwrap();
        let gc = serde_json::json!({
            "strategy": ["marker"],
            "completion_marker": "meta.json",
            "orphan_age_hours": 0,
        });
        let v = crate::storage_gc("/gc_state_probe", &gc, Some(&state)).unwrap();

        assert_eq!(v["deleted"], serde_json::json!(["episode_1"]), "{v}");
        assert!(!base.join("episode_1").exists(), "the uploaded episode is reclaimed");
        assert!(base.join("episode_2").exists(), "the unsent episode is kept");
        assert!(
            std::fs::read_dir(base.join("episode_2")).unwrap().count() == 1,
            "and no receipt file was ever needed"
        );
    }

    #[test]
    fn the_state_file_survives_a_process_that_does_not() {
        let state = state("survives", "rig");
        state.uploaded(&["episode_1"]).unwrap();
        let path = state.path().to_string();
        drop(state);

        let restored = UploadState::new("rig", &path);
        assert!(!restored.should_upload("episode_1").unwrap());
        assert!(restored.should_upload("episode_9").unwrap());
    }
}
