//! The writer thread: drains `Rec` from the hub into one `Recorder` at a
//! time, keeping a pre-roll so a take can start from before the button.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use crate::hub::{Hub, Rec, SRC_CAM};
use crate::record::record::Recorder;
use crate::record::video;

/// How much of the run-up to a take the pre-roll keeps. This is a bound, not
/// the working size, because the gesture resolves within `hub::ARM_NS`.
const PREROLL_NS: u64 = 2_000_000_000;

#[derive(Default)]
struct PreRoll(std::collections::VecDeque<(u64, Rec)>);

impl PreRoll {
    fn push(&mut self, at_ns: u64, msg: Rec) {
        self.0.push_back((at_ns, msg));
        while self.0.front().is_some_and(|&(t, _)| at_ns.saturating_sub(t) > PREROLL_NS) {
            self.0.pop_front();
        }
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    /// Everything from `from_ns` on, except that each camera's video starts
    /// at its last key frame before `from_ns`, because the frames after it
    /// cannot be decoded without it.
    fn drain_into(&mut self, r: &mut Recorder, from_ns: u64) {
        let mut video_from = [usize::MAX; 3];
        for (i, (at_ns, msg)) in self.0.iter().enumerate() {
            if let Rec::Video { slot, key_frame: true, .. } = *msg {
                if let Some(first) = video_from.get_mut(slot) {
                    if *at_ns <= from_ns || *first == usize::MAX {
                        *first = i;
                    }
                }
            }
        }
        for (i, (at_ns, msg)) in std::mem::take(&mut self.0).into_iter().enumerate() {
            if let Rec::Video { at_ns, capture, slot, data, key_frame } = msg {
                if from_ns != u64::MAX && video_from.get(slot).is_some_and(|&first| i >= first) {
                    r.video(at_ns, capture, slot, &data, key_frame);
                }
                continue;
            }
            if at_ns < from_ns {
                continue;
            }
            match msg {
                Rec::Frame { at_ns, capture, slot, jpeg } => r.frame(at_ns, capture, slot, &jpeg),
                Rec::Pose { at_ns, capture, part, located } => {
                    r.pose(at_ns, capture, part, located)
                }
                Rec::Gripper { at_ns, seq, side, raw, calib } => {
                    r.gripper(at_ns, seq, side, raw, calib)
                }
                Rec::Joy { at_ns, side, axes, buttons } => r.joy(at_ns, side, &axes, &buttons),
                Rec::InputChanges { at_ns, side, changes } => {
                    r.input_changes(at_ns, side, &changes)
                }
                _ => {}
            }
        }
    }
}

/// The video the encoders still hold at a stop, captured before `stop_ns`,
/// goes into the take. Everything else that arrives meanwhile goes to `held`,
/// for the loop to handle after the take has closed.
fn collect_video(
    hub: &Hub,
    rx: &Receiver<Rec>,
    r: &mut Recorder,
    stop_ns: u64,
    held: &mut VecDeque<Rec>,
) {
    let mut route = |msg| match msg {
        Rec::Video { at_ns, capture, slot, data, key_frame } if at_ns <= stop_ns => {
            r.video(at_ns, capture, slot, &data, key_frame)
        }
        msg => held.push_back(msg),
    };
    let mut waiting = hub.flush_video();
    let until = Instant::now() + video::FLUSH_WAIT;
    while !waiting.is_empty() && Instant::now() < until {
        if let Ok(msg) = rx.recv_timeout(Duration::from_millis(5)) {
            route(msg);
        }
        waiting.retain(|done| matches!(done.try_recv(), Err(TryRecvError::Empty)));
    }
    // An encoder sends its frames before its answer, so they are queued now.
    while let Ok(msg) = rx.try_recv() {
        route(msg);
    }
}

pub(crate) fn write_episodes(hub: &Hub, rx: Receiver<Rec>, dir: &str) {
    let mut recorder: Option<Recorder> = None;
    let mut last_stop = None;
    let mut pre = PreRoll::default();
    let mut held = VecDeque::new();
    while let Some(msg) = held.pop_front().or_else(|| rx.recv().ok()) {
        if let Some(r) = &mut recorder {
            for line in crate::misc::journal::drain() {
                r.log(&line);
            }
        }
        let (msg, reply) = match msg {
            Rec::Call { command, reply } => (*command, Some(reply)),
            msg => (msg, None),
        };
        let mut answer = Ok(serde_json::json!(true));
        if matches!(msg, Rec::Shutdown) {
            if let Some(reply) = reply {
                let _ = reply.send(answer);
            }
            break;
        }
        match msg {
            Rec::Arm => {}
            Rec::Disarm => {
                if recorder.is_none() {
                    pre.clear();
                }
            }
            Rec::Pause => hub.record_stopped(hub.recording_failed()),
            Rec::Start { gesture, config } => {
                if recorder.is_some() {
                    if let Some(reply) = reply {
                        let _ = reply.send(Err("already recording".into()));
                    }
                    continue;
                }
                let stats = hub.stats();
                last_stop = None;
                let expecting = std::array::from_fn(|slot| stats[SRC_CAM[slot]].live);
                match Recorder::start_with_config(
                    dir,
                    hub.calibration(),
                    expecting,
                    gesture,
                    config,
                ) {
                    Ok(mut r) => {
                        log::info!("recording to {}", r.name);
                        hub.request_key_frames();
                        r.set_head_stereo(hub.head_stereo());
                        pre.drain_into(&mut r, gesture.map_or(u64::MAX, |g| g.task_end_ns));
                        // A fault already standing never opens during the take,
                        // so it would otherwise go unrecorded.
                        let at_ns = crate::misc::clock::boottime_ns();
                        for reason in hub.standing_faults() {
                            r.fault(at_ns, reason.source, reason.fault, true);
                        }
                        recorder = Some(r);
                        hub.record_started();
                    }
                    Err(e) => {
                        log::warn!("cannot start recording: {e}");
                        answer = Err(e.to_string());
                        hub.record_stopped(false);
                    }
                }
            }
            Rec::Stop { success, gesture } => {
                if let Some(mut r) = recorder.take() {
                    let dropped = hub.dropped();
                    collect_video(hub, &rx, &mut r, crate::misc::clock::boottime_ns(), &mut held);
                    if dropped > 0 {
                        log::warn!(
                            "episode {}: {dropped} messages dropped (disk too slow)",
                            r.name
                        );
                        r.dropped(crate::misc::clock::boottime_ns(), dropped);
                    }
                    let complete = r.finish(success, gesture);
                    last_stop = Some(complete);
                    answer = Ok(serde_json::json!(complete));
                    hub.record_stopped(!complete);
                } else {
                    answer = last_stop
                        .map(|ok| serde_json::json!(ok))
                        .ok_or_else(|| "no recording to stop".into());
                }
            }
            Rec::Advance { gesture } => match &mut recorder {
                Some(r) => r.advance(gesture),
                None => log::warn!("advance with no episode open"),
            },
            Rec::Fail { at_ns } => match &mut recorder {
                Some(r) => r.fail(at_ns),
                None => log::warn!("fail with no episode open"),
            },
            Rec::Resume { gesture } => match &mut recorder {
                Some(r) => r.resume(gesture),
                None => log::warn!("resume with no episode open"),
            },
            Rec::Frame { at_ns, capture, slot, jpeg } => match &mut recorder {
                Some(r) => r.frame(at_ns, capture, slot, &jpeg),
                None => pre.push(at_ns, Rec::Frame { at_ns, capture, slot, jpeg }),
            },
            Rec::Video { at_ns, capture, slot, data, key_frame } => match &mut recorder {
                Some(r) => r.video(at_ns, capture, slot, &data, key_frame),
                None => pre.push(at_ns, Rec::Video { at_ns, capture, slot, data, key_frame }),
            },
            Rec::Pose { at_ns, capture, part, located } => match &mut recorder {
                Some(r) => r.pose(at_ns, capture, part, located),
                None => pre.push(at_ns, Rec::Pose { at_ns, capture, part, located }),
            },
            Rec::Gripper { at_ns, seq, side, raw, calib } => match &mut recorder {
                Some(r) => r.gripper(at_ns, seq, side, raw, calib),
                None => pre.push(at_ns, Rec::Gripper { at_ns, seq, side, raw, calib }),
            },
            Rec::Joy { at_ns, side, axes, buttons } => match &mut recorder {
                Some(r) => r.joy(at_ns, side, &axes, &buttons),
                None => pre.push(at_ns, Rec::Joy { at_ns, side, axes, buttons }),
            },
            Rec::InputChanges { at_ns, side, changes } => match &mut recorder {
                Some(r) => r.input_changes(at_ns, side, &changes),
                None => pre.push(at_ns, Rec::InputChanges { at_ns, side, changes }),
            },
            Rec::Status { at_ns, event, statuses } => {
                if let Some(r) = &mut recorder {
                    r.status(at_ns, event, &statuses);
                }
            }
            Rec::Fault { at_ns, source, fault } => {
                if let Some(r) = &mut recorder {
                    r.fault(at_ns, source, fault, true);
                }
            }
            Rec::FaultOver { at_ns, source, fault } => {
                if let Some(r) = &mut recorder {
                    r.fault(at_ns, source, fault, false);
                }
            }
            Rec::Cancel => {
                if let Some(r) = recorder.take() {
                    r.interrupted();
                }
                hub.record_stopped(hub.recording_failed());
            }
            Rec::Uuid(uuid) => match &mut recorder {
                Some(r) => r.set_uuid(uuid),
                None => answer = Err("no recorder for episode UUID".into()),
            },
            Rec::Call { .. } | Rec::Shutdown => answer = Err("invalid recorder request".into()),
        }
        if recorder.as_ref().is_some_and(|r| r.failed()) {
            if let Some(r) = recorder.take() {
                r.finish(false, None);
            }
            hub.record_stopped(true);
            hub.send_task(crate::record::task::Event::Cancel);
        }
        crate::misc::journal::keep(recorder.is_some());
        hub.set_take_open(recorder.is_some());
        if let Some(reply) = reply {
            let _ = reply.send(answer);
        }
    }
    if let Some(r) = recorder.take() {
        r.interrupted();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::misc::capture::Capture;

    fn video(at_ns: u64, key_frame: bool) -> Rec {
        let data = at_ns.to_le_bytes().to_vec();
        Rec::Video { at_ns, capture: Capture::default(), slot: 0, data, key_frame }
    }

    /// The pre-roll keeps the gesture's start for everything else, but a
    /// camera's video goes back to the key frame the first kept frame needs.
    #[test]
    fn the_pre_roll_starts_each_cameras_video_at_a_key_frame() {
        let base =
            std::env::temp_dir().join(format!("yubigo_writer_preroll-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let mut r =
            Recorder::start(base.to_str().unwrap(), Default::default(), [false; 3], None).unwrap();
        let path = base.join(crate::record::record::TAKES).join(&r.name).join("signals.mcap");
        let mut pre = PreRoll::default();
        for (at_ns, key) in [(100, true), (200, false), (300, true), (400, false), (500, false)] {
            pre.push(at_ns, video(at_ns, key));
        }
        pre.drain_into(&mut r, 450);
        r.finish(true, None);
        let stamps: Vec<u64> = crate::record::mcap::read_all(&path)
            .unwrap()
            .iter()
            .filter(|m| m.0 == "/left_camera/video")
            .map(|m| m.1)
            .collect();
        assert_eq!(stamps.len(), 3, "from the key frame at 300: {stamps:?}");
    }
}
