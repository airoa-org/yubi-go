use std::sync::Arc;
use std::time::Duration;

use crate::hub::{Hub, SourceStats, GRIPPER_PAIR, SLOT_HEAD, SOURCES, SOURCE_NAMES};
use crate::misc::clock::boottime_ns;
use airoa_sdk::gate::{Group, GroupState, BLOCK_START};
// This module owns the gate half of the airoa_core boundary. Everything else
// that needs a verdict's shape takes it from here, so the SDK's gate types
// have one door into the app rather than one per caller.
pub use airoa_sdk::gate::{Condition, GroupVerdict, Verdict, HARD_STOP, OK};

/// Less free space than this refuses a start. A three-minute take writes
/// ~0.9 GB with `h264` at the 18 GB/h `misc::power` measured, and the rest
/// is reserve, because Android fails writes near zero. `jpeg` writes 88 GB/h,
/// so it fills this floor in ~90 s and can outrun it.
pub const SPACE_FLOOR: u64 = 2 << 30;
/// How long a pose may go untracked before it counts, so a blink does not
/// refuse a start.
pub const TRACKING_FLOOR: Duration = Duration::from_secs(1);
/// Every USB source silent at once: one reason for the hub, not five.
pub const USB_HUB: &str = "hub";
/// The wrist cameras as a pair, for whether they are matched to the grippers.
pub const CAMERAS: &str = "cameras";
/// The upstream reference gate config's duration limits: a subtask past
/// `subtask_warn` blocks a start, and one past `subtask_limit` stops the take.
pub const SUBTASK_WARN: Duration = Duration::from_secs(90);
pub const SUBTASK_LIMIT: Duration = Duration::from_secs(120);

/// Whether a source has gone untracked for long enough to count. The sensor
/// reports how long; deciding what "long enough" means is this module's, so
/// the threshold does not leak back into `sensors/`.
fn lost_tracking(stats: &SourceStats) -> bool {
    stats.untracked_sec >= TRACKING_FLOOR.as_secs_f32()
}

const SRC_CAM: [usize; 3] = crate::hub::SRC_CAM;
const SRC_ENCODER: [usize; 2] = crate::hub::SRC_ENCODER;
const SRC_CONTROLLER: [usize; 2] = crate::hub::SRC_CONTROLLER;
const SRC_HMD: usize = crate::hub::SRC_HMD;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
    Silent,
    NotFraming,
    Starved,
    Uncalibrated,
    NoTask,
    LowSpace,
    LowCharge,
    Untracked,
    Lost,
    Overrun,
    Settling,
    CoreDown,
    Unmatched,
}

impl Fault {
    pub fn cancels_the_take(self) -> bool {
        matches!(self, Fault::Silent | Fault::NotFraming)
    }

    pub fn stops_the_take(self) -> bool {
        matches!(self, Fault::Overrun)
    }

    pub fn text(self) -> &'static str {
        match self {
            Fault::Silent => "no data",
            Fault::NotFraming => "stalled; replug the hub",
            Fault::Starved => "well below its rate",
            Fault::Uncalibrated => "never calibrated",
            Fault::NoTask => "none assigned — the dashboard queue is empty",
            Fault::LowSpace => "no room to record",
            Fault::LowCharge => "no charge to record",
            Fault::Untracked => "extrapolated, not seen",
            Fault::Lost => "no pose at all",
            Fault::Overrun => "subtask over the 120 s limit",
            Fault::Settling => "settling — waiting for the rig to hold steady",
            Fault::CoreDown => "airoa_core is not running",
            Fault::Unmatched => "not yet matched to the grippers; turn a gripper",
        }
    }

    /// What the operator reads, short enough for a panel column. [`Fault::text`]
    /// stays the fuller wording for logs and `/state`.
    pub fn shown(self) -> &'static str {
        match self {
            Fault::Silent => "no data",
            Fault::NotFraming => "stalled, replug the hub",
            Fault::Starved => "rate far too low",
            Fault::Uncalibrated => "needs calibration",
            Fault::NoTask => "nothing assigned",
            Fault::LowSpace => "disk too full",
            Fault::LowCharge => "battery too low",
            Fault::Untracked => "not tracked",
            Fault::Lost => "lost",
            Fault::Overrun => "over 120 s",
            Fault::Settling => "settling",
            Fault::CoreDown => "airoa_core not running",
            Fault::Unmatched => "turn a gripper to match",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reason {
    pub source: &'static str,
    pub fault: Fault,
}

impl Reason {
    pub fn text(&self) -> String {
        format!("{}: {}", self.source, self.fault.text())
    }

    pub fn shown(&self) -> String {
        format!("{}: {}", self.source, self.fault.shown())
    }

    /// Lowest first. A reason whose fix may clear others comes before them,
    /// and one that clears by itself comes last. The voice speaks only the
    /// first, so this order decides what the operator hears.
    fn priority(&self) -> (u8, u8, usize) {
        let index = SOURCE_NAMES.iter().position(|&s| s == self.source);
        let group = match (self.fault, index) {
            (Fault::CoreDown, _) => 0,
            (Fault::Overrun, _) => 1,
            (Fault::LowCharge | Fault::LowSpace, _) => 2,
            _ if self.source == USB_HUB => 3,
            (_, Some(i)) if SRC_CAM.contains(&i) => 4,
            (_, Some(i)) if SRC_ENCODER.contains(&i) => 5,
            (_, Some(_)) => 6,
            (Fault::Uncalibrated | Fault::Unmatched, _) => 7,
            (Fault::NoTask, _) => 8,
            _ => 9,
        };
        let fault = match self.fault {
            Fault::NotFraming => 0,
            Fault::Silent => 1,
            Fault::Lost => 2,
            Fault::Untracked => 3,
            _ => 4,
        };
        (group, fault, index.unwrap_or(0))
    }
}

fn by_priority(mut reasons: Vec<Reason>) -> Vec<Reason> {
    reasons.sort_by_key(Reason::priority);
    reasons
}

#[derive(Clone, Copy, Default)]
pub struct Inputs {
    pub stats: [SourceStats; SOURCES],
    pub calibrated: bool,
    pub has_task: bool,
    pub free: Option<u64>,
    pub charge: Option<Duration>,
    pub subtask_elapsed: Option<Duration>,
    /// No camera on the headset (`Config::head_camera` off), so its slot is not judged.
    pub head_absent: bool,
    /// The wrist cameras are known to be in the right slots.
    pub cameras_matched: bool,
}

pub fn faults(stats: &[SourceStats; SOURCES]) -> [Option<Fault>; SOURCES] {
    std::array::from_fn(|i| {
        let s = &stats[i];
        match () {
            _ if !s.live && s.bytes_mb_s > 0.0 => Some(Fault::NotFraming),
            _ if !s.live => Some(Fault::Silent),
            _ if s.starved() => Some(Fault::Starved),
            _ => None,
        }
    })
}

pub fn start_reasons(inputs: &Inputs) -> Vec<Reason> {
    let mut reasons = Vec::new();
    for (i, fault) in faults(&inputs.stats).into_iter().enumerate() {
        if i == SRC_CAM[SLOT_HEAD] && inputs.head_absent {
            continue;
        }
        if let Some(fault) = fault {
            reasons.push(Reason { source: SOURCE_NAMES[i], fault });
        }
    }
    for &i in SRC_CONTROLLER.iter().chain(&[SRC_HMD]) {
        if lost_tracking(&inputs.stats[i]) {
            let fault = if inputs.stats[i].lost { Fault::Lost } else { Fault::Untracked };
            reasons.push(Reason { source: SOURCE_NAMES[i], fault });
        }
    }
    let usb: Vec<&str> = SRC_CAM
        .iter()
        .filter(|&&i| !(i == SRC_CAM[SLOT_HEAD] && inputs.head_absent))
        .chain(&SRC_ENCODER)
        .map(|&i| SOURCE_NAMES[i])
        .collect();
    let silent = |source| reasons.contains(&Reason { source, fault: Fault::Silent });
    if usb.iter().all(|&source| silent(source)) {
        reasons.retain(|reason| !usb.contains(&reason.source));
        reasons.push(Reason { source: USB_HUB, fault: Fault::Silent });
    }
    if !inputs.calibrated {
        reasons.push(Reason { source: GRIPPER_PAIR, fault: Fault::Uncalibrated });
    }
    if !inputs.cameras_matched {
        reasons.push(Reason { source: CAMERAS, fault: Fault::Unmatched });
    }
    if !inputs.has_task {
        reasons.push(Reason { source: "task", fault: Fault::NoTask });
    }
    if inputs.free.is_some_and(|free| free < SPACE_FLOOR) {
        reasons.push(Reason { source: "storage", fault: Fault::LowSpace });
    }
    if inputs.charge.is_some_and(|left| left < crate::misc::power::CHARGE_FLOOR) {
        reasons.push(Reason { source: "battery", fault: Fault::LowCharge });
    }
    by_priority(reasons)
}

pub fn verdict_reasons(verdict: &airoa_sdk::gate::Verdict) -> Vec<Reason> {
    let mut reasons = Vec::new();
    for condition in verdict.groups.iter().flat_map(|group| &group.failing) {
        let source = SOURCE_NAMES.iter().copied().find(|name| *name == condition.name);
        let reason = source.and_then(|source| {
            let fault = match condition.reason.as_str() {
                text if text == Fault::Silent.text() => Fault::Silent,
                text if text == Fault::NotFraming.text() => Fault::NotFraming,
                text if text == Fault::Starved.text() => Fault::Starved,
                _ => return None,
            };
            Some(Reason { source, fault })
        });
        if let Some(reason) = reason {
            if !reasons.contains(&reason) {
                reasons.push(reason);
            }
            continue;
        }
        if let Some(source) = condition
            .name
            .strip_suffix(" tracked")
            .and_then(|name| SOURCE_NAMES.iter().copied().find(|source| *source == name))
        {
            if !reasons.iter().any(|reason| reason.source == source) {
                let fault = match condition.reason == Fault::Lost.text() {
                    true => Fault::Lost,
                    false => Fault::Untracked,
                };
                reasons.push(Reason { source, fault });
            }
        } else if condition.name == "subtask_limit" {
            reasons.push(Reason { source: "subtask", fault: Fault::Overrun });
        }
    }
    by_priority(reasons)
}

pub fn verdict_changes(
    before: Option<&airoa_sdk::gate::Verdict>,
    after: &airoa_sdk::gate::Verdict,
) -> (Vec<Reason>, Vec<Reason>) {
    let before = before.map(verdict_reasons).unwrap_or_default();
    let after = verdict_reasons(after);
    let opened = after.iter().copied().filter(|reason| !before.contains(reason)).collect();
    let closed = before.iter().copied().filter(|reason| !after.contains(reason)).collect();
    (opened, closed)
}

const fn group(name: &str, recovery_sec: u64) -> Group<'_> {
    Group { name, settle: Duration::from_secs(0), recovery: Duration::from_secs(recovery_sec) }
}

pub const GROUPS: [Group<'static>; 6] = [
    group("health", 1),
    group("tf", 1),
    group("vr_tracking", 1),
    group("calibration", 1),
    group("task", 1),
    group("duration_limits", 1),
];

pub fn checkers(inputs: &Inputs) -> Vec<(String, Vec<Condition>)> {
    let per_source = faults(&inputs.stats);
    let condition = |name: String, passed: bool, reason: String, escalation: u8| Condition {
        name,
        passed,
        reason,
        escalation,
        settle_exempt: false,
    };
    let source = |i: usize, escalation: u8| {
        condition(
            SOURCE_NAMES[i].to_string(),
            per_source[i].is_none(),
            per_source[i].map_or("ok", Fault::text).to_string(),
            escalation,
        )
    };
    let tracking = |i: usize| {
        condition(
            format!("{} tracked", SOURCE_NAMES[i]),
            !lost_tracking(&inputs.stats[i]),
            match (lost_tracking(&inputs.stats[i]), inputs.stats[i].lost) {
                (true, true) => Fault::Lost.text().to_string(),
                (true, false) => "extrapolated — not tracked".to_string(),
                (false, _) => "ok".to_string(),
            },
            BLOCK_START,
        )
    };
    let elapsed = inputs.subtask_elapsed;
    let duration = |name: &str, cap: Duration, escalation: u8| {
        condition(
            name.to_string(),
            !elapsed.is_some_and(|t| t >= cap),
            match elapsed {
                Some(t) if t >= cap => {
                    format!("{:.0} s over the {} s cap", t.as_secs_f32(), cap.as_secs())
                }
                _ => "ok".to_string(),
            },
            escalation,
        )
    };
    let head = (!inputs.head_absent).then(|| source(SRC_CAM[SLOT_HEAD], BLOCK_START));
    vec![
        (
            "health".to_string(),
            [source(SRC_CAM[0], HARD_STOP), source(SRC_CAM[1], HARD_STOP)]
                .into_iter()
                .chain(head)
                .chain([
                    source(SRC_ENCODER[0], HARD_STOP),
                    source(SRC_ENCODER[1], HARD_STOP),
                    condition(
                        "storage".to_string(),
                        !inputs.free.is_some_and(|free| free < SPACE_FLOOR),
                        match inputs.free {
                            Some(free) if free < SPACE_FLOOR => format!(
                                "{:.1} GB free — {}",
                                free as f64 / (1u64 << 30) as f64,
                                Fault::LowSpace.text()
                            ),
                            Some(_) => "ok".to_string(),
                            None => "unknown".to_string(),
                        },
                        BLOCK_START,
                    ),
                    condition(
                        "battery".to_string(),
                        !inputs.charge.is_some_and(|left| left < crate::misc::power::CHARGE_FLOOR),
                        match inputs.charge {
                            Some(left) if left < crate::misc::power::CHARGE_FLOOR => format!(
                                "{} min left — {}",
                                left.as_secs() / 60,
                                Fault::LowCharge.text()
                            ),
                            Some(left) => format!("{} min left", left.as_secs() / 60),
                            None => "not falling".to_string(),
                        },
                        BLOCK_START,
                    ),
                ])
                .collect(),
        ),
        (
            "tf".to_string(),
            vec![
                source(SRC_CONTROLLER[0], BLOCK_START),
                source(SRC_CONTROLLER[1], BLOCK_START),
                source(SRC_HMD, BLOCK_START),
            ],
        ),
        (
            "vr_tracking".to_string(),
            vec![tracking(SRC_CONTROLLER[0]), tracking(SRC_CONTROLLER[1]), tracking(SRC_HMD)],
        ),
        (
            "calibration".to_string(),
            vec![
                condition(
                    GRIPPER_PAIR.to_string(),
                    inputs.calibrated,
                    match inputs.calibrated {
                        true => "ok".to_string(),
                        false => Fault::Uncalibrated.text().to_string(),
                    },
                    BLOCK_START,
                ),
                condition(
                    CAMERAS.to_string(),
                    inputs.cameras_matched,
                    match inputs.cameras_matched {
                        true => "ok".to_string(),
                        false => Fault::Unmatched.text().to_string(),
                    },
                    BLOCK_START,
                ),
            ],
        ),
        (
            "task".to_string(),
            vec![condition(
                "assigned".to_string(),
                inputs.has_task,
                match inputs.has_task {
                    true => "ok".to_string(),
                    false => Fault::NoTask.text().to_string(),
                },
                BLOCK_START,
            )],
        ),
        (
            "duration_limits".to_string(),
            vec![
                duration("subtask_warn", SUBTASK_WARN, BLOCK_START),
                duration("subtask_limit", SUBTASK_LIMIT, HARD_STOP),
            ],
        ),
    ]
}

/// 8 Hz, twice the `eval_rate: 4.0` of the YUBI ROS stack (yubi-sw,
/// `recording_gate.yaml`; upstream's
/// code default is 2 Hz). The rate bounds how much bad data reaches a take
/// before a HARD_STOP, and one pass over fifteen conditions costs 133 µs, so
/// 8 Hz spends 1.1 ms of every second for the lower latency.
const GATE_PERIOD: Duration = Duration::from_millis(125);

/// Gather the conditions, let airoa_core decide, hand the verdict to the hub,
/// then let the hub act on it. Until the core is up it decides nothing, so
/// the gate stays where it started: closed.
pub(crate) fn spawn_tick(hub: Arc<Hub>) {
    std::thread::spawn(move || {
        let mut states: Option<Vec<GroupState>> = None;
        loop {
            std::thread::sleep(GATE_PERIOD);
            if crate::misc::core::up() {
                let now = boottime_ns() as f64 / 1e9;
                let states = states.get_or_insert_with(|| {
                    GROUPS.iter().map(|&group| GroupState::new(group, now)).collect()
                });
                let conditions = checkers(&hub.gate_inputs());
                let prev = hub.gate_level();
                match airoa_sdk::gate::orchestrate(states, &conditions, prev, now) {
                    Ok(fresh) => hub.apply_verdict(fresh),
                    Err(e) => log::warn!("gate: {e}"),
                }
            }
            hub.tick_controls();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Inputs {
        let live = SourceStats { hz: 30.0, live: true, ..Default::default() };
        Inputs {
            stats: [live; SOURCES],
            calibrated: true,
            has_task: true,
            free: Some(SPACE_FLOOR * 10),
            charge: Some(Duration::from_secs(3600)),
            subtask_elapsed: None,
            head_absent: false,
            cameras_matched: true,
        }
    }

    #[test]
    fn unmatched_wrist_cameras_refuse_the_start() {
        let mut inputs = healthy();
        inputs.cameras_matched = false;
        assert_eq!(start_reasons(&inputs), [Reason { source: CAMERAS, fault: Fault::Unmatched }]);
        let failing: Vec<_> = checkers(&inputs)
            .into_iter()
            .flat_map(|(_, conditions)| conditions)
            .filter(|c| !c.passed)
            .collect();
        assert_eq!(failing.len(), 1);
        assert_eq!((failing[0].name.as_str(), failing[0].escalation), (CAMERAS, BLOCK_START));
    }

    fn verdict(failing: Vec<Condition>) -> airoa_sdk::gate::Verdict {
        airoa_sdk::gate::Verdict {
            gate_level: HARD_STOP,
            groups: vec![airoa_sdk::gate::GroupVerdict {
                name: "health".to_string(),
                level: HARD_STOP,
                settled: true,
                settle_is_initial: false,
                settle_start_time: 0.0,
                results: failing.clone(),
                failing,
            }],
        }
    }

    fn checked_reasons(inputs: &Inputs) -> Vec<Reason> {
        let failing = checkers(inputs)
            .into_iter()
            .flat_map(|(_, conditions)| conditions)
            .filter(|c| !c.passed)
            .collect();
        verdict_reasons(&verdict(failing))
    }

    #[test]
    fn a_verdict_transition_opens_and_closes_a_recorded_fault_window() {
        let healthy = verdict(Vec::new());
        let silent = verdict(vec![Condition {
            name: SOURCE_NAMES[0].to_string(),
            passed: false,
            reason: Fault::Silent.text().to_string(),
            escalation: HARD_STOP,
            settle_exempt: false,
        }]);
        let reason = Reason { source: SOURCE_NAMES[0], fault: Fault::Silent };

        assert_eq!(verdict_changes(Some(&healthy), &silent), (vec![reason], Vec::new()));
        assert_eq!(verdict_changes(Some(&silent), &healthy), (Vec::new(), vec![reason]));
    }

    /// A rig with no headset camera is not refused for its silent slot, and
    /// the slot gets no condition at all rather than a passing one.
    #[test]
    fn an_absent_head_camera_is_not_judged() {
        let mut inputs = healthy();
        inputs.stats[SRC_CAM[SLOT_HEAD]].live = false;
        assert!(!start_reasons(&inputs).is_empty(), "a fitted head camera that is silent refuses");
        inputs.head_absent = true;
        assert!(start_reasons(&inputs).is_empty());
        let names: Vec<String> =
            checkers(&inputs).into_iter().flat_map(|(_, c)| c).map(|c| c.name).collect();
        assert!(!names.iter().any(|n| n == SOURCE_NAMES[SRC_CAM[SLOT_HEAD]]), "{names:?}");
    }

    #[test]
    fn a_healthy_rig_passes_every_checker() {
        assert!(start_reasons(&healthy()).is_empty());
        for (_, conditions) in checkers(&healthy()) {
            assert!(conditions.iter().all(|c| c.passed), "{conditions:?}");
        }
    }

    #[test]
    fn each_start_condition_refuses_alone() {
        for (mutate, source) in [
            (
                Box::new(|i: &mut Inputs| i.stats[SRC_CAM[0]].live = false)
                    as Box<dyn Fn(&mut Inputs)>,
                "left wrist cam",
            ),
            (Box::new(|i: &mut Inputs| i.calibrated = false), GRIPPER_PAIR),
            (Box::new(|i: &mut Inputs| i.has_task = false), "task"),
            (Box::new(|i: &mut Inputs| i.free = Some(0)), "storage"),
            (Box::new(|i: &mut Inputs| i.charge = Some(Duration::ZERO)), "battery"),
            (Box::new(|i: &mut Inputs| i.stats[SRC_HMD].untracked_sec = 2.0), "headset"),
        ] {
            let mut inputs = healthy();
            mutate(&mut inputs);
            let reasons = start_reasons(&inputs);
            assert!(reasons.iter().any(|r| r.source == source), "{source}: {reasons:?}");
        }
    }

    #[test]
    fn an_unknown_disk_or_battery_does_not_refuse() {
        let mut inputs = healthy();
        inputs.free = None;
        inputs.charge = None;
        assert!(start_reasons(&inputs).is_empty());
    }

    #[test]
    fn a_stalled_camera_cancels_and_a_starved_one_does_not() {
        let mut inputs = healthy();
        inputs.stats[SRC_CAM[0]].live = false;
        inputs.stats[SRC_CAM[0]].bytes_mb_s = 2.4;
        let reasons = checked_reasons(&inputs);
        assert!(reasons.iter().any(|r| r.fault == Fault::NotFraming));
        assert!(Fault::NotFraming.cancels_the_take());
        assert!(!Fault::Starved.cancels_the_take());
    }

    #[test]
    fn the_subtask_overrun_stops_but_never_blocks_a_start() {
        let mut inputs = healthy();
        inputs.subtask_elapsed = Some(Duration::from_secs(130));
        assert!(start_reasons(&inputs).is_empty(), "duration is not a start gate");
        let reasons = checked_reasons(&inputs);
        assert!(reasons.iter().any(|r| r.fault == Fault::Overrun), "{reasons:?}");
        assert!(Fault::Overrun.stops_the_take());
        assert!(!Fault::Overrun.cancels_the_take());

        let all = checkers(&inputs);
        let (_, limits) = all.iter().find(|(name, _)| name == "duration_limits").unwrap();
        assert!(limits.iter().all(|c| !c.passed));

        inputs.subtask_elapsed = Some(Duration::from_secs(100));
        let all = checkers(&inputs);
        let (_, limits) = all.iter().find(|(name, _)| name == "duration_limits").unwrap();
        assert!(limits.iter().find(|c| c.name == "subtask_warn").is_some_and(|c| !c.passed));
        assert!(limits.iter().find(|c| c.name == "subtask_limit").is_some_and(|c| c.passed));
    }

    #[test]
    fn a_pose_with_no_validity_is_lost_and_one_still_estimated_is_untracked() {
        let mut inputs = healthy();
        inputs.stats[SRC_HMD].untracked_sec = 2.0;
        let fault = |inputs: &Inputs| start_reasons(inputs)[0].fault;
        let checked = |inputs: &Inputs| checked_reasons(inputs)[0].fault;
        assert_eq!((fault(&inputs), checked(&inputs)), (Fault::Untracked, Fault::Untracked));
        inputs.stats[SRC_HMD].lost = true;
        assert_eq!((fault(&inputs), checked(&inputs)), (Fault::Lost, Fault::Lost));
    }

    #[test]
    fn every_usb_source_silent_is_one_reason_for_the_hub() {
        let mut inputs = healthy();
        for i in SRC_CAM.into_iter().chain(SRC_ENCODER) {
            inputs.stats[i].live = false;
        }
        let hub = Reason { source: USB_HUB, fault: Fault::Silent };
        assert_eq!(start_reasons(&inputs), [hub]);
        inputs.head_absent = true;
        assert_eq!(start_reasons(&inputs), [hub], "an absent head camera is not needed");
        inputs.head_absent = false;
        inputs.stats[SRC_CAM[SLOT_HEAD]].live = true;
        assert_eq!(start_reasons(&inputs).len(), 4, "one live source means the hub is there");
    }

    #[test]
    fn reasons_come_in_priority_order() {
        let mut inputs = healthy();
        inputs.has_task = false;
        inputs.calibrated = false;
        inputs.stats[SRC_CONTROLLER[0]].untracked_sec = 2.0;
        inputs.stats[SRC_CONTROLLER[1]].untracked_sec = 2.0;
        inputs.stats[SRC_CONTROLLER[1]].lost = true;
        inputs.stats[SRC_ENCODER[0]].live = false;
        inputs.stats[SRC_CAM[0]].nominal = 30.0;
        inputs.stats[SRC_CAM[0]].hz = 5.0;
        inputs.stats[SRC_CAM[1]].live = false;
        inputs.stats[SRC_CAM[1]].bytes_mb_s = 2.4;
        inputs.charge = Some(Duration::ZERO);
        let order: Vec<_> = start_reasons(&inputs).iter().map(Reason::text).collect();
        assert_eq!(
            order,
            [
                "battery: no charge to record",
                "right wrist cam: stalled; replug the hub",
                "left wrist cam: well below its rate",
                "left gripper: no data",
                "right controller: no pose at all",
                "left controller: extrapolated, not seen",
                "grippers: never calibrated",
                "task: none assigned — the dashboard queue is empty",
            ]
        );
    }

    #[test]
    fn group_names_match_what_the_dashboard_already_binds_on() {
        let names: Vec<_> = checkers(&healthy()).into_iter().map(|(name, _)| name).collect();
        assert_eq!(
            names,
            ["health", "tf", "vr_tracking", "calibration", "task", "duration_limits"]
        );
        assert_eq!(GROUPS.len(), names.len());
    }
}
