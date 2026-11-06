use std::path::Path;
use std::time::Duration;

use log::warn;

#[derive(Clone, PartialEq)]
pub struct UploadConfig {
    pub endpoint: String,
    pub bucket: String,
    pub access_key: String,
    pub secret_key: String,
    pub region: String,
    pub prefix: String,
    pub path_rule: PathRule,
    pub delete_after_upload: bool,
}

impl std::fmt::Debug for UploadConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use crate::misc::config::redacted;
        f.debug_struct("UploadConfig")
            .field("endpoint", &self.endpoint)
            .field("bucket", &self.bucket)
            .field("access_key", &redacted(&self.access_key))
            .field("secret_key", &redacted(&self.secret_key))
            .field("region", &self.region)
            .field("prefix", &self.prefix)
            .field("path_rule", &self.path_rule)
            .field("delete_after_upload", &self.delete_after_upload)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PathRule {
    #[default]
    Flat,
    Canonical,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StorageSettings {
    /// The configured target's name, which keys the upload state — present
    /// even when the target itself is unusable, so the GC can still be asked
    /// what has been stored.
    pub name: String,
    pub target: Option<UploadConfig>,
    pub gc: Option<serde_json::Value>,
    /// How long a sent take stays on disk, when `delete_after_upload` is off.
    pub retention: Option<Duration>,
    pub delete_after_upload: bool,
    pub state_purge_age_hours: f64,
}

const STORAGE_FILE: &str = "storage.yaml";
/// No file is the operator choosing to keep episodes on the device, not a fault.
pub const NO_FILE: &str = "no storage.yaml";
/// A file that is there but cannot be read until airoa_core is up.
const WAITING: &str = "waiting for airoa_core";

pub(crate) use crate::misc::core::GUEST_ROOT;

static STORAGE: std::sync::Mutex<Option<(String, std::time::SystemTime, StorageState)>> =
    std::sync::Mutex::new(None);

type StorageState = Result<StorageSettings, String>;

/// When `storage.yaml` last changed; `None` while there is no file.
pub(super) fn storage_modified(dir: &str) -> Option<std::time::SystemTime> {
    std::fs::metadata(Path::new(dir).join(STORAGE_FILE)).and_then(|m| m.modified()).ok()
}

pub(super) fn storage_settings(dir: &str, guest_root: &str) -> StorageState {
    let modified = storage_modified(dir).ok_or_else(|| NO_FILE.to_string())?;
    if let Ok(cache) = STORAGE.lock() {
        if let Some((had_dir, at, state)) = cache.as_ref() {
            if had_dir == dir && *at == modified {
                return state.clone();
            }
        }
    }
    if !crate::misc::core::up() {
        return Err(WAITING.to_string());
    }
    let state = airoa_sdk::load_storage_config(&format!("{guest_root}/{STORAGE_FILE}"))
        .map(settings_from)
        .map_err(|e| format!("storage.yaml unusable: {e}"));
    if let Ok(mut cache) = STORAGE.lock() {
        *cache = Some((dir.to_string(), modified, state.clone()));
    }
    state
}

fn settings_from(cfg: airoa_sdk::StorageConfig) -> StorageSettings {
    if cfg.targets.len() > 1 {
        warn!("storage.yaml: {} targets configured — only the first is used", cfg.targets.len());
    }
    let Some(t) = cfg.targets.into_iter().next() else { return StorageSettings::default() };
    let name = t.name.clone();
    if t.endpoint.is_empty() || t.access_key.is_empty() || t.secret_key.is_empty() {
        warn!("storage.yaml: target {} has no endpoint or credentials", t.name);
        return StorageSettings {
            name,
            target: None,
            gc: t.gc,
            retention: match cfg.local_retention_hours {
                0 => None,
                hours => Some(Duration::from_secs(hours as u64 * 3600)),
            },
            delete_after_upload: cfg.delete_after_upload,
            state_purge_age_hours: cfg.state_purge_age_hours,
        };
    }
    let endpoint = match t.endpoint.contains("://") {
        true => t.endpoint.trim_end_matches('/').to_string(),
        false => match t.use_ssl {
            true => format!("https://{}", t.endpoint),
            false => format!("http://{}", t.endpoint),
        },
    };
    let target = Some(UploadConfig {
        endpoint,
        bucket: t.bucket,
        access_key: t.access_key,
        secret_key: t.secret_key,
        region: t.region,
        prefix: t.prefix,
        path_rule: match t.path_rule.as_str() {
            "canonical" => PathRule::Canonical,
            _ => PathRule::Flat,
        },
        delete_after_upload: cfg.delete_after_upload,
    });
    StorageSettings {
        name,
        target,
        gc: t.gc,
        // Upstream treats zero as "never sweep", and so does the tick.
        retention: match cfg.local_retention_hours {
            0 => None,
            hours => Some(Duration::from_secs(hours as u64 * 3600)),
        },
        delete_after_upload: cfg.delete_after_upload,
        state_purge_age_hours: cfg.state_purge_age_hours,
    }
}

pub(super) fn target(dir: &str, said: &mut Option<String>) -> Option<UploadConfig> {
    let settings = match storage_settings(dir, GUEST_ROOT) {
        Ok(settings) => settings,
        Err(why) => {
            super::state::stop(
                said,
                &why,
                &format!("upload: {why} — episodes stay on the device for a pull"),
            );
            return None;
        }
    };
    let Some(target) = settings.target else {
        super::state::stop(
            said,
            "no usable target in storage.yaml",
            "upload: no usable target in storage.yaml — episodes stay on the device for a pull",
        );
        return None;
    };
    if let Err(e) = check_endpoint(&target.endpoint) {
        let why = format!("upload endpoint unusable: {e}");
        super::state::stop(said, &why, &format!("upload: {why}"));
        return None;
    }
    super::state::say(
        said,
        format!("upload: queue running against {}/{}", target.endpoint, target.bucket),
    );
    Some(target)
}

/// What the :8080 page shows of the target. Never the keys.
pub fn shown(dir: &str) -> Option<serde_json::Value> {
    let t = storage_settings(dir, GUEST_ROOT).ok()?.target?;
    Some(serde_json::json!({
        "endpoint": t.endpoint,
        "bucket": t.bucket,
        "prefix": t.prefix,
        "region": t.region,
        "delete_after_upload": t.delete_after_upload,
    }))
}

/// Write `storage.yaml` from the page's form, or remove it on `{"off": true}`.
/// A blank key keeps the current one while the endpoint stays on the same
/// host, and the keys the form does not show
/// (`gc`, retention, path rule, target name) are carried over.
pub fn save(dir: &str, body: &[u8]) -> Result<(), String> {
    save_at(dir, GUEST_ROOT, body)
}

fn save_at(dir: &str, guest_root: &str, body: &[u8]) -> Result<(), String> {
    let path = Path::new(dir).join(STORAGE_FILE);
    let form: serde_json::Value =
        serde_json::from_slice(body).map_err(|e| format!("not valid JSON: {e}"))?;
    if form["off"] == true {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("cannot remove storage.yaml: {e}"))
            }
            _ => Ok(()),
        };
    }
    let field = |key: &str| form[key].as_str().unwrap_or("").trim().to_string();
    let endpoint = field("endpoint").trim_end_matches('/').to_string();
    if !endpoint.contains("://") {
        return Err(format!("endpoint needs http:// or https:// in front: {endpoint}"));
    }
    check_endpoint(&endpoint)?;
    let bucket = field("bucket");
    if bucket.is_empty() {
        return Err("bucket is empty".into());
    }
    let had = known(storage_settings(dir, guest_root))?;
    let had_target = had.as_ref().and_then(|h| h.target.as_ref());
    use crate::misc::config::origin;
    // The keys are sent wherever the endpoint points, so a kept key only
    // follows an endpoint on the same host, as backend.api_key does.
    let kept = had_target;
    let moved = kept.is_some_and(|t| origin(&t.endpoint) != origin(&endpoint));
    let key = |key: &str, kept: Option<&String>| match field(key) {
        k if !k.is_empty() => Ok(k),
        _ if moved => Err("the endpoint is on another host now — enter the access key and \
                           secret key again to use them there"
            .to_string()),
        _ => kept.cloned().ok_or(format!("{} is empty", key.replace('_', " "))),
    };
    let access_key = key("access_key", kept.map(|t| &t.access_key))?;
    let secret_key = key("secret_key", kept.map(|t| &t.secret_key))?;
    // JSON strings and objects are valid YAML, so nothing typed here can
    // break the file's structure.
    let q = |s: &str| serde_json::Value::from(s).to_string();
    let name = had.as_ref().map(|h| h.name.clone()).filter(|n| !n.is_empty());
    let mut yaml = format!(
        "targets:\n  {}:\n    endpoint: {}\n    access_key: {}\n    secret_key: {}\n    bucket: {}\n    prefix: {}\n",
        q(name.as_deref().unwrap_or("rig")),
        q(&endpoint),
        q(&access_key),
        q(&secret_key),
        q(&bucket),
        q(&field("prefix")),
    );
    if !field("region").is_empty() {
        yaml += &format!("    region: {}\n", q(&field("region")));
    }
    // canonical when there is nothing to keep, as docs/manual.md's template.
    let flat = had_target.is_some_and(|t| t.path_rule == PathRule::Flat);
    yaml += &format!("    path_rule: {}\n", if flat { "flat" } else { "canonical" });
    if let Some(gc) = had.as_ref().and_then(|h| h.gc.as_ref()) {
        yaml += &format!("    gc: {gc}\n");
    }
    yaml += &format!("delete_after_upload: {}\n", form["delete_after_upload"] == true);
    if let Some(had) = &had {
        // A parsed file always has a retention, so `None` is the file's own 0.
        let hours = had.retention.map_or(0, |d| d.as_secs() / 3600);
        yaml += &format!("local_retention_hours: {hours}\n");
        yaml += &format!("state_purge_age_hours: {}\n", had.state_purge_age_hours);
    }
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml)
        .and_then(|_| std::fs::rename(&tmp, &path))
        .map_err(|e| format!("cannot write storage.yaml: {e}"))
}

/// What a save carries over. Saving blind would rename the target and drop
/// the keys the form does not show, and a renamed target sends every episode
/// again, so a file airoa_core has not read yet refuses the save.
fn known(state: StorageState) -> Result<Option<StorageSettings>, String> {
    match state {
        Ok(had) => Ok(Some(had)),
        Err(e) if e == WAITING => Err("airoa_core is still starting, so the saved storage \
                                       settings cannot be read yet — try again in a few seconds"
            .into()),
        Err(_) => Ok(None),
    }
}

pub(super) fn check_endpoint(endpoint: &str) -> Result<(), String> {
    let url: url::Url = endpoint.parse().map_err(|e| format!("{endpoint}: {e}"))?;
    match url.scheme() {
        "http" | "https" => Ok(()),
        other => Err(format!("{endpoint}: {other} is not an http(s) endpoint")),
    }
}

pub(super) fn host_of(endpoint: &str) -> String {
    endpoint
        .parse::<url::Url>()
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_else(|| endpoint.to_string())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yubigo_upload_{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn storage_yaml_is_parsed_by_airoa_core() {
        let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("airoa-sdk/build");
        airoa_sdk::start(&payload)
            .expect("the payload and its cwasm must both be built — see the error");
        let base = temp("storage_yaml");
        std::fs::write(
            base.join(STORAGE_FILE),
            "targets:\n  rig:\n    endpoint: minio:9000\n    access_key: a\n    secret_key: s\n    prefix: /yubi-go/\n    path_rule: canonical\n    gc:\n      strategy: [marker]\n      check_interval_sec: 60\ndelete_after_upload: false\n",
        )
        .unwrap();
        airoa_sdk::add_preopen(&base, "/storage_probe", airoa_sdk::Access::ReadOnly);

        let settings = storage_settings(base.to_str().unwrap(), "/storage_probe").unwrap();
        let target = settings.target.expect("a usable target");
        assert_eq!(target.endpoint, "http://minio:9000");
        assert_eq!(target.bucket, "data", "upstream's default bucket");
        assert_eq!(target.prefix, "yubi-go/", "normalized upstream");
        assert_eq!(target.path_rule, PathRule::Canonical);
        assert!(!target.delete_after_upload);
        assert_eq!(
            settings.gc.as_ref().expect("the gc block rides along")["check_interval_sec"],
            60
        );

        let bare = temp("no_storage_yaml");
        let err = storage_settings(bare.to_str().unwrap(), "/nowhere").unwrap_err();
        assert_eq!(err, "no storage.yaml", "missing file keeps uploads off, like before");
    }

    #[test]
    fn the_page_writes_a_storage_yaml_airoa_core_reads() {
        let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("airoa-sdk/build");
        airoa_sdk::start(&payload)
            .expect("the payload and its cwasm must both be built — see the error");
        let base = temp("page_storage");
        airoa_sdk::add_preopen(&base, "/page_storage", airoa_sdk::Access::ReadOnly);
        let dir = base.to_str().unwrap();
        let save = |body: &str| save_at(dir, "/page_storage", body.as_bytes());

        let form = r#"{"endpoint": "http://rig:9000/", "bucket": "data", "prefix": "a \"b\": c",
                       "access_key": "AK", "secret_key": "S#K", "delete_after_upload": true}"#;
        assert_eq!(save(&form.replace("AK", "")).unwrap_err(), "access key is empty");
        save(form).unwrap();
        let t = storage_settings(dir, "/page_storage").unwrap().target.unwrap();
        assert_eq!((t.endpoint.as_str(), t.secret_key.as_str()), ("http://rig:9000", "S#K"));
        assert_eq!(t.prefix, "a \"b\": c/");
        assert_eq!(t.path_rule, PathRule::Canonical);
        assert!(t.delete_after_upload);

        // Blank keys keep the saved ones while the endpoint stays on its host.
        save(r#"{"endpoint": "http://rig:9000", "bucket": "other"}"#).unwrap();
        let t = storage_settings(dir, "/page_storage").unwrap().target.unwrap();
        assert_eq!((t.bucket.as_str(), t.access_key.as_str()), ("other", "AK"));
        assert!(!t.delete_after_upload);
        let moved = save(r#"{"endpoint": "http://evil:9000", "bucket": "data"}"#).unwrap_err();
        assert!(moved.contains("another host"), "{moved}");
        save(
            r#"{"endpoint": "http://rig:9001", "bucket": "data", "access_key": "AK2",
                 "secret_key": "SK2"}"#,
        )
        .unwrap();
        let t = storage_settings(dir, "/page_storage").unwrap().target.unwrap();
        assert_eq!((t.endpoint.as_str(), t.access_key.as_str()), ("http://rig:9001", "AK2"));

        assert!(save(r#"{"endpoint": "rig:9000", "bucket": "data"}"#).is_err());
        save(r#"{"off": true}"#).unwrap();
        assert_eq!(storage_settings(dir, "/page_storage").unwrap_err(), NO_FILE);
    }

    /// The keys the form does not show survive a save, including a retention
    /// of 0, which upstream reads as "never sweep" and a missing key as 24 h.
    #[test]
    fn a_save_keeps_the_storage_keys_the_page_does_not_show() {
        let payload = Path::new(env!("CARGO_MANIFEST_DIR")).join("airoa-sdk/build");
        airoa_sdk::start(&payload)
            .expect("the payload and its cwasm must both be built — see the error");
        let base = temp("hidden_storage_keys");
        airoa_sdk::add_preopen(&base, "/hidden_storage_keys", airoa_sdk::Access::ReadOnly);
        let dir = base.to_str().unwrap();
        std::fs::write(
            base.join(STORAGE_FILE),
            "targets:\n  office:\n    endpoint: http://rig:9000\n    access_key: a\n    secret_key: s\n    path_rule: flat\n    gc:\n      strategy: [marker]\ndelete_after_upload: false\nlocal_retention_hours: 0\nstate_purge_age_hours: 48\n",
        )
        .unwrap();

        save_at(dir, "/hidden_storage_keys", br#"{"endpoint": "http://rig:9000", "bucket": "b"}"#)
            .unwrap();
        let s = storage_settings(dir, "/hidden_storage_keys").unwrap();
        assert_eq!(s.name, "office", "a renamed target sends every episode again");
        assert_eq!(s.retention, None, "0 must not become upstream's 24 h default");
        assert_eq!(s.state_purge_age_hours, 48.0);
        assert!(s.gc.is_some());
        assert_eq!(s.target.unwrap().path_rule, PathRule::Flat);
    }

    #[test]
    fn a_save_is_refused_while_the_saved_settings_are_unknown() {
        let e = known(Err(WAITING.into())).unwrap_err();
        assert!(e.contains("still starting"), "{e}");
        assert_eq!(known(Err(NO_FILE.into())), Ok(None), "no file has nothing to keep");
    }

    #[test]
    fn an_endpoint_without_a_scheme_is_refused_where_it_is_configured() {
        assert!(check_endpoint("http://minio:9000").is_ok());
        assert!(check_endpoint("https://s3.ap-northeast-1.wasabisys.com").is_ok());
        let e = check_endpoint("minio:9000").unwrap_err();
        assert!(e.contains("not an http(s) endpoint"), "{e}");
        assert!(check_endpoint("").is_err());
        assert!(super::super::s3::presign_put(
            &UploadConfig { endpoint: "".into(), ..minio_target() },
            "k"
        )
        .is_err());
    }

    fn minio_target() -> UploadConfig {
        UploadConfig {
            endpoint: "http://minio:9000".into(),
            bucket: "data".into(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            region: String::new(),
            prefix: String::new(),
            path_rule: PathRule::Flat,
            delete_after_upload: false,
        }
    }
}
