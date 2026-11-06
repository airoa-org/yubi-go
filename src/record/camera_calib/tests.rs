use super::*;

#[test]
fn the_default_parses_and_keeps_the_laptop_fit() {
    let d = default();
    assert_eq!(d.grippers, ["PL028", "PR028"]);
    assert_eq!(d.head[0].f, 543.77);
    assert_eq!(d.wrist.right.mount.t, [-0.00503, 0.00045, -0.00227]);
    let back: Calib = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
    assert_eq!(back.encoder.left.k, d.encoder.left.k);
}

#[test]
fn an_id_is_the_gripper_pair_in_plain_letters() {
    assert_eq!(
        store::id(&["PL028".into(), "PR028".into()]),
        "PL028-PR028",
        "saved ids keep their name"
    );
    assert!(store::safe_id("PL028-PR028") && !store::safe_id("../x") && !store::safe_id(""));
}

#[test]
fn every_pair_gets_an_id_of_its_own_that_the_routes_accept() {
    let pair = |a: &str, b: &str| store::id(&[a.into(), b.into()]);
    let ids = [
        pair("PL028", "PR028"),
        pair("a.b", "c"),
        pair("a-b", "c"),
        pair("a", "b-c"),
        pair("a-b-c", ""),
        pair("PR 028/x", "PL028"),
        pair(&"L".repeat(40), &"R".repeat(40)),
        pair(&"L".repeat(40), &format!("{}x", "R".repeat(40))),
    ];
    for id in &ids {
        assert!(store::safe_id(id), "{id}");
    }
    let distinct: std::collections::HashSet<&String> = ids.iter().collect();
    assert_eq!(distinct.len(), ids.len(), "{ids:?}");
    assert!(ids[1].starts_with("a-b-c-"), "still readable: {}", ids[1]);
}

/// Arrivals scattered ±2 ms by USB, as on 2026-10-28, come back to within 0.1 ms
/// of the camera clock's line, across its 32-bit wrap; a frame that arrived
/// 30 ms late is placed by the clock.
#[test]
fn frame_times_follow_the_camera_clock() {
    let origin = 1_000_000_000_000u64;
    let mut frames = Vec::new();
    let mut truth = Vec::new();
    for i in 0..500u64 {
        let t = origin + 5_000_000_000 + i * 10_120_000;
        let jitter = ((i * 7919) % 400) as i64 * 10_000 - 2_000_000;
        let late = if i == 250 { 30_000_000 } else { 0 };
        let pts = (4_294_000_000u64 + i * 151_800) as u32;
        let first = (t as i64 + jitter + late) as u64;
        frames.push((first + 8_000_000, pts, first));
        truth.push((t - origin) as f64);
    }
    let times = frame_times(&frames, origin);
    assert_eq!(times.len(), 500);
    let mean = truth.iter().zip(&times).map(|(t, f)| f.1 - t).sum::<f64>() / 500.0;
    for (t, f) in truth.iter().zip(&times) {
        assert!((f.1 - t - mean).abs() < 1e5, "{} vs {t}", f.1);
    }
}
