//! The saved calibrations, one per gripper pair, in
//! `camera-calibration/<left>-<right>.json`.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

pub const DIR: &str = "camera-calibration";

fn root(base: &str) -> PathBuf {
    Path::new(base).join(DIR)
}

/// A calibration's id, from its grippers: what the routes accept, and its
/// file name. Plain ids (`PL028`) give `PL028-PR028`. Any other pair gets
/// a hash of both ids after it, because replacing characters makes `a.b`
/// and `a-b` one id, and a long pair would pass [`safe_id`]'s 64 bytes.
/// That form has two dashes or more, so it never meets a plain one.
pub fn id(grippers: &[String; 2]) -> String {
    let plain =
        |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    let joined = format!("{}-{}", grippers[0], grippers[1]);
    if grippers.iter().all(|g| plain(g)) && joined.len() <= 64 {
        return joined;
    }
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' })
            .take(20)
            .collect()
    };
    // FNV-1a, because the id names a file and must not change between builds
    // the way `DefaultHasher` may.
    let hash = grippers
        .iter()
        .flat_map(|g| g.bytes().chain([0]))
        .fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3));
    format!("{}-{}-{hash:016x}", clean(&grippers[0]), clean(&grippers[1]))
}

pub fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn saved(base: &str) -> Vec<Value> {
    let mut out: Vec<Value> = std::fs::read_dir(root(base))
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| serde_json::from_slice(&std::fs::read(e.path()).ok()?).ok())
        .collect();
    out.sort_by_key(|c: &Value| std::cmp::Reverse(c["fit"]["created"].as_u64().unwrap_or(0)));
    out
}

/// Every saved calibration, newest first.
pub fn status(base: &str) -> Value {
    let saved: Vec<Value> = saved(base)
        .into_iter()
        .map(|c| json!({ "id": id_of(&c), "grippers": c["grippers"], "fit": c["fit"] }))
        .collect();
    json!({ "saved": saved })
}

fn id_of(c: &Value) -> String {
    let g = |i: usize| c["grippers"][i].as_str().unwrap_or("").to_string();
    id(&[g(0), g(1)])
}

/// The calibration for gripper pair `id`, or the newest saved one for
/// `latest`. Either falls back to the built-in default, marked `"default"`.
pub fn load(base: &str, id: &str) -> Value {
    let found = if id == "latest" {
        saved(base).into_iter().next()
    } else {
        std::fs::read(root(base).join(format!("{id}.json")))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
    };
    found.unwrap_or_else(|| {
        let mut d: Value =
            serde_json::from_str(super::DEFAULT).expect("assets/calib/default.json parses");
        d["default"] = Value::Bool(true);
        d
    })
}
