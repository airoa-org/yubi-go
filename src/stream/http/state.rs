//! `/state`: everything the page renders, in one poll.

use crate::hub::{Hub, EPISODE_DIR};

/// Seconds recorded across the finished takes, from each `meta.json`. Kept per
/// take because `/state` is polled several times a second and a closed take's
/// `meta.json` never changes. A take still recording has none yet, so it is
/// counted on the first poll after it closes.
fn recorded_sec(names: &[String]) -> f64 {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static SEEN: Mutex<Option<HashMap<String, f64>>> = Mutex::new(None);
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    let seen = seen.get_or_insert_with(HashMap::new);
    seen.retain(|name, _| names.contains(name));
    let root = crate::hub::episodes::takes(EPISODE_DIR);
    for name in names {
        if seen.contains_key(name) {
            continue;
        }
        let path = std::path::Path::new(&root).join(name).join(crate::hub::episodes::META);
        let Some(meta) = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        else {
            continue;
        };
        let episode = &meta["episode"];
        if let (Some(start), Some(end)) =
            (episode["start_time"].as_f64(), episode["end_time"].as_f64())
        {
            seen.insert(name.clone(), (end - start).max(0.0));
        }
    }
    seen.values().sum()
}

/// Everything the page needs to render, in one poll.
///
/// **No credential is ever in here**, and the endpoint carries nothing the
/// panel does not already show to whoever is standing in front of the headset.
pub(super) fn state(hub: &Hub) -> String {
    let view = hub.task_view();
    let config = &view.config;
    let stats = hub.stats();
    // `ready()`, not `refusal()`. The two differ on purpose: `refusal` is empty
    // until a start has actually been refused, so the panel does not sit under
    // an error the operator was never told about. Opening this page *is* asking
    // "is the rig all right?", so the standing checklist belongs here.
    let reasons = hub.ready().err().unwrap_or_default();
    let calibration = hub.calibration();
    let (prompt, held, message) = hub.calibration_ui();
    let disk = crate::misc::disk::usage(crate::hub::EPISODE_DIR);
    let names = crate::hub::episodes::list(crate::hub::EPISODE_DIR);
    let upload = crate::hub::upload::state();
    let (upload_text, upload_level) = upload.verdict();
    // Production unless `config.json` asks for standalone or lacks a backend
    // URL or API key (`Config::standalone`).
    let production = config.production();
    // Elapsed rather than a clock time, for the reason `panel::ui::ago` is: the
    // device has no timezone, and how long it has been going on is the half
    // that answers "did I lose this take or this session".
    let offline_min = crate::hub::backend::offline_since_ns()
        .map(|ns| crate::misc::clock::realtime_ns().saturating_sub(ns) / 60_000_000_000);

    let mut state = serde_json::json!({
        "mode": if production { "production" } else { "standalone" },
        // What is still on the device with nowhere to be. A page open on a
        // headset that has stopped uploading is the surface where somebody
        // finds out, since the operator is not looking at anything.
        "queued": crate::hub::upload::depth(),
        "identity": {
            "organization": config.identity.organization,
            "site": config.identity.site,
            "location": config.identity.location,
            "robot_type": config.identity.robot_type,
            "robot_id": config.identity.robot_id,
            "operator": config.identity.operator,
        },
        "task": config.has_task().then(|| config.task()),
        // The instruction the hands are on, when the assignment has subtasks —
        // the band's second row, in the band's own words.
        "step": view.step_line(),
        "task_completed": config.task_completed(),
        "recording": hub.recording(),
        "elapsed_sec": hub.task_display().episode_elapsed_sec,
        "write_failed": hub.recording_failed(),
        "episode_count": names.len(),
        // The page shows the same words the panel does. Two surfaces
        // disagreeing about why a rig will not start is worse than either
        // alone, so both render `Reason::text` rather than paraphrasing it.
        "ready": reasons.is_empty(),
        "reasons": reasons.iter().map(|r| r.text()).collect::<Vec<_>>(),
        // The same reasons as the panel shows them.
        "reasons_shown": reasons.iter().map(|r| r.shown()).collect::<Vec<_>>(),
        // Never a stand-in percentage on a device that will not answer, for the
        // reason `sensors::battery::read` errors rather than guessing.
        "battery": crate::misc::power::battery().map(|b| {
            let (to, secs) = match b.eta {
                Some(crate::misc::power::Eta::Full(d)) => ("full", Some(d.as_secs())),
                Some(crate::misc::power::Eta::Empty(d)) => ("empty", Some(d.as_secs())),
                None => ("unknown", None),
            };
            serde_json::json!({"pct": b.pct, "charging": b.charging, "eta_to": to, "eta_sec": secs})
        }),
        "disk": disk.map(|u| serde_json::json!({
            "free": u.free,
            "used": u.used,
            // The number the panel actually acts on: bytes free says nothing
            // until it is minutes of recording. `null` means the floor is
            // reached and the gate is already refusing starts.
            "recording_sec": crate::misc::power::recording_left(u.free, hub.video()).map(|d| d.as_secs()),
        })),
        // One sentence, from `upload` so the band and the page cannot
        // paraphrase each other.
        "upload": {"text": upload_text, "level": match (&upload, upload_level) {
            // Not a fault, but not good news either: grey, as on the panel
            // (`upload_colour`).
            (crate::hub::upload::State::NoTarget(why), _) if why == crate::hub::upload::NO_FILE => {
                "idle"
            }
            (_, crate::hub::upload::Level::Ok) => "ok",
            (_, crate::hub::upload::Level::Warn) => "warn",
            (_, crate::hub::upload::Level::Bad) => "bad",
        }},
        "backend_offline_min": offline_min,
        "unbound": crate::hub::backend::unbound(),
        // Off by default, and worth saying only when it is on: a session
        // collecting its faulted takes deliberately is a session whose
        // meta.json means something different.
        "keep_faulted_episodes": config.keep_faulted_episodes,
        // What the Setup tab edits.
        "tasks": config.tasks,
        // Every name the rig may say, by text: whether its audio is made yet,
        // and the id `/speech/<id>.wav` serves it by.
        "speech": crate::misc::speech::states().into_iter().map(|(text, state)| {
            let id = crate::misc::speech::id(&text);
            let state = match state {
                crate::misc::speech::State::Pending => serde_json::json!({"state": "pending", "id": id}),
                crate::misc::speech::State::Ready => serde_json::json!({"state": "ready", "id": id}),
                crate::misc::speech::State::Failed(e) => serde_json::json!({"state": "failed", "id": id, "error": e}),
            };
            (text, state)
        }).collect::<serde_json::Map<_, _>>(),
        "head_camera": config.head_camera,
        "video": config.video.name(),
        // The API key never leaves the device; the form only learns whether
        // one is set, to say so in its placeholder.
        "backend": (!config.backend.base_url.is_empty()).then(|| serde_json::json!({
            "base_url": config.backend.base_url,
            "api_key_set": !config.backend.api_key.is_empty(),
        })),
        // The camera each slot reads (`camera_roles`), and how many USB
        // cameras have negotiated a format, so the picker is chosen against
        // what is plugged in.
        "cameras": {
            "ranks": hub.roles().ranks(),
            "found": crate::hub::SRC_CAM.iter().filter(|&&i| stats[i].width > 0).count(),
            "matched": hub.cameras_matched(),
        },
        // The sweep, so the prompt an operator has to read is on the surface
        // they can see with the headset round their neck.
        "calibration": {
            "prompt": prompt.map(|p| p.text()),
            "held": held,
            "message": message.iter().map(|m| m.text()).collect::<Vec<_>>(),
            "sides": (0..2).map(|side| calibration.side(side).map(|s| serde_json::json!({
                "device_id": s.device_id,
                // Degrees, because the envelope is the one number here a person
                // checks against a gripper they can see.
                "envelope_deg": (s.envelope().to_degrees() * 10.0).round() / 10.0,
                "captured_realtime_ns": s.captured_realtime_ns,
            }))).collect::<Vec<_>>(),
        },
        "sources": crate::hub::SOURCE_NAMES.iter().zip(stats.iter()).enumerate().map(|(i, (name, s))| {
            serde_json::json!({
                "name": name,
                "hz": (s.hz * 10.0).round() / 10.0,
                "nominal": s.nominal,
                "live": s.live,
                "starved": s.starved(),
                "extrapolated": s.extrapolated,
                "bytes_mb_s": (s.bytes_mb_s * 100.0).round() / 100.0,
                "width": s.width,
                "height": s.height,
                // Whether `/stream` can actually serve this slot. A live rate is
                // not enough: a wrist that frames but never delivers keeps
                // ticking, so a page that trusted `live` drew a black rectangle
                // and called it video.
                "preview": crate::hub::SRC_CAM.iter().position(|&c| c == i)
                    .is_some_and(|slot| hub.has_preview(slot)),
            })
        }).collect::<Vec<_>>(),
    });
    // Outside the literal above, which is at `json!`'s recursion limit.
    state["recorded_sec"] = recorded_sec(&names).into();
    state["folder"] = config.folder.clone().into();
    state["announce_steps"] = config.announce_steps.into();
    state["require_operator"] = config.require_operator.into();
    state["operator_picks_task"] = config.operator_picks_task.into();
    state["queue"] = config
        .queue
        .iter()
        .map(|(task, left)| serde_json::json!({"task": task, "left": left}))
        .collect::<Vec<_>>()
        .into();
    state["storage"] = crate::hub::upload::shown(EPISODE_DIR).into();
    state["mdns_host"] = crate::stream::mdns::shown().into();
    state.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::http::assets::PAGE;

    /// **The page and `/state` are one document apart and nothing else joins
    /// them.** The page is a string constant in this file, so the join can be
    /// checked here: every `s.<field>` it reads off the state object has to be
    /// a field the endpoint actually serves.
    ///
    /// It exists because this is the drift that costs a rig visit. A renamed
    /// key compiles, serves, polls and renders — as `undefined`, which the page
    /// draws as an empty row or `NaN`, on the surface an operator is using
    /// *because* they cannot see the headset's own panel.
    ///
    /// One-way on purpose: a field served and not yet read is a field the next
    /// change to the page will use, and failing on that would make adding one a
    /// two-commit job. The other direction is the one that breaks.
    #[test]
    fn the_page_reads_no_field_state_does_not_serve() {
        let json = state(&Hub::default());
        let served: serde_json::Value = serde_json::from_str(&json).expect("state is JSON");
        let served = served.as_object().expect("state is an object");

        // Every `s.name` in the script, which is what each renderer binds the
        // polled state to. `row`'s span is deliberately not called `s`.
        let script = PAGE.split_once("<script>").expect("the page has a script").1;
        let mut read: Vec<&str> = Vec::new();
        for (at, _) in script.match_indices("s.") {
            let before = script[..at].chars().next_back().unwrap_or(' ');
            // `s.` only where `s` is the whole identifier — not the tail of
            // `this.` or `cams.`.
            if before.is_alphanumeric() || before == '_' || before == '.' {
                continue;
            }
            let field: &str = script[at + 2..]
                .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next()
                .unwrap_or("");
            if !field.is_empty() {
                read.push(field);
            }
        }
        assert!(read.len() > 20, "found only {} reads — the scan is broken", read.len());
        for field in read {
            assert!(
                served.contains_key(field),
                "page.html reads s.{field}, which /state does not serve"
            );
        }
    }
}
