use super::{Config, CAMERA_KEYS};
use crate::hub::Roles;

/// Longer than any `bus-port.port…` chain, which USB caps at 7 hops.
const MAX_PORT_LEN: usize = 32;

/// Every top-level key of `config.json`.
pub(super) const EDITABLE: [&str; 14] = [
    "mode",
    "identity",
    "backend",
    "tasks",
    "selected_task",
    "head_camera",
    "keep_faulted_episodes",
    "camera_roles",
    "camera_ports",
    "video",
    "folder",
    "announce_steps",
    "require_operator",
    "operator_picks_task",
];

/// Keys a pre-release build wrote, and their names now. `load` still reads
/// them; the page may not write them.
pub(super) const RENAMED: [(&str, &str); 2] =
    [("front_camera", "head_camera"), ("keep_faulted_takes", "keep_faulted_episodes")];

const IDENTITY_FIELDS: [&str; 6] =
    ["organization", "site", "location", "robot_type", "robot_id", "operator"];

/// Bounds on what the page may write. Generous for anything a person types,
/// and small enough that a hostile request cannot fill the disk.
const MAX_LEN: usize = 128;
const MAX_URL: usize = 512;
const MAX_TASKS: usize = 64;
const MAX_TASK_LEN: usize = 200;
/// At the 120 s per-step limit, 32 steps is over an hour in one take.
const MAX_SUBTASKS: usize = 32;

impl Config {
    /// Apply an edit from the `:8080` page (or the headset's camera picker) to
    /// the document on disk, or say in words why it cannot be applied.
    ///
    /// **Absent means unchanged, not cleared**: the page never receives the API
    /// key, so a whole-document write would wipe it with every identity edit.
    /// **An unknown key is an error**, because a field that silently does
    /// nothing is how an afternoon goes to "why is it still standalone". The
    /// error strings are shown to whoever holds the phone, so they name the fix.
    pub fn merge(
        current: &serde_json::Value,
        patch: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let patch = patch.as_object().ok_or("expected a JSON object")?;
        let mut doc = current.as_object().cloned().unwrap_or_default();

        for key in patch.keys() {
            if let Some((_, current)) = RENAMED.iter().find(|(old, _)| old == key) {
                return Err(format!("{key:?} is now called {current:?}"));
            }
            if !EDITABLE.contains(&key.as_str()) {
                return Err(format!("{key:?} is not a configurable field"));
            }
        }

        if let Some(mode) = patch.get("mode") {
            let mode = mode.as_str().ok_or("mode must be a string")?;
            if mode != "standalone" && mode != "production" {
                return Err(format!("unknown mode {mode:?} — standalone or production"));
            }
            doc.insert("mode".into(), mode.into());
        }

        if let Some(video) = patch.get("video") {
            let video = video.as_str().ok_or("video must be a string")?;
            if video != "h264" && video != "jpeg" {
                return Err(format!("unknown video {video:?} — h264 or jpeg"));
            }
            doc.insert("video".into(), video.into());
        }

        if let Some(folder) = patch.get("folder") {
            let folder = folder.as_str().ok_or("folder must be a string")?;
            doc.insert("folder".into(), crate::hub::episodes::clean_folder(folder)?.into());
        }

        if let Some(identity) = patch.get("identity") {
            let identity = identity.as_object().ok_or("identity must be an object")?;
            let mut merged =
                doc.get("identity").and_then(|v| v.as_object()).cloned().unwrap_or_default();
            for (key, value) in identity {
                if !IDENTITY_FIELDS.contains(&key.as_str()) {
                    return Err(format!("identity.{key} is not a field"));
                }
                let value = clean(value, &format!("identity.{key}"), MAX_LEN)?;
                merged.insert(key.clone(), value.into());
            }
            doc.insert("identity".into(), merged.into());
        }

        if let Some(backend) = patch.get("backend") {
            let backend = backend.as_object().ok_or("backend must be an object")?;
            let mut merged =
                doc.get("backend").and_then(|v| v.as_object()).cloned().unwrap_or_default();
            let was =
                |key: &str| merged.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
            let (old_url, old_key) = (was("base_url"), was("api_key"));
            for (key, value) in backend {
                if !["base_url", "api_key"].contains(&key.as_str()) {
                    return Err(format!("backend.{key} is not a field"));
                }
                merged
                    .insert(key.clone(), clean(value, &format!("backend.{key}"), MAX_URL)?.into());
            }
            let base_url = merged.get("base_url").and_then(|v| v.as_str()).unwrap_or_default();
            if base_url.is_empty() {
                // Clearing the address clears the key with it: a key for a
                // backend nobody can name is a secret with no owner.
                if backend
                    .get("api_key")
                    .is_some_and(|v| !v.as_str().unwrap_or_default().is_empty())
                {
                    return Err("backend.api_key needs backend.base_url — with no address there \
                                is no backend to attach it to"
                        .into());
                }
                doc.remove("backend");
            } else {
                if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
                    return Err(format!(
                        "backend URL needs an http:// or https:// scheme: {base_url}"
                    ));
                }
                // The key is sent wherever base_url points, so anyone who can
                // reach the page could send it to a host of theirs.
                let retyped = backend
                    .get("api_key")
                    .is_some_and(|v| !v.as_str().unwrap_or_default().trim().is_empty());
                if !old_key.is_empty() && !retyped && origin(&old_url) != origin(base_url) {
                    return Err("the backend URL is on another host now — enter the API key \
                                again to use it there"
                        .into());
                }
                doc.insert("backend".into(), merged.into());
            }
        }

        if let Some(tasks) = patch.get("tasks") {
            let tasks = tasks.as_array().ok_or("tasks must be an array")?;
            if tasks.len() > MAX_TASKS {
                return Err(format!("more than {MAX_TASKS} tasks"));
            }
            // Replaced whole, because a list has no key to merge on. Blank rows
            // are an editor's empty lines, so they drop out.
            let mut declared = Vec::new();
            let mut names = std::collections::HashSet::new();
            for task in tasks {
                let (name, subtasks, still) = match task {
                    serde_json::Value::Object(task) => (
                        task.get("name").unwrap_or(&serde_json::Value::Null),
                        match task.get("subtasks") {
                            None => &[][..],
                            Some(s) => s.as_array().ok_or("subtasks must be an array")?,
                        },
                        match task.get("advance_when_still") {
                            None => false,
                            Some(v) => {
                                v.as_bool().ok_or("advance_when_still must be true or false")?
                            }
                        },
                    ),
                    name => (name, &[][..], false),
                };
                let full = text(name, "task")?;
                let mut steps = Vec::new();
                for step in subtasks {
                    let step = text(step, "subtask")?;
                    if !step.is_null() {
                        steps.push(step);
                    }
                }
                if full.is_null() {
                    if let Some(step) = steps.first() {
                        return Err(format!("subtask {step} belongs to a task with no name"));
                    }
                    continue;
                }
                let name = full.as_str().unwrap_or("").to_string();
                if steps.len() > MAX_SUBTASKS {
                    return Err(format!("{name:?} has more than {MAX_SUBTASKS} subtasks"));
                }
                // `selected_task` picks by name, so two of one name is one too many.
                if !names.insert(name.clone()) {
                    return Err(format!("{name:?} is declared twice"));
                }
                // A task without steps stays a bare string, as pre-release builds read it.
                declared.push(match (steps.is_empty(), still) {
                    (true, false) => full,
                    (_, false) => serde_json::json!({"name": full, "subtasks": steps}),
                    _ => serde_json::json!(
                        {"name": full, "subtasks": steps, "advance_when_still": true}
                    ),
                });
            }
            doc.insert("tasks".into(), declared.into());
        }

        for key in [
            "head_camera",
            "keep_faulted_episodes",
            "announce_steps",
            "require_operator",
            "operator_picks_task",
        ] {
            if let Some(value) = patch.get(key) {
                let value =
                    value.as_bool().ok_or_else(|| format!("{key} must be true or false"))?;
                doc.insert(key.into(), value.into());
            }
        }

        // Refused rather than repaired: two roles reading one camera records
        // that module twice under two names, with every count looking right.
        if let Some(roles) = patch.get("camera_roles") {
            let array = roles.as_array().ok_or("camera_roles must be an array")?;
            let ranks: Vec<usize> =
                array.iter().filter_map(|r| r.as_u64()).map(|r| r as usize).collect();
            if ranks.len() != array.len() || Roles::from_ranks(&ranks).is_none() {
                return Err(format!(
                    "camera_roles must be a permutation of {:?}",
                    crate::hub::USB_CAM_SLOTS
                ));
            }
            doc.insert("camera_roles".into(), array.clone().into());
            // The newer choice wins, and a stale port list would outrank it.
            doc.remove("camera_ports");
        }

        // Replaced whole, and the rank list goes with it: a port names the
        // camera itself, so it stays right after a replug where a rank does not.
        if let Some(ports) = patch.get("camera_ports") {
            let ports = ports.as_object().ok_or("camera_ports must be an object")?;
            let mut saved = serde_json::Map::new();
            for (key, port) in ports {
                if !CAMERA_KEYS.contains(&key.as_str()) {
                    return Err(format!("camera_ports.{key} is not a camera — {CAMERA_KEYS:?}"));
                }
                let port = clean(port, &format!("camera_ports.{key}"), MAX_PORT_LEN)?;
                if port.is_empty() {
                    continue;
                }
                if saved.values().any(|p| p.as_str() == Some(port.as_str())) {
                    return Err(format!("camera_ports gives port {port} to two cameras"));
                }
                saved.insert(key.clone(), port.into());
            }
            doc.insert("camera_ports".into(), saved.into());
            doc.remove("camera_roles");
        }

        if let Some(selected) = patch.get("selected_task") {
            let selected = clean(selected, "selected_task", MAX_TASK_LEN)?;
            match selected.is_empty() {
                // Clearing it means "back to the first task on the list".
                true => doc.remove("selected_task"),
                false => doc.insert("selected_task".into(), selected.into()),
            };
        }

        // Checked against the result, so switching to production with nowhere
        // to talk to and clearing the address of a production rig both fail.
        if doc.get("mode").and_then(|v| v.as_str()) == Some("production")
            && !doc.contains_key("backend")
        {
            return Err("production mode needs a backend URL — there is nothing to talk to".into());
        }

        // A selection must name a task on the list. Picking one nobody declared
        // is a typo and is refused; re-declaring the list out from under a
        // standing selection drops it, so a task re-added later cannot revive it.
        // A production rig picks from the dashboard's queue instead, which
        // this document does not hold.
        let selected = doc.get("selected_task").and_then(|v| v.as_str()).map(str::to_string);
        let backend = |key: &str| {
            doc.get("backend").and_then(|b| b.get(key)).and_then(|v| v.as_str()).unwrap_or("")
        };
        let standalone = doc.get("mode").and_then(|v| v.as_str()) == Some("standalone")
            || backend("base_url").is_empty()
            || backend("api_key").is_empty();
        if let Some(selected) = selected.filter(|_| standalone) {
            let known = doc.get("tasks").and_then(|v| v.as_array()).is_some_and(|t| {
                t.iter().any(|d| {
                    let name = d.get("name").unwrap_or(d);
                    name.as_str().or_else(|| name.get("en").and_then(|n| n.as_str()))
                        == Some(&*selected)
                })
            });
            if !known && patch.contains_key("selected_task") {
                return Err(format!("{selected:?} is not one of the declared tasks"));
            }
            if !known {
                doc.remove("selected_task");
            }
        }

        Ok(doc.into())
    }
}

/// A name from the page, cleaned, or null when blank.
fn text(value: &serde_json::Value, field: &str) -> Result<serde_json::Value, String> {
    let name = clean(value, field, MAX_TASK_LEN)?;
    Ok(match name.is_empty() {
        true => serde_json::Value::Null,
        false => name.into(),
    })
}

/// A string field from the page, trimmed. Control characters are refused
/// rather than stripped: the panel cannot draw them, and
/// silently altering what someone submitted is worse than declining it.
fn clean(value: &serde_json::Value, field: &str, max: usize) -> Result<String, String> {
    let value = value.as_str().ok_or_else(|| format!("{field} must be a string"))?.trim();
    if value.chars().count() > max {
        return Err(format!("{field} is longer than {max} characters"));
    }
    if value.chars().any(char::is_control) {
        return Err(format!("{field} contains a control character"));
    }
    Ok(value.to_string())
}

/// Scheme, host and port: what decides who receives a request.
pub(crate) fn origin(url: &str) -> Option<String> {
    url::Url::parse(url).ok().map(|u| u.origin().ascii_serialization())
}
