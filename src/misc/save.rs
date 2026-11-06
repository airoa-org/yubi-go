//! Saving state that has to survive a force-stop.
//!
//! Replace a file's contents, or leave the old ones alone.
//!
//! A force-stop is routine on this device, and `std::fs::write` truncates
//! before it writes: a process killed in that window leaves a half-written
//! file where a whole one used to be. That is survivable for a log and not
//! for the id of the episode still open at the backend.
//!
//! Writing beside the target and renaming over it is the usual answer: a
//! rename within a directory is atomic, so a reader sees either every old
//! byte or every new one.

use std::io::Write;
use std::path::Path;

/// Write `bytes` to `path` through a neighbouring temporary file.
///
/// The temporary carries the target's name so a crash between the write and
/// the rename leaves something identifiable rather than a bare `tmp`.
pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let tmp = path.with_file_name(format!(".{name}.tmp"));
    let mut file = std::fs::File::create(&tmp)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = written {
        drop(std::fs::remove_file(&tmp));
        return Err(e);
    }
    // Rename last: until it lands the old contents are still whole.
    std::fs::rename(&tmp, path).inspect_err(|_| drop(std::fs::remove_file(&tmp)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_replaced_file_is_never_seen_half_written() {
        let dir = std::env::temp_dir().join(format!("yubigo_atomic-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");

        replace(&path, b"first").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"first");

        // A shorter second write must not leave the first one's tail behind.
        replace(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");

        // And nothing is left beside it for the next reader to trip over.
        let strays: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "state.db")
            .collect();
        assert!(strays.is_empty(), "{strays:?}");
    }

    #[test]
    fn a_failed_write_leaves_the_old_contents_whole() {
        let dir = std::env::temp_dir().join(format!("yubigo_atomic_fail-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        replace(&path, b"good").unwrap();

        // A directory cannot be created where the temporary wants to go.
        std::fs::create_dir(dir.join(".state.db.tmp")).unwrap();
        assert!(replace(&path, b"bad").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"good");
    }
}
