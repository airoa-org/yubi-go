//! The panel's own thread. Everything the panel shows is read and drawn here,
//! off the render thread: the hub's locks, the disk, the preview JPEGs and
//! resvg all cost milliseconds the 72 Hz frame does not have. The render thread
//! hears back only by `mpsc`: what changed, as patches, once per tick.

use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use reactive_svg::{signal, Getter, Patch, Setter};

use super::episodes::episodes;
use super::guide::{self, Said};
use super::pairing;
use super::panel;
use crate::hub::{
    Feedback, Hub, Roles, SourceStats, EPISODE_DIR, SLOT_HEAD, SRC_CAM, SRC_CONTROLLER,
    SRC_ENCODER, SRC_HMD,
};
use crate::misc::config::{Config, CAMERA_KEYS};
use log::{info, warn};

const GRIPPER_RGBA: [f32; 4] = [0.25, 0.78, 0.88, 0.5];
const GRIPPER_UNCALIBRATED_RGBA: [f32; 4] = [0.88, 0.66, 0.19, 0.5];
const GRIPPER_LOST_RGBA: [f32; 4] = [0.92, 0.24, 0.24, 0.5];

const TASK_REFRESH: Duration = Duration::from_secs(2);
const UI_TICK: Duration = Duration::from_millis(100);
/// How long after a match one wave still names its camera: enough to wave a
/// hand once "Cameras assigned" (1.2 s) ends, and short enough not to speak
/// mid-task later.
const WAVE_WINDOW: Duration = Duration::from_secs(15);

/// What the render thread's controllers decided, for the panel to show.
#[derive(Clone, Default)]
pub struct Input {
    pub qr: bool,
}

/// One tick, for the render thread.
pub struct Tick {
    pub patches: Vec<Patch>,
    pub centre_shown: bool,
    /// The patches hold the QR page, so the render thread may show its quad.
    pub qr_shown: bool,
    pub gripper_rgba: [[f32; 4]; 2],
    pub feedback: Option<Feedback>,
    /// Spoken after `feedback`'s tone (`guide`).
    pub speech: Vec<&'static str>,
}

/// The thread stops when the receiver is dropped, at its next tick.
pub fn spawn(hub: &'static Hub) -> (Setter<Input>, Receiver<Tick>) {
    let (input, set_input) = signal(Input::default());
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("panel".into())
        .spawn(move || run(hub, input, tx))
        .expect("spawn the panel thread");
    (set_input, rx)
}

/// What the panel reads off disk, refreshed every `TASK_REFRESH`.
#[derive(Default)]
struct Disk {
    episodes_on_disk: usize,
    episodes: Vec<panel::Episode>,
    disk: Option<panel::Disk>,
    address: Option<String>,
    refreshed: Option<Instant>,
}

impl Disk {
    fn refresh(&mut self, force: bool, video: crate::misc::config::Video) {
        if !force && self.refreshed.is_some_and(|t| t.elapsed() < TASK_REFRESH) {
            return;
        }
        self.refreshed = Some(Instant::now());
        let names = crate::hub::episodes::list(EPISODE_DIR);
        self.episodes_on_disk = names.len();
        self.episodes = episodes(EPISODE_DIR, &names, 3);
        self.disk = crate::misc::disk::usage(EPISODE_DIR).map(|u| panel::Disk {
            free: u.free,
            used: u.used,
            recording_left: crate::misc::power::recording_left(u.free, video),
        });
        self.address = local_ip();
    }
}

fn run(hub: &'static Hub, mut input: Getter<Input>, tx: Sender<Tick>) {
    let mut ui = panel::Ui::default();
    let mut seen = [0u64; 3];
    let mut thumbs: [Option<Vec<u8>>; 3] = Default::default();
    let mut disk = Disk::default();
    let mut said_calib = Said::default();
    let mut sweeping = false;
    // Finished sweeps when this one began; `calibrations_finished` is read
    // before the prompt for that, and after it for the end.
    let mut sweep_from = 0;
    let mut calibration_presses = 0;
    let mut check = Check::default();
    loop {
        let input = input.get().clone();
        let mut speech = Vec::new();
        let mut feedback = hub.take_fault_cue();
        if feedback.is_none() {
            let low =
                crate::misc::power::poll_reserve(disk.disk.as_ref().map(|d| d.free), hub.video());
            if !low.is_empty() {
                feedback = Some(Feedback::RunningOut);
                speech.extend(low.into_iter().filter_map(super::voice::low));
            }
        }
        let (frames, _, stats) = hub.ui_snapshot(seen);
        let calibration = hub.calibration();
        let gripper_rgba = std::array::from_fn(|side| {
            if !stats[SRC_ENCODER[side]].live {
                GRIPPER_LOST_RGBA
            } else if calibration.side(side).is_none() {
                GRIPPER_UNCALIBRATED_RGBA
            } else {
                GRIPPER_RGBA
            }
        });
        let mut fresh = [false; 3];
        for (i, frame) in frames.into_iter().enumerate() {
            if let Some((jpeg, seq)) = frame {
                seen[i] = seq;
                if let Some(t) = panel::thumbnail(&jpeg) {
                    thumbs[i] = Some(t);
                    fresh[i] = true;
                }
            }
        }
        match check.tick(hub, &thumbs, fresh, &stats) {
            Some(Heard::Assigned) => {
                feedback = feedback.or(Some(Feedback::Assigned));
                speech.push(guide::CAMS_DONE);
            }
            Some(Heard::Moving(slot)) => speech.push(super::voice::moving(slot)),
            None => {}
        }
        disk.refresh(false, hub.video());
        let task = hub.task_view();
        let step = task.step_line();
        let config = &task.config;
        let finished_before = hub.calibrations_finished();
        let (prompt, held, messages) = hub.calibration_ui();
        let finished_after = hub.calibrations_finished();
        let presses_now = hub.calibration_presses();
        if presses_now != calibration_presses {
            calibration_presses = presses_now;
            said_calib = Said::default();
        }
        speech.extend(said_calib.changed(guide::calibration(prompt, &messages)));
        if !sweeping && prompt.is_some() {
            sweep_from = finished_before;
        }
        if std::mem::replace(&mut sweeping, prompt.is_some()) && prompt.is_none() {
            match (finished_after != sweep_from, messages.is_empty()) {
                (true, _) => speech.push(guide::CALIB_DONE),
                (false, true) => speech.push(guide::CALIB_CANCELLED),
                (false, false) => {}
            }
        }
        let prompt = prompt.map(|p| p.text());
        let messages: Vec<String> = messages.iter().map(|m| m.text()).collect();
        let production = config.production();
        let refused = hub.refusal();
        let calibrating = prompt.is_some() || !messages.is_empty();
        let centre_shown = calibrating || !refused.is_empty();
        let task_shown = config.has_task().then(|| config.task().to_string());
        let state = panel::State {
            cams: [
                cam_of(stats[SRC_CAM[0]]),
                cam_of(stats[SRC_CAM[1]]),
                cam_of(stats[SRC_CAM[SLOT_HEAD]]),
            ],
            thumbs: [thumbs[0].as_deref(), thumbs[1].as_deref(), thumbs[2].as_deref()],
            rec_failed: hub.recording_failed(),
            buttons: panel::Buttons { recording: hub.recording(), markable: hub.recording() },
            task: task_shown.as_deref(),
            step: step.as_deref(),
            completed: config.task_completed(),
            jaws: jaws(hub, &stats),
            encoders: [pose_of(stats[SRC_ENCODER[0]]), pose_of(stats[SRC_ENCODER[1]])],
            controllers: [pose_of(stats[SRC_CONTROLLER[0]]), pose_of(stats[SRC_CONTROLLER[1]])],
            hmd: pose_of(stats[SRC_HMD]),
            battery: crate::misc::power::battery(),
            disk: disk.disk,
            production,
            offline_min: crate::hub::backend::offline_since_ns()
                .map(|ns| crate::misc::clock::realtime_ns().saturating_sub(ns) / 60_000_000_000),
            unbound: crate::hub::backend::unbound(),
            upload: crate::hub::upload::state(),
            episodes_on_disk: disk.episodes_on_disk,
            queued: crate::hub::upload::depth(),
            robot_id: &config.identity.robot_id,
            address: disk.address.as_deref(),
            episodes: &disk.episodes,
            calib: calibrating.then(|| panel::Calib { prompt, held, messages: &messages }),
            refused: &refused,
            qr: input.qr,
        };
        let patches = ui.draw(&state);
        if tx
            .send(Tick {
                patches,
                centre_shown,
                qr_shown: input.qr,
                gripper_rgba,
                feedback,
                speech,
            })
            .is_err()
        {
            return;
        }
        std::thread::sleep(UI_TICK);
    }
}

/// Matches the cameras to their slots without the operator: the head camera
/// by its picture size, the wrists by which controller each one follows.
///
/// A take needs the wrists matched (the gate's `cameras`). They are matched
/// again after launch and after both wrist cameras were down at once, because
/// then either can come back on either port. A rig keeps its cameras, so when
/// only one drops, the one that returns takes the free slot. Nothing moves
/// during a take, because that would swap topics mid-take.
///
/// A match is heard, so the operator who turned a gripper knows it worked.
/// Then the first wave of one hand alone names the camera that moved, so the
/// operator can hear it agree.
#[derive(Default)]
struct Check {
    wrists: pairing::Wrists,
    /// Since the match, while it still waits for that one wave.
    wave: Option<(Instant, pairing::Wave)>,
    roles: Option<Roles>,
    grey: [Option<Vec<u8>>; 2],
    poses: [Option<[f64; 7]>; 2],
    at: Option<Instant>,
}

impl Check {
    fn tick(
        &mut self,
        hub: &Hub,
        thumbs: &[Option<Vec<u8>>; 3],
        fresh: [bool; 3],
        stats: &[SourceStats],
    ) -> Option<Heard> {
        let live = [stats[SRC_CAM[0]].live, stats[SRC_CAM[1]].live];
        let wrists_live = live[0] && live[1];
        if live == [false, false] && hub.cameras_matched() {
            info!("cameras: both wrist cameras are down, so they need matching again");
            hub.set_cameras_matched(false);
        }
        let roles = hub.roles();
        if hub.recording() || self.roles != Some(roles) {
            *self = Check { roles: Some(roles), ..Check::default() };
        }
        if hub.recording() {
            return None;
        }
        let mut sizes = [None; 3];
        for slot in [0, 1, SLOT_HEAD] {
            let s = stats[SRC_CAM[slot]];
            if let (Some(rank), true) = (roles.rank(slot), s.live) {
                sizes[rank] = Some((s.width as u32, s.height as u32));
            }
        }
        let head = pairing::head_rank(sizes).filter(|&rank| roles.rank(SLOT_HEAD) != Some(rank));
        if let Some(rank) = head {
            info!("cameras: the head camera was in a wrist slot; moved by picture size");
            hub.set_cameras_matched(false);
            save_cameras(hub, roles.swapped(SLOT_HEAD, rank));
            return None;
        }
        // A stale rank list (`camera_roles`) can leave a wrist slot on a rank
        // nobody opened while the unjudged head slot holds a live camera.
        let wrist_down = live.iter().position(|&l| !l);
        if let (true, true, Some(slot), Some(rank)) =
            (hub.head_absent(), stats[SRC_CAM[SLOT_HEAD]].live, wrist_down, roles.rank(SLOT_HEAD))
        {
            info!(
                "cameras: no head camera is fitted, so the head slot's camera moves to slot {slot}"
            );
            hub.set_cameras_matched(false);
            save_cameras(hub, roles.swapped(slot, rank));
            return None;
        }
        if self.wave.as_ref().is_some_and(|(at, _)| at.elapsed() >= WAVE_WINDOW) {
            self.wave = None;
        }
        if (hub.cameras_matched() && self.wave.is_none()) || !wrists_live {
            return None;
        }
        let now = Instant::now();
        let secs = self.at.map(|t| now.duration_since(t).as_secs_f64()).unwrap_or(0.0);
        self.at = Some(now);
        let poses = hub.poses();
        let spin = std::array::from_fn(|side| {
            let (was, is) = (self.poses[side], poses[side]);
            self.poses[side] = is;
            pairing::spin(was?, is?, secs)
        });
        let change = std::array::from_fn(|slot| {
            let grey = thumbs[slot].as_deref().filter(|_| fresh[slot]).map(pairing::grey);
            let was = std::mem::replace(&mut self.grey[slot], grey.clone());
            pairing::change(&was?, &grey?)
        });
        if let Some((_, wave)) = &mut self.wave {
            let slot = wave.tick(change, spin)?;
            info!("cameras: one hand waved alone and slot {slot}'s picture moved more");
            self.wave = None;
            return Some(Heard::Moving(slot));
        }
        let heard = match self.wrists.tick(change, spin) {
            Some(pairing::Verdict::Matched) => {
                info!("cameras: the wrists follow their own grippers");
                hub.set_cameras_matched(true);
                Some(Heard::Assigned)
            }
            Some(pairing::Verdict::Crossed) => {
                info!("cameras: the wrists were crossed; swapped");
                let ranks = roles.ranks();
                let swapped = Roles::new([ranks[1], ranks[0], ranks[SLOT_HEAD]]);
                let saved = save_cameras(hub, swapped);
                hub.set_cameras_matched(true);
                // The swap is ours, so the wave survives the roles changing,
                // and each slot's last picture now belongs to the other camera.
                self.roles = Some(swapped);
                self.grey = [None, None];
                (saved == Feedback::Started).then_some(Heard::Assigned)
            }
            None => None,
        };
        if heard.is_some() {
            self.wave = Some((Instant::now(), pairing::Wave::default()));
        }
        heard
    }
}

enum Heard {
    Assigned,
    /// A wave after the match moved this wrist slot's picture.
    Moving(usize),
}

/// Make `roles` the rig's, and save it by the ports those cameras sit in so it
/// holds after a replug. A camera whose port is unknown saves its number.
fn save_cameras(hub: &Hub, roles: Roles) -> Feedback {
    hub.set_roles(roles);
    let mut ports = roles.ports(&hub.cam_ports());
    if hub.head_absent() {
        ports[SLOT_HEAD] = Some(String::new());
    }
    let patch = match ports.iter().all(Option::is_some) {
        true => {
            let ports =
                CAMERA_KEYS.iter().zip(&ports).map(|(k, p)| (k.to_string(), p.clone().into()));
            serde_json::json!({ "camera_ports": serde_json::Map::from_iter(ports) })
        }
        false => serde_json::json!({ "camera_roles": roles.ranks() }),
    };
    let saved = Config::edit(EPISODE_DIR, &patch).and_then(|w| w.map_err(|e| e.to_string()));
    match saved {
        Ok(()) => {
            info!("cameras assigned: {patch}");
            hub.adopt_cameras(&Config::load(EPISODE_DIR));
            Feedback::Started
        }
        Err(e) => {
            warn!("cameras not saved: {e}");
            Feedback::Refused
        }
    }
}

fn jaws(hub: &Hub, stats: &[SourceStats]) -> [panel::Jaw; 2] {
    let calibration = hub.calibration();
    let sides = hub.sides();
    std::array::from_fn(|side| {
        // A gripper that sends nothing cannot be calibrated, so that is what to say.
        if !stats[SRC_ENCODER[side]].live {
            return panel::Jaw::Stopped;
        }
        let Some(s) = calibration.side(side) else { return panel::Jaw::Uncalibrated };
        match hub.gripper_angle(side, sides[side]) {
            Some(angle) => {
                panel::Jaw::Open((angle / s.envelope().max(1e-6)).clamp(0.0, 1.0) as f32)
            }
            None => panel::Jaw::Uncalibrated,
        }
    })
}

fn pose_of(s: SourceStats) -> panel::Pose {
    panel::Pose { hz: s.hz, live: s.live, extrapolated: s.extrapolated }
}

fn cam_of(s: SourceStats) -> panel::Cam {
    panel::Cam {
        width: s.width as u32,
        height: s.height as u32,
        hz: s.hz,
        nominal: s.nominal,
        live: s.live,
        starved: s.starved(),
        bytes_flowing: s.bytes_mb_s > 0.0,
    }
}

pub fn local_ip() -> Option<String> {
    routed_ip().or_else(interface_ip)
}

fn routed_ip() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("8.8.8.8:80").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

/// A LAN with no route to the internet still serves :8080, so the address
/// comes from the interfaces then: wlan0's if it has one, else the first that
/// is up and neither loopback nor link-local.
fn interface_ip() -> Option<String> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `list`, and it is freed once below.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return None;
    }
    let mut found: Vec<(bool, std::net::Ipv4Addr)> = Vec::new();
    let mut at = list;
    while !at.is_null() {
        // SAFETY: every node and its name and address stay valid until freeifaddrs.
        let ifa = unsafe { &*at };
        at = ifa.ifa_next;
        let up = ifa.ifa_flags & libc::IFF_UP as u32 != 0;
        if !up || ifa.ifa_addr.is_null() {
            continue;
        }
        if unsafe { (*ifa.ifa_addr).sa_family } as i32 != libc::AF_INET {
            continue;
        }
        let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
        let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
        if ip.is_loopback() || ip.is_link_local() {
            continue;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) };
        found.push((name.to_bytes() == b"wlan0", ip));
    }
    unsafe { libc::freeifaddrs(list) };
    found.iter().find(|(wlan, _)| *wlan).or(found.first()).map(|(_, ip)| ip.to_string())
}
