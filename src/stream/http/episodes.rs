//! The recorded takes: the list, their files whole or as a tar, what the
//! viewer plays from, and filing or deleting one.

use std::io::{Read, Write};
use std::net::TcpStream;

use super::response::{content_type, ranged, send, send_tar};
use crate::hub::EPISODE_DIR;

/// Every finished episode a host may take, newest first. Only finished ones:
/// an unfinished directory is deleted by the next startup, and serving it would
/// hand out a partial episode that then vanishes.
pub(super) fn list(takes: &str) -> String {
    let root = std::path::Path::new(takes);
    let list: Vec<serde_json::Value> = crate::hub::episodes::list(EPISODE_DIR)
        .into_iter()
        .filter(|name| crate::hub::episodes::is_complete(&root.join(name)))
        .map(|name| {
            let dir = root.join(&name);
            let files = plain_files(&dir);
            let bytes: u64 = files.iter().filter_map(|f| f["bytes"].as_u64()).sum();
            // What a person picks an episode by, so choosing does not mean
            // pulling all of them first.
            let meta: Option<serde_json::Value> =
                std::fs::read_to_string(dir.join(crate::hub::episodes::META))
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok());
            let episode = |key: &str| meta.as_ref().map(|m| m["episode"][key].clone());
            serde_json::json!({
                "name": name,
                "bytes": bytes,
                "files": files,
                "uuid": meta.as_ref().and_then(|m| m["uuid"].as_str()),
                "label": episode("label"),
                "success": episode("success"),
                "start_time": episode("start_time"),
                "folder": crate::hub::episodes::folder_of(&dir),
                "end_time": episode("end_time"),
            })
        })
        .collect();
    serde_json::Value::Array(list).to_string()
}

/// Plain files in one episode, as `{name, bytes}`.
fn plain_files(dir: &std::path::Path) -> Vec<serde_json::Value> {
    let mut out: Vec<serde_json::Value> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let meta = entry.metadata().ok().filter(|m| m.is_file())?;
            let name = entry.file_name().into_string().ok()?;
            Some(serde_json::json!({"name": name, "bytes": meta.len()}))
        })
        .collect();
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out
}

pub(super) fn serve_file(
    sock: &mut TcpStream,
    takes: &str,
    name: &str,
    file: &str,
    range: Option<&str>,
) -> std::io::Result<()> {
    use std::io::Seek;
    let path = std::path::Path::new(takes).join(name).join(file);
    let Ok(mut f) = std::fs::File::open(&path) else {
        return send(sock, 404, "text/plain", b"not found\n");
    };
    let len = f.metadata()?.len();
    let Some((start, end)) = ranged(sock, range, len, content_type(file))? else { return Ok(()) };
    f.seek(std::io::SeekFrom::Start(start))?;
    std::io::copy(&mut f.take(end - start), sock)?;
    Ok(())
}

/// One finished episode as an uncompressed tar of its directory. `meta.json`
/// goes last, so a download cut off part-way unpacks to a directory without
/// one, which is what an unfinished episode looks like everywhere else.
pub(super) fn serve_tar(sock: &mut TcpStream, takes: &str, name: &str) -> std::io::Result<()> {
    serve_tars(sock, takes, &[name.to_string()], &format!("{name}.tar"))
}

/// Finished episodes as one tar, a folder each, so a browser saves a selection
/// as one file. `folder.txt` stays out: it files the take in this viewer, and
/// a copy made for a computer has no use for it (`pull.py` keeps it, because
/// `yubi-export --folder` reads it there).
pub(super) fn serve_tars(
    sock: &mut TcpStream,
    takes: &str,
    names: &[String],
    filename: &str,
) -> std::io::Result<()> {
    use crate::hub::episodes::{is_complete, FOLDER, META};
    let mut files: Vec<(String, std::path::PathBuf, u64)> = Vec::new();
    for name in names {
        let dir = std::path::Path::new(takes).join(name);
        if !is_complete(&dir) {
            return send(sock, 404, "text/plain", format!("no such episode: {name}\n").as_bytes());
        }
        let mut take: Vec<_> = plain_files(&dir)
            .iter()
            .filter_map(|f| {
                let file = f["name"].as_str().filter(|&f| f != FOLDER)?;
                Some((format!("{name}/{file}"), dir.join(file), f["bytes"].as_u64()?))
            })
            .collect();
        // Last in its take, so a download cut short leaves no take that
        // looks finished.
        take.sort_by_key(|(_, path, _)| path.ends_with(META));
        files.extend(take);
    }
    send_tar(sock, filename, &files)
}

/// A finished episode's index, or `None` for one that is not on disk or
/// still being written.
fn playback(takes: &str, name: &str) -> Option<std::sync::Arc<crate::hub::episodes::Index>> {
    use crate::hub::episodes::{index, is_complete, SIGNALS};
    let dir = std::path::Path::new(takes).join(name);
    if !is_complete(&dir) {
        return None;
    }
    index(&dir.join(SIGNALS)).inspect_err(|e| log::warn!("cannot index {name}: {e}")).ok()
}

/// What the viewer plays from: frame times and the gripper traces.
pub(super) fn view(sock: &mut TcpStream, takes: &str, name: &str) -> std::io::Result<()> {
    match playback(takes, name) {
        Some(index) => send(sock, 200, "application/json", index.view().to_string().as_bytes()),
        None => send(sock, 404, "text/plain", b"no such episode\n"),
    }
}

/// `[{"failed": bool, "lost": bool}, ...]`, a subtask each, for the export
/// dialog's counts. Not while the rig captures, because reading a take's
/// signals works the disk and a core.
pub(super) fn subtasks(
    hub: &crate::hub::Hub,
    sock: &mut TcpStream,
    takes: &str,
    name: &str,
) -> std::io::Result<()> {
    if hub.capturing() {
        return send(sock, 409, "text/plain", b"the rig is armed or recording\n");
    }
    let dir = std::path::Path::new(takes).join(name);
    if !crate::hub::episodes::is_complete(&dir) {
        return send(sock, 404, "text/plain", b"no such episode\n");
    }
    match crate::hub::export::subtasks(&dir) {
        Ok(found) => {
            let found: Vec<_> = found
                .iter()
                .map(|&(failed, lost)| serde_json::json!({ "failed": failed, "lost": lost }))
                .collect();
            send(
                sock,
                200,
                "application/json",
                serde_json::Value::from(found).to_string().as_bytes(),
            )
        }
        Err(e) => {
            log::warn!("cannot read the subtasks of {name}: {e}");
            send(sock, 500, "text/plain", b"cannot read the subtasks\n")
        }
    }
}

pub(super) fn video(
    sock: &mut TcpStream,
    takes: &str,
    name: &str,
    slot: usize,
    range: Option<&str>,
) -> std::io::Result<()> {
    let Some(video) = playback(takes, name).and_then(|i| i.video(slot)) else {
        return send(sock, 404, "text/plain", b"no such video\n");
    };
    let Some((start, end)) = ranged(sock, range, video.len(), "video/mp4")? else {
        return Ok(());
    };
    let mut out = std::io::BufWriter::with_capacity(64 * 1024, sock);
    video.write(start, end, &mut out)?;
    out.flush()
}

pub(super) fn jpeg(
    sock: &mut TcpStream,
    takes: &str,
    name: &str,
    slot: usize,
    i: usize,
) -> std::io::Result<()> {
    match playback(takes, name).and_then(|x| x.jpeg(slot, i)) {
        Some(Ok(jpeg)) => send(sock, 200, "image/jpeg", &jpeg),
        Some(Err(e)) => {
            log::warn!("cannot read frame {i} of {name}: {e}");
            send(sock, 500, "text/plain", b"cannot read the frame\n")
        }
        None => send(sock, 404, "text/plain", b"no such frame\n"),
    }
}

/// `{"folder": "a/b"}` files one finished take under that folder.
pub(super) fn set_folder(takes: &str, name: &str, body: &[u8]) -> (u16, serde_json::Value) {
    let dir = std::path::Path::new(takes).join(name);
    let folder = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["folder"].as_str().map(str::to_string))
        .ok_or_else(|| "expected {\"folder\": \"a/b\"}".to_string())
        .and_then(|f| crate::hub::episodes::clean_folder(&f));
    match folder {
        _ if !crate::hub::episodes::is_complete(&dir) => {
            (404, serde_json::json!({ "error": "no such episode" }))
        }
        Err(e) => (400, serde_json::json!({ "error": e })),
        Ok(folder) => match crate::hub::episodes::set_folder(&dir, &folder) {
            Ok(()) => (200, serde_json::json!({ "folder": folder })),
            Err(e) => (500, serde_json::json!({ "error": e.to_string() })),
        },
    }
}

/// One take gone, unless it is still being recorded or finalized (until its
/// `meta.json` is written, the recorder is still writing into the directory),
/// the uploader is sending it, or an export may be reading it.
pub(super) fn delete_take(takes: &str, name: &str) -> (u16, &'static [u8]) {
    let dir = std::path::Path::new(takes).join(name);
    if dir.is_dir() && !crate::hub::episodes::is_complete(&dir) {
        return (409, br#"{"error": "this episode is still being written"}"#);
    }
    if crate::hub::export::running() {
        return (409, r#"{"error": "an export is running — wait for it to finish"}"#.as_bytes());
    }
    let remove = || std::fs::remove_dir_all(&dir);
    let Some(removed) = crate::hub::upload::unless_sending(&dir, remove) else {
        return (409, br#"{"error": "this episode is being uploaded; delete it after"}"#);
    };
    match removed {
        Ok(()) => {
            log::info!("episode {name} deleted over http");
            (200, br#"{"deleted": true}"#)
        }
        Err(e) => {
            log::warn!("cannot delete episode {name}: {e}");
            (404, br#"{"error": "no such episode"}"#)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_take_still_being_written_is_not_deleted() {
        let root = std::env::temp_dir().join(format!("yubigo_http_delete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("episode_1")).unwrap();
        std::fs::create_dir_all(root.join("episode_2")).unwrap();
        std::fs::write(root.join("episode_2").join(crate::hub::episodes::META), "{}").unwrap();
        let takes = root.to_str().unwrap();

        assert_eq!(delete_take(takes, "episode_1").0, 409);
        assert!(root.join("episode_1").is_dir());
        assert_eq!(delete_take(takes, "episode_2").0, 200);
        assert!(!root.join("episode_2").exists());
        assert_eq!(delete_take(takes, "episode_3").0, 404);
    }

    #[test]
    fn a_take_being_uploaded_is_not_deleted() {
        let root =
            std::env::temp_dir().join(format!("yubigo_http_delete_sending-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("episode_1")).unwrap();
        std::fs::write(root.join("episode_1").join(crate::hub::episodes::META), "{}").unwrap();
        let takes = root.to_str().unwrap();

        let take = root.join("episode_1");
        let sending = crate::hub::upload::sending(&take).unwrap();
        assert_eq!(delete_take(takes, "episode_1").0, 409);
        drop(sending);
        assert_eq!(delete_take(takes, "episode_1").0, 200);
        assert!(crate::hub::upload::sending(&take).is_none(), "gone, so not sent");
    }
}
