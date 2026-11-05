//! Spoken readiness. The clips are generated from `assets/voice/phrases.tsv`
//! by `tools/voice/generate.py`, one per gate reason, and embedded here.

use crate::hub::{Fault, Reason, USB_HUB};
use crate::hub::{Feedback, GRIPPER_PAIR, SOURCE_NAMES};
use crate::misc::power::{Battery, Low, Supply};

/// Voiced speech in the clips is 0.17 RMS (median of 32) against 0.053 for
/// the tones, so this puts speech ~2 dB above them. Untested by ear on the
/// headset.
const GAIN: f32 = 0.4;

type Clips = &'static [(&'static str, &'static [u8])];

macro_rules! clips {
    ($($id:literal)*) => {
        const EN: Clips =
            &[$(($id, include_bytes!(concat!("../../assets/voice/en/", $id, ".wav")))),*];
    };
}

clips! {
    "ready" "task_is" "not_ready" "recording"
    "recording.cancelled" "recording.stopped" "marked_failed" "marked_failed.ended"
    "subtask.warning"
    "hub.silent"
    "left_cam.silent" "left_cam.stalled" "left_cam.starved"
    "right_cam.silent" "right_cam.stalled" "right_cam.starved"
    "head_cam.silent" "head_cam.stalled" "head_cam.starved"
    "left_gripper_sensor.silent" "left_gripper_sensor.starved"
    "right_gripper_sensor.silent" "right_gripper_sensor.starved"
    "grippers.uncalibrated"
    "left_controller.starved" "left_controller.untracked" "left_controller.lost"
    "right_controller.starved" "right_controller.untracked" "right_controller.lost"
    "headset.starved" "headset.untracked" "headset.lost"
    "task.none" "storage.low" "battery.low"
    "gate.settling" "gate.core_down" "subtask.overrun"
    "calib.squeeze" "calib.release" "calib.done" "calib.cancelled"
    "calib.left_barely" "calib.right_barely" "calib.left_wrapped" "calib.right_wrapped"
    "cams.done" "cameras.unmatched"
    "battery.unknown"
    "battery.pct.0" "battery.pct.5" "battery.pct.10" "battery.pct.15" "battery.pct.20"
    "battery.pct.25" "battery.pct.30" "battery.pct.35" "battery.pct.40" "battery.pct.45"
    "battery.pct.50" "battery.pct.55" "battery.pct.60" "battery.pct.65" "battery.pct.70"
    "battery.pct.75" "battery.pct.80" "battery.pct.85" "battery.pct.90" "battery.pct.95"
    "battery.pct.100"
    "battery.charging" "battery.plugged_not_charging" "battery.not_charging"
    "battery.left.under"
    "battery.left.15" "battery.left.30" "battery.left.45" "battery.left.60"
    "battery.left.75" "battery.left.90" "battery.left.105" "battery.left.120"
    "battery.left.135" "battery.left.150" "battery.left.165" "battery.left.180"
    "battery.left.over" "battery.left.unknown"
    "power.connected" "power.disconnected" "power.unstable"
    "battery.low_15" "battery.low_5" "storage.low_15" "storage.low_5"
    "digit.0" "digit.1" "digit.2" "digit.3" "digit.4"
    "digit.5" "digit.6" "digit.7" "digit.8" "digit.9"
    "dot" "colon" "address.none"
    "left_cam.moving" "right_cam.moving"
}

pub const READY: &str = "ready";
/// Said between "Ready to record." and the task's made name.
pub const TASK_IS: &str = "task_is";

/// "Not ready." and the reason that comes first in the gate's order. With no
/// reason, or one with no clip (core's own refusals), only "Not ready.".
pub fn not_ready(first: Option<&Reason>) -> Vec<&'static str> {
    std::iter::once("not_ready").chain(first.and_then(id)).collect()
}

/// A settings press during a take: the gate's first fault, or that all is well.
pub fn during_take(first: Option<&Reason>) -> Vec<&'static str> {
    match first {
        None => vec!["recording"],
        Some(reason) => id(reason).into_iter().collect(),
    }
}

/// What a cue says in place of its tone, given the reason the hub acted on.
/// `None` keeps the tone.
pub fn spoken(what: Feedback, reason: Option<Reason>) -> Option<Vec<&'static str>> {
    let reason = reason.as_ref().and_then(id);
    Some(match what {
        Feedback::Fault => reason.into_iter().collect(),
        Feedback::Cancelled => std::iter::once("recording.cancelled").chain(reason).collect(),
        Feedback::EndedFailed if reason == Some("subtask.overrun") => {
            vec!["recording.stopped", "subtask.overrun"]
        }
        Feedback::EndedFailed => vec!["marked_failed.ended"],
        Feedback::MarkedFailed => vec!["marked_failed"],
        Feedback::TimeWarning => vec!["subtask.warning"],
        _ => return None,
    })
}

/// A B press outside a take: the charge in 5% steps, whether it charges, and
/// the time left in 15 min steps. Time to full is not said.
pub fn battery(battery: Option<Battery>) -> Vec<&'static str> {
    let Some(b) = battery else { return vec!["battery.unknown"] };
    let pct = (b.pct.clamp(0, 100) + 2) / 5 * 5;
    let mut ids: Vec<_> = known(&format!("battery.pct.{pct}")).into_iter().collect();
    ids.push(match (b.plugged, b.charging) {
        (_, true) => "battery.charging",
        (true, false) => "battery.plugged_not_charging",
        (false, false) => "battery.not_charging",
    });
    let left = match (b.left(), b.charging) {
        (Some(left), _) => match left.as_secs_f64() / 60.0 {
            m if m < 15.0 => known("battery.left.under"),
            m if m > 180.0 => known("battery.left.over"),
            m => known(&format!("battery.left.{}", (m / 15.0).round() as u32 * 15)),
        },
        (None, false) => known("battery.left.unknown"),
        (None, true) => None,
    };
    ids.extend(left);
    ids
}

/// Said after the running-out tone.
pub fn low(low: Low) -> Option<&'static str> {
    let what = if low.disk { "storage" } else { "battery" };
    known(&format!("{what}.low_{}", low.left.as_secs() / 60))
}

/// The wrist slot whose picture moved in the wave after a match.
#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
pub fn moving(slot: usize) -> &'static str {
    ["left_cam.moving", "right_cam.moving"][slot]
}

/// The monitor address, one character at a time, e.g. `192.168.1.5:8080`.
pub fn address(address: Option<&str>) -> Vec<&'static str> {
    let Some(address) = address else { return vec!["address.none"] };
    address
        .chars()
        .filter_map(|c| match c {
            '.' => known("dot"),
            ':' => known("colon"),
            c => known(&format!("digit.{c}")),
        })
        .collect()
}

#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
pub fn supply(supply: Supply) -> &'static str {
    match supply {
        Supply::Connected => "power.connected",
        Supply::Disconnected => "power.disconnected",
        Supply::Unstable => "power.unstable",
    }
}

fn id(reason: &Reason) -> Option<&'static str> {
    const PREFIXES: [&str; 8] = [
        "left_cam",
        "right_cam",
        "head_cam",
        "left_gripper_sensor",
        "right_gripper_sensor",
        "left_controller",
        "right_controller",
        "headset",
    ];
    let prefix = match reason.source {
        USB_HUB => "hub",
        GRIPPER_PAIR => "grippers",
        source => match SOURCE_NAMES.iter().position(|&s| s == source) {
            Some(i) => PREFIXES[i],
            None => source,
        },
    };
    let suffix = match reason.fault {
        Fault::Silent => "silent",
        Fault::NotFraming => "stalled",
        Fault::Starved => "starved",
        Fault::Uncalibrated => "uncalibrated",
        Fault::NoTask => "none",
        Fault::LowSpace | Fault::LowCharge => "low",
        Fault::Untracked => "untracked",
        Fault::Lost => "lost",
        Fault::Overrun => "overrun",
        Fault::Settling => "settling",
        Fault::CoreDown => "core_down",
        Fault::Unmatched => "unmatched",
    };
    known(&format!("{prefix}.{suffix}"))
}

fn known(wanted: &str) -> Option<&'static str> {
    EN.iter().map(|&(id, _)| id).find(|&id| id == wanted)
}

/// The clips for `ids`, back to back, at `sound::RATE`.
pub fn pcm(ids: &[&str]) -> Vec<i16> {
    let mut out = Vec::new();
    for &id in ids {
        let Some(&(_, wav)) = EN.iter().find(|(known, _)| *known == id) else { continue };
        out.extend(upsampled(&samples(wav)));
    }
    out
}

/// A clip's 24 kHz samples at the stream's 48 kHz and the voice's level.
pub fn upsampled(samples: &[i16]) -> Vec<i16> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for (k, &s) in samples.iter().enumerate() {
        let next = samples.get(k + 1).copied().unwrap_or(0);
        let scale = |v: f32| (v * GAIN) as i16;
        out.push(scale(s as f32));
        out.push(scale((s as f32 + next as f32) / 2.0));
    }
    out
}

/// The 16-bit samples of a mono WAV, found by walking its chunks.
pub fn samples(wav: &[u8]) -> Vec<i16> {
    let mut at = 12;
    while at + 8 <= wav.len() {
        let size = u32::from_le_bytes(wav[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &wav[at + 8..(at + 8 + size).min(wav.len())];
        if &wav[at..at + 4] == b"data" {
            return body.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect();
        }
        at += 8 + size + size % 2;
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::sound::RATE;

    /// Kokoro's output rate; `pcm` doubles it to the player's.
    const CLIP_RATE: u32 = 24_000;

    #[test]
    fn the_clips_are_the_phrase_table() {
        let mut rows = include_str!("../../assets/voice/phrases.tsv").lines();
        let header: Vec<&str> = rows.next().unwrap().split('\t').collect();
        let rows: Vec<Vec<&str>> = rows.map(|row| row.split('\t').collect()).collect();
        assert_eq!(header, ["id", "en"]);
        assert!(rows.iter().all(|row| row.get(1).is_some_and(|t| !t.is_empty())));
        let written: Vec<&str> = rows.iter().map(|row| row[0]).collect();
        let embedded: Vec<&str> = EN.iter().map(|&(id, _)| id).collect();
        assert_eq!(written, embedded);
    }

    #[test]
    fn every_clip_is_mono_16_bit_at_the_clip_rate() {
        assert_eq!(RATE, 2 * CLIP_RATE, "the player upsamples by exactly two");
        for &(id, wav) in EN {
            let format = &wav[20..36];
            let channels = u16::from_le_bytes([format[2], format[3]]);
            let rate = u32::from_le_bytes(format[4..8].try_into().unwrap());
            let bits = u16::from_le_bytes([format[14], format[15]]);
            assert_eq!((channels, rate, bits), (1, CLIP_RATE, 16), "{id}");
            // A spoken digit is one word, and Kokoro says one in ~0.4 s.
            let floor = if address(Some("0123456789.:")).contains(&id) { 0.3 } else { 0.5 };
            let floor = (floor * CLIP_RATE as f32) as usize;
            assert!(samples(wav).len() > floor, "{id} is under {floor} samples");
        }
    }

    #[test]
    fn every_reason_the_gate_gives_has_a_clip() {
        let mut reasons = Vec::new();
        for (i, &source) in SOURCE_NAMES.iter().enumerate() {
            let faults: &[Fault] = match i {
                0..=2 => &[Fault::Silent, Fault::NotFraming, Fault::Starved],
                3..=4 => &[Fault::Silent, Fault::Starved],
                _ => &[Fault::Starved, Fault::Untracked, Fault::Lost],
            };
            reasons.extend(faults.iter().map(|&fault| Reason { source, fault }));
        }
        for (source, fault) in [
            (USB_HUB, Fault::Silent),
            (GRIPPER_PAIR, Fault::Uncalibrated),
            ("task", Fault::NoTask),
            ("storage", Fault::LowSpace),
            ("battery", Fault::LowCharge),
            ("gate", Fault::Settling),
            ("gate", Fault::CoreDown),
            ("subtask", Fault::Overrun),
            (crate::hub::CAMERAS, Fault::Unmatched),
        ] {
            reasons.push(Reason { source, fault });
        }
        let mut spoken: Vec<_> = reasons.iter().map(|r| id(r).unwrap_or("none")).collect();
        assert!(!spoken.contains(&"none"), "{reasons:?}");
        spoken.extend([READY, TASK_IS, "not_ready", "recording"]);
        spoken.extend([Supply::Connected, Supply::Disconnected, Supply::Unstable].map(supply));
        let mut reserve = crate::misc::power::Reserve::new();
        let minutes = |m: u64| Some(std::time::Duration::from_secs(m * 60));
        for (charge, disk) in [(14, 14), (4, 4)] {
            for step in reserve.poll(minutes(charge), minutes(disk)) {
                spoken.push(low(step).unwrap_or("none"));
            }
        }
        spoken.extend([
            "recording.cancelled",
            "recording.stopped",
            "marked_failed",
            "marked_failed.ended",
            "subtask.warning",
        ]);
        for pct in (0..=100).step_by(5) {
            for minutes in (0..=200u64).step_by(5) {
                let left = std::time::Duration::from_secs(minutes * 60);
                let eta = Some(crate::misc::power::Eta::Empty(left));
                for (plugged, charging) in [(false, false), (true, false), (true, true)] {
                    spoken.extend(battery(Some(Battery { pct, plugged, charging, eta })));
                }
            }
            let unknown = Battery { pct, plugged: false, charging: false, eta: None };
            spoken.extend(battery(Some(unknown)));
        }
        spoken.extend(battery(None));
        spoken.extend(address(Some("0123456789.:")));
        spoken.extend(address(None));
        spoken.extend([moving(0), moving(1)]);
        spoken.sort();
        spoken.dedup();
        let guided = crate::view::guide::all();
        spoken.extend(guided.iter().filter(|id| !id.ends_with("gripper_sensor.silent")));
        spoken.sort();
        let mut all: Vec<&str> = EN.iter().map(|&(id, _)| id).collect();
        all.sort();
        assert_eq!(spoken, all, "every clip is reachable");
    }

    #[test]
    fn a_refusal_names_one_reason_after_not_ready() {
        let reason = Reason { source: SOURCE_NAMES[2], fault: Fault::NotFraming };
        assert_eq!(not_ready(Some(&reason)), ["not_ready", "head_cam.stalled"]);
        assert_eq!(not_ready(None), ["not_ready"]);
        let unknown = Reason { source: "gate", fault: Fault::Uncalibrated };
        assert_eq!(not_ready(Some(&unknown)), ["not_ready"]);
    }

    #[test]
    fn a_take_ending_says_why() {
        let stalled = Reason { source: SOURCE_NAMES[0], fault: Fault::NotFraming };
        let overrun = Reason { source: "subtask", fault: Fault::Overrun };
        let cancelled = spoken(Feedback::Cancelled, Some(stalled));
        assert_eq!(cancelled.unwrap(), ["recording.cancelled", "left_cam.stalled"]);
        assert_eq!(spoken(Feedback::Cancelled, None).unwrap(), ["recording.cancelled"]);
        let stopped = spoken(Feedback::EndedFailed, Some(overrun));
        assert_eq!(stopped.unwrap(), ["recording.stopped", "subtask.overrun"]);
        assert_eq!(spoken(Feedback::MarkedFailed, None).unwrap(), ["marked_failed"]);
        assert_eq!(spoken(Feedback::EndedFailed, None).unwrap(), ["marked_failed.ended"]);
        assert_eq!(spoken(Feedback::Fault, Some(stalled)).unwrap(), ["left_cam.stalled"]);
        assert_eq!(spoken(Feedback::Started, None), None, "boundaries keep their tones");
    }

    #[test]
    fn a_press_during_a_take_names_the_fault_without_not_ready() {
        let lost = Reason { source: SOURCE_NAMES[5], fault: Fault::Lost };
        assert_eq!(during_take(Some(&lost)), ["left_controller.lost"]);
        assert_eq!(during_take(None), ["recording"]);
    }

    #[test]
    fn the_battery_is_said_in_steps() {
        use crate::misc::power::Eta;
        let draining = |pct: i64, minutes: u64| Battery {
            pct,
            plugged: false,
            charging: false,
            eta: Some(Eta::Empty(std::time::Duration::from_secs(minutes * 60))),
        };
        assert_eq!(
            battery(Some(draining(87, 100))),
            ["battery.pct.85", "battery.not_charging", "battery.left.105"]
        );
        assert_eq!(battery(Some(draining(3, 10)))[2], "battery.left.under");
        assert_eq!(battery(Some(draining(100, 300)))[2], "battery.left.over");
        let unknown = Battery { pct: 64, plugged: false, charging: false, eta: None };
        assert_eq!(battery(Some(unknown))[2], "battery.left.unknown");
        let losing = Battery { plugged: true, ..draining(94, 100) };
        assert_eq!(
            battery(Some(losing)),
            ["battery.pct.95", "battery.plugged_not_charging", "battery.left.105"]
        );
        let charging = Battery {
            pct: 40,
            plugged: true,
            charging: true,
            eta: Some(Eta::Full(std::time::Duration::from_secs(600))),
        };
        assert_eq!(battery(Some(charging)), ["battery.pct.40", "battery.charging"]);
        assert_eq!(battery(None), ["battery.unknown"]);
    }

    #[test]
    fn the_address_is_said_character_by_character() {
        assert_eq!(
            address(Some("10.0.1.5:8080")),
            [
                "digit.1", "digit.0", "dot", "digit.0", "dot", "digit.1", "dot", "digit.5",
                "colon", "digit.8", "digit.0", "digit.8", "digit.0",
            ]
        );
        assert_eq!(address(None), ["address.none"]);
    }

    #[test]
    fn speech_doubles_the_rate_and_ends_at_silence() {
        let pcm = pcm(&["ready"]);
        assert_eq!(pcm.len(), 2 * samples(EN[0].1).len());
        assert!(pcm[pcm.len() - 1].abs() < 50, "a step at the end clicks");
    }
}
