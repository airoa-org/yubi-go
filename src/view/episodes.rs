//! What the panel's episode list reads off disk: the take's clock time, its
//! label and length from `meta.json`, its size, and which of the three stages
//! its marker files put it in. Nothing here draws.

use std::path::{Path, PathBuf};

use super::panel;

pub(super) fn dir_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.filter_map(|e| e.ok()).filter_map(|e| e.metadata().ok()).map(|m| m.len()).sum()
        })
        .unwrap_or(0)
}

/// Seconds east of UTC on the device's clock at `secs`. Bionic reads the zone
/// from `persist.sys.timezone`, the one the headset's settings set.
fn utc_offset_sec(secs: i64) -> i64 {
    let t = secs as libc::time_t;
    // SAFETY: `tm` is plain old data, and both pointers are to live locals.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    match unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        true => 0,
        false => tm.tm_gmtoff as i64,
    }
}

pub(super) fn hhmm(name: &str) -> String {
    let Some(ms) = name.strip_prefix("episode_").and_then(|t| t.parse::<i64>().ok()) else {
        return String::new();
    };
    let secs = ms / 1000;
    let day = (secs + utc_offset_sec(secs)).rem_euclid(86_400);
    format!("{:02}:{:02}", day / 3600, (day % 3600) / 60)
}

pub(super) fn meta_of(dir: &Path) -> (String, Option<u64>) {
    let Some(meta) = std::fs::read_to_string(dir.join(crate::hub::episodes::META))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    else {
        return (String::new(), None);
    };
    let task = meta["episode"]["label"].as_str().unwrap_or_default().to_string();
    let secs = match (meta["episode"]["start_time"].as_f64(), meta["episode"]["end_time"].as_f64())
    {
        (Some(t0), Some(t1)) if t1 > t0 => Some((t1 - t0).round() as u64),
        _ => None,
    };
    (task, secs)
}

pub(super) fn episodes(base: &str, names: &[String], max: usize) -> Vec<panel::Episode> {
    let takes = crate::hub::episodes::takes(base);
    // The directory is read once, here, so the task, duration and size
    // columns all describe the same take.
    let mut named: Vec<(String, PathBuf, panel::Stage)> = names
        .iter()
        .cloned()
        .map(|name| {
            let dir = Path::new(&takes).join(&name);
            let stage = if crate::hub::upload::was_sent(&name) {
                panel::Stage::Uploaded
            } else if crate::hub::episodes::is_complete(&dir) {
                panel::Stage::Waiting
            } else {
                panel::Stage::Recording
            };
            (name, dir, stage)
        })
        .collect();
    named.sort_by_key(|(_, _, stage)| !matches!(stage, panel::Stage::Waiting));
    named
        .into_iter()
        .take(max)
        .map(|(name, dir, stage)| {
            let (task, secs) = meta_of(&dir);
            panel::Episode { at: hhmm(&name), task, secs, bytes: dir_bytes(&dir), name, stage }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_takes_come_first_and_each_stage_stays_newest_first() {
        let base =
            std::env::temp_dir().join(format!("yubigo_panel_episodes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let base = base.to_str().unwrap().to_string();
        let takes = crate::hub::episodes::takes(&base);
        for (name, complete) in
            [("episode_1000", true), ("episode_2000", false), ("episode_3000", true)]
        {
            let dir = Path::new(&takes).join(name);
            std::fs::create_dir_all(&dir).unwrap();
            if complete {
                std::fs::write(dir.join(crate::hub::episodes::META), "{}").unwrap();
            }
        }
        let names = crate::hub::episodes::list(&base);
        let shown: Vec<String> = episodes(&base, &names, 3).into_iter().map(|e| e.name).collect();
        assert_eq!(shown, ["episode_3000", "episode_1000", "episode_2000"]);
    }

    /// Prints rather than asserts the zone, so it runs on any host. On a
    /// headset set to Asia/Tokyo it must print 09:00.
    #[test]
    fn a_take_s_time_is_the_device_s_local_time() {
        let offset = utc_offset_sec(0);
        let expected = offset.rem_euclid(86_400);
        let shown = hhmm("episode_0");
        println!("episode_0 at UTC offset {offset} s shows {shown}");
        assert_eq!(shown, format!("{:02}:{:02}", expected / 3600, (expected % 3600) / 60));
        assert_eq!(hhmm("not_a_take"), "");
    }
}
