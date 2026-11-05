use std::path::Path;

use log::{info, warn};

use super::config::{host_of, PathRule, UploadConfig, GUEST_ROOT};
use super::state::{self, State};

const MARKER: &str = ".recording_complete";

pub(super) fn probe(target: &UploadConfig) -> State {
    let name = format!("{}/{}", host_of(&target.endpoint), target.bucket);
    let url = match super::s3::presign_probe(target) {
        Ok(url) => url,
        Err(e) => return State::NoTarget(format!("upload endpoint unusable: {e}")),
    };
    verdict(&name, super::s3::agent().get(url.as_str()).call())
}

pub(super) fn verdict(name: &str, answer: Result<ureq::Response, ureq::Error>) -> State {
    match answer {
        Ok(_) => State::Idle(name.to_string()),
        Err(ureq::Error::Status(code, response)) => {
            refused(name, Some(code), &response.into_string().unwrap_or_default())
        }
        Err(ureq::Error::Transport(t)) => refused(name, None, &super::s3::transport(&t)),
    }
}

fn refused(name: &str, code: Option<u16>, body: &str) -> State {
    State::Unreachable(match code {
        Some(403) => format!("{name}: credentials rejected (403)"),
        Some(404) => format!("{name}: no such bucket"),
        Some(code) => format!("{name} → {code}: {}", body.trim()),
        None => body.trim().to_string(),
    })
}

/// The prefix it landed under, and the ETag every object came back with.
/// [`super::s3::PAUSED`] when `stop` turned true between files or parts.
pub(super) fn send(
    target: &UploadConfig,
    episode: &Path,
    stop: &dyn Fn() -> bool,
) -> Result<(String, serde_json::Value), String> {
    let prefix = s3_prefix(target, episode)?;
    let files = contents(episode);
    if files.is_empty() {
        return Err("no files to send".to_string());
    }
    let agent = super::s3::agent();
    let mut manifest = serde_json::Map::new();
    for (done, (name, _size)) in files.iter().enumerate() {
        if stop() {
            return Err(super::s3::PAUSED.to_string());
        }
        state::set(State::Sending { done, total: files.len() });
        let key = format!("{prefix}{name}");
        let etag = super::s3::put_file(&agent, target, &key, &episode.join(name), stop)?;
        manifest.insert(name.clone(), etag.into());
    }
    let marker = serde_json::json!({
        "completed_at": crate::misc::clock::rfc3339(crate::misc::clock::realtime_ns()),
        "files": manifest,
    })
    .to_string();
    super::s3::put_bytes(&agent, target, &format!("{prefix}{MARKER}"), marker.as_bytes())?;
    Ok((prefix, manifest.into()))
}

pub(super) fn settle(
    target: &UploadConfig,
    episode: &Path,
    prefix: &str,
    etags: &serde_json::Value,
    upload_state: &airoa_sdk::storage::UploadState,
) {
    state::set(State::Idle(format!("{}/{}", host_of(&target.endpoint), target.bucket)));
    let name = episode.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    match upload_state.uploaded_etagged(name, etags) {
        Ok(()) => state::note_sent([name.to_string()]),
        Err(e) => warn!("upload: {name} at {prefix} not recorded: {e} — it will be sent again"),
    }
    if !target.delete_after_upload {
        info!("upload: {} stored at {prefix}", episode.display());
        return;
    }
    match std::fs::remove_dir_all(episode) {
        Ok(()) => info!("upload: {} stored at {prefix}, local copy reclaimed", episode.display()),
        Err(e) => warn!("upload: {} stored at {prefix} but not reclaimed: {e}", episode.display()),
    }
}

/// Every file in the take, as upstream's own listing does it: the completion
/// marker is the only name skipped. Upstream stops at the top level because a
/// rosbag has no subdirectories. One level down is walked too, because takes
/// from pre-release builds carry `videos/`. A file being
/// written beside its target (`.name.tmp`, as `misc::save::replace` writes)
/// is skipped too, because it is renamed away mid-send.
pub(super) fn contents(episode: &Path) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(episode) else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == MARKER || (name.starts_with('.') && name.ends_with(".tmp")) {
            continue;
        }
        match entry.metadata() {
            Ok(meta) if meta.is_file() => out.push((name, meta.len())),
            Ok(meta) if meta.is_dir() => {
                let Ok(inner) = std::fs::read_dir(entry.path()) else { continue };
                for file in inner.flatten() {
                    let child = file.file_name().to_string_lossy().into_owned();
                    if let Ok(meta) = file.metadata() {
                        if meta.is_file() {
                            out.push((format!("{name}/{child}"), meta.len()));
                        }
                    }
                }
            }
            _ => {}
        }
    }
    out.sort();
    out
}

fn s3_prefix(target: &UploadConfig, episode: &Path) -> Result<String, String> {
    let name = episode.file_name().and_then(|n| n.to_str()).ok_or("episode dir has no name")?;
    let rule = match target.path_rule {
        PathRule::Flat => "flat",
        PathRule::Canonical => "canonical",
    };
    let guest = format!("{}/{name}", crate::record::record::takes(GUEST_ROOT));
    airoa_sdk::resolve_s3_prefix(&target.prefix, &guest, rule).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::s3::tests::minio_target;
    use super::*;
    use crate::test_http::{files_root, up};

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

    #[test]
    fn a_settled_upload_writes_no_receipt_file_and_survives_a_restart() {
        core();
        let base = temp("settle");
        let dir = base.to_str().unwrap();
        let sent = episode(&base, "episode_1", true);
        airoa_sdk::add_preopen(&base, "/settle_probe", airoa_sdk::Access::ReadWrite);
        let state = airoa_sdk::storage::UploadState::new("rig", "/settle_probe/db/upload_state.db");

        let etags = serde_json::json!({"meta.json": "\"abc\""});
        settle(&minio_target(), &sent, "yubi-go/episode_1/", &etags, &state);
        assert!(sent.exists(), "delete_after_upload is off, so the take stays");
        assert!(!sent.join(".uploaded").exists(), "and no receipt file is written");
        assert!(base.join("db/upload_state.db").exists(), "upstream wrote the record to disk");

        // What a restarted process would open: the same file, nothing carried over.
        let restarted =
            airoa_sdk::storage::UploadState::new("rig", "/settle_probe/db/upload_state.db");
        drop(state);
        assert!(
            super::super::state::pending(dir, &restarted).is_empty(),
            "the record outlived the process"
        );
    }

    #[test]
    fn the_nested_video_is_sent_and_the_completion_marker_is_not() {
        let base = temp("contents");
        let dir = episode(&base, "episode_1", true);
        std::fs::create_dir_all(dir.join("videos")).unwrap();
        std::fs::write(dir.join("videos/head.mp4"), [0u8; 16]).unwrap();
        // Upstream skips exactly one name and takes every other file, dot or not.
        std::fs::write(dir.join(MARKER), "x").unwrap();
        std::fs::write(dir.join(".uploaded"), "x").unwrap();
        std::fs::write(dir.join(".meta.json.tmp"), "{").unwrap();
        assert_eq!(
            contents(&dir),
            vec![
                (".uploaded".to_string(), 1),
                ("meta.json".to_string(), META.len() as u64),
                ("signals.mcap".to_string(), 16),
                ("videos/head.mp4".to_string(), 16),
            ]
        );
    }

    #[test]
    fn the_prefix_is_resolved_by_airoa_core_through_the_sandbox_mount() {
        up();
        let dir = episode(&files_root(), "episode_1787154123798", true);
        let target = UploadConfig { prefix: "yubi-go/".into(), ..minio_target() };
        assert_eq!(s3_prefix(&target, &dir).unwrap(), "yubi-go/episode_1787154123798/");

        let target = UploadConfig { path_rule: PathRule::Canonical, ..target };
        let prefix = s3_prefix(&target, &dir).unwrap();
        assert!(prefix.starts_with("yubi-go/org="), "{prefix}");
        assert!(prefix.ends_with("/uuid=abc-123/"), "{prefix}");
    }

    #[test]
    fn a_rejected_credential_reads_differently_from_a_network_that_is_gone() {
        let name = "minio/data";
        let why = |code, body| match refused(name, code, body) {
            State::Unreachable(why) => why,
            other => panic!("a failed probe is not {other:?}"),
        };
        assert!(why(None, "connection refused").contains("refused"));
        assert!(why(Some(403), "").contains("credentials"));
        assert!(why(Some(404), "").contains("no such bucket"));
        let other = why(Some(502), "<html>bad gateway");
        assert!(other.contains("502") && other.contains("bad gateway"), "{other}");

        let ok = ureq::Response::new(200, "OK", "").unwrap();
        assert_eq!(verdict(name, Ok(ok)), State::Idle(name.to_string()));
    }
}
