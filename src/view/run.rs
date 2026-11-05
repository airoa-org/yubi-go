//! The render thread: one XR session, and every frame of it.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use log::{error, info, warn};
use openxr as xr;

use super::mesh;
use super::panel;
use super::xr::{controller_pose, has_pose, held, is_tracked, located, pressed, Xr};
use super::{gl, painter, place};
use crate::hub::{Feedback, Hub, CAM_STRIP, SLOT_HEAD};

/// Long enough that a glance at the QR stays silent.
const ADDRESS_HOLD: Duration = Duration::from_secs(1);

const CONTROLLER_RGBA: [f32; 4] = [0.38, 0.50, 0.92, 0.3];
const CONTROLLER_LOST_RGBA: [f32; 4] = [0.92, 0.24, 0.24, 0.3];

// Where the panel's quads hang, in metres from the head, set by eye in the
// headset.
const BAND_WIDTH: f32 = 0.52;
const STATUS_DROP: f32 = -0.2;
const HINTS_DROP: f32 = -0.25;
const BAND_DISTANCE: f32 = 0.9;
const BAND_DROP: f32 = -0.123;
const CAM_WIDTH: f32 = 0.28;
const CAM_GAP: f32 = 0.012;
const CAMS_DROP: f32 = 0.14;
const MENU_WIDTH: f32 = 0.62;
const MENU_DROP: f32 = -0.128;
const QR_WIDTH: f32 = 0.6;

/// A controller that turns less than 3° counts as lying still. This is the
/// cosine of the 1.5° half-angle, which a quaternion dot product gives
/// directly. 3° is chosen by hand: above a resting IMU's noise, and below any
/// motion a hand makes on purpose.
const RESTING_COS: f32 = 0.99966;
/// How long a controller must stay inside [`RESTING_COS`] before a dropout
/// reads as "set down" rather than "lost". Chosen by hand.
const RESTING_AFTER: Duration = Duration::from_millis(1500);

/// The finger meshes each side draws, by `mount::hand_links` index:
/// `right_finger_link` is 2 and `left_finger_link` is 4.
const FINGERS: [[(usize, &str); 2]; 2] = [
    [(2, "hand_left_right_finger_link.STL"), (4, "hand_left_left_finger_link.STL")],
    [(2, "hand_right_right_finger_link.STL"), (4, "hand_right_left_finger_link.STL")],
];
const CONTROLLER_SHELLS: [&str; 2] = ["left.stl", "right.stl"];

fn xr_from_ros(pos: glam::DVec3, rot: glam::DQuat) -> glam::Mat4 {
    let translation = glam::Vec3::new(-pos.y as f32, pos.z as f32, -pos.x as f32);
    let rotation = glam::Quat::from_xyzw(-rot.y as f32, rot.z as f32, -rot.x as f32, rot.w as f32);
    glam::Mat4::from_rotation_translation(rotation, translation)
}

fn xr_from_ros_link(pos: glam::DVec3, rot: glam::DQuat) -> glam::Mat4 {
    let translation = glam::Vec3::new(-pos.y as f32, pos.z as f32, -pos.x as f32);
    let r = crate::misc::mount::xr_to_ros().inverse() * rot;
    let rotation = glam::Quat::from_xyzw(r.x as f32, r.y as f32, r.z as f32, r.w as f32);
    glam::Mat4::from_rotation_translation(rotation, translation)
}

/// The panel texture as the panel thread last drew it, rows bottom-up for GL,
/// and what each swapchain image still lacks of it: OpenXR hands the images
/// out in turn, so a patch has to reach every one of them.
struct Texture {
    mirror: Vec<u8>,
    stale: Vec<Vec<reactive_svg::Rect>>,
}

impl Texture {
    fn new(images: usize) -> Self {
        let full = reactive_svg::Rect { x: 0, y: 0, w: panel::W, h: panel::TEX_H };
        Self { mirror: vec![0; panel::W * panel::TEX_H * 4], stale: vec![vec![full]; images] }
    }

    fn paste(&mut self, patches: &[reactive_svg::Patch]) {
        for p in patches {
            let r = p.rect;
            for (row, src) in p.rgba.chunks_exact(r.w * 4).enumerate() {
                let y = panel::TEX_H - 1 - (r.y + row);
                self.mirror[(y * panel::W + r.x) * 4..][..r.w * 4].copy_from_slice(src);
            }
            self.stale.iter_mut().for_each(|s| s.push(r));
        }
        // While nothing renders, nothing uploads, so the rects would pile up.
        let full = reactive_svg::Rect { x: 0, y: 0, w: panel::W, h: panel::TEX_H };
        for stale in &mut self.stale {
            if stale.iter().map(|r| r.w * r.h).sum::<usize>() >= full.w * full.h {
                *stale = vec![full];
            }
        }
    }

    /// Nothing is acquired while every image is current: the quads keep
    /// showing the last one released.
    fn upload(&mut self, x: &mut Xr) -> anyhow::Result<()> {
        if self.stale.iter().all(Vec::is_empty) {
            return Ok(());
        }
        let index = x.ui_swapchain.acquire_image().context("ui acquire")? as usize;
        x.ui_swapchain.wait_image(xr::Duration::INFINITE).context("ui wait")?;
        for r in self.stale[index].drain(..) {
            let gl_y = panel::TEX_H - r.y - r.h;
            gl::upload_rect(x.ui_images[index], panel::W, &self.mirror, (r.x, gl_y, r.w, r.h));
        }
        x.ui_swapchain.release_image().context("ui release")?;
        Ok(())
    }
}

/// Tells a gripper set down on the table from one the headset has lost. A
/// set-down controller reports no pose at all: on a Quest 3S (2026-08-27) all
/// four `*_VALID`/`*_TRACKED` bits clear a few seconds after it stops moving.
/// So the last valid sample is the only evidence, and a device that was lying
/// still then is still lying there.
#[derive(Clone, Copy)]
struct Stillness {
    at: [f32; 4],
    since: Instant,
    resting: bool,
}

impl Stillness {
    fn new() -> Self {
        Self { at: [0.0, 0.0, 0.0, 1.0], since: Instant::now(), resting: false }
    }

    fn saw(&mut self, q: xr::Quaternionf) {
        let now = [q.x, q.y, q.z, q.w];
        let held = self.at.iter().zip(now).map(|(a, b)| a * b).sum::<f32>().abs();
        if held < RESTING_COS {
            if self.since.elapsed() < RESTING_AFTER {
                self.resting = false;
            }
            self.at = now;
            self.since = Instant::now();
        } else if self.since.elapsed() >= RESTING_AFTER {
            self.resting = true;
        }
    }
}

pub fn start(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void, hub: &'static Hub) {
    static ONCE: std::sync::Once = std::sync::Once::new();
    let mut started = false;
    ONCE.call_once(|| started = true);
    if !started {
        return;
    }
    super::xr::adopt_android_context(vm, activity);
    std::thread::spawn(move || {
        let mut backoff = Duration::from_secs(1);
        let mut had_session = false;
        loop {
            wait_until_foreground();
            match run(hub, &mut had_session) {
                Ok(()) => exit_without_a_session(hub, "the runtime ended the session"),
                Err(e) if had_session => {
                    error!("openxr: {e:#}");
                    exit_without_a_session(hub, "the session is gone and cannot be rebuilt here");
                }
                Err(e) => {
                    warn!("openxr: {e:#} — no session yet, retrying in {:.0?}", backoff);
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(30));
                }
            }
        }
    });
}

fn exit_without_a_session(hub: &Hub, why: &str) -> ! {
    error!("openxr: {why} — exiting so the next launch gets a fresh process");
    hub.stop_and_wait();
    std::process::exit(0);
}

/// A tick's feedback and the speech after it, waiting for a frame to play in.
type Cue = (Option<Feedback>, Vec<&'static str>);

/// What the panel thread last sent, held until a frame shows it.
struct Panel {
    texture: Texture,
    centre_shown: bool,
    qr_shown: bool,
    gripper_rgba: [[f32; 4]; 2],
    cues: Vec<Cue>,
}

impl Panel {
    fn new(images: usize) -> Self {
        Self {
            texture: Texture::new(images),
            centre_shown: false,
            qr_shown: false,
            gripper_rgba: [[0.0; 4]; 2],
            cues: Vec::new(),
        }
    }

    /// Every tick since the last call, so the channel stays empty whether or
    /// not a frame is rendered. Patches are deltas, so each one is pasted; only
    /// the cues wait for a frame, and while none renders only the newest is kept.
    fn drain(&mut self, ticks: &std::sync::mpsc::Receiver<painter::Tick>, rendering: bool) {
        for tick in ticks.try_iter() {
            self.texture.paste(&tick.patches);
            self.centre_shown = tick.centre_shown;
            self.qr_shown = tick.qr_shown;
            self.gripper_rgba = tick.gripper_rgba;
            if tick.feedback.is_some() || !tick.speech.is_empty() {
                self.cues.push((tick.feedback, tick.speech));
            }
        }
        if !rendering && self.cues.len() > 1 {
            self.cues.drain(..self.cues.len() - 1);
        }
    }

    fn play_cues(&mut self, x: &Xr, hub: &Hub) {
        for (feedback, speech) in self.cues.drain(..) {
            match feedback {
                Some(what) if speech.is_empty() || super::voice::spoken(what, None).is_some() => {
                    tell(x, hub, what)
                }
                Some(what) => {
                    buzz(x, what);
                    super::sound::say(Some(what), speech);
                }
                None if !speech.is_empty() => super::sound::say(None, speech),
                None => {}
            }
        }
    }
}

fn tell(x: &Xr, hub: &Hub, what: Feedback) {
    buzz(x, what);
    let named = matches!(
        what,
        Feedback::Fault | Feedback::Cancelled | Feedback::MarkedFailed | Feedback::EndedFailed
    );
    let reason = if named { hub.take_last_fault() } else { None };
    match super::voice::spoken(what, reason) {
        Some(ids) => super::sound::say(named.then_some(what), ids),
        None => match step_name(hub, what) {
            Some(pcm) => super::sound::say_name(what, pcm),
            None => super::sound::play(what),
        },
    }
}

/// After the start or advance tone, the step the hands are now on, once
/// `misc::speech` has made it.
fn step_name(hub: &Hub, what: Feedback) -> Option<Vec<i16>> {
    let view = hub.task_view();
    if !matches!(what, Feedback::Started | Feedback::Advanced) || !view.config.announce_steps {
        return None;
    }
    let text = view.step_text()?;
    let samples = crate::misc::speech::samples(crate::hub::EPISODE_DIR, &text)?;
    Some(super::voice::upsampled(&samples))
}

/// The task a take would record, said after "Ready to record. Task is", once
/// `misc::speech` has made it.
fn task_name(hub: &Hub) -> Option<Vec<i16>> {
    let view = hub.task_view();
    let text = view.config.has_task().then(|| view.config.task())?;
    let samples = crate::misc::speech::samples(crate::hub::EPISODE_DIR, text)?;
    Some(super::voice::upsampled(&samples))
}

/// A refused start, by A or by a double-click: the buzz, then why.
fn refused(x: &Xr, hub: &Hub) {
    buzz(x, Feedback::Refused);
    let first = hub.refused_first();
    super::sound::say(None, super::voice::not_ready(first.as_ref()));
}

fn buzz(x: &Xr, what: Feedback) {
    let (ms, amplitude, hz) = match what {
        Feedback::Started => (60, 1.0, 180.0),
        Feedback::Stopped => (140, 0.6, 110.0),
        Feedback::Advanced | Feedback::Assigned => (50, 0.45, 180.0),
        Feedback::MarkedFailed | Feedback::EndedFailed => (260, 0.9, 70.0),
        Feedback::Refused => (450, 1.0, 40.0),
        Feedback::Fault => (200, 1.0, 320.0),
        Feedback::Cancelled => (600, 1.0, 320.0),
        Feedback::RunningOut => (350, 0.7, 90.0),
        Feedback::TimeWarning => (200, 0.6, 140.0),
    };
    let event = xr::HapticVibration::new()
        .amplitude(amplitude)
        .duration(xr::Duration::from_nanos(ms * 1_000_000))
        .frequency(hz);
    if let Err(e) = x.actions.feedback.apply_feedback(&x.session, xr::Path::NULL, &event) {
        warn!("haptic: {e}");
    }
}

static FOREGROUND: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_foreground(on: bool) {
    FOREGROUND.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn is_foreground() -> bool {
    FOREGROUND.load(std::sync::atomic::Ordering::Relaxed)
}

fn wait_until_foreground() {
    if is_foreground() {
        return;
    }
    info!("openxr: backgrounded — waiting to be in front again");
    while !is_foreground() {
        std::thread::sleep(Duration::from_millis(250));
    }
    info!("openxr: foreground again");
}

/// What the buttons are in the middle of, across frames.
#[derive(Default)]
struct Holds {
    /// Either trigger is down. The QR fills the view from the panel tick
    /// that drew it, so the quad never shows what the cells left there.
    qr_shown: bool,
    /// When a trigger went down to show the QR, and whether that hold already said the address.
    qr_down: Option<(Instant, bool)>,
}

/// Reads the controllers and acts on every gesture.
fn handle_input(x: &Xr, hub: &Hub, holds: &mut Holds) -> anyhow::Result<()> {
    x.session.sync_actions(&[(&x.actions.set).into()]).context("sync_actions")?;
    let xr_offset = super::xr::xr_to_boottime(&x.instance);
    x.actions.inputs.publish(&x.session, hub, xr_offset);
    if pressed(&x.session, &x.actions.rec_toggle) {
        let on = !hub.recording();
        if hub.set_recording(on) {
            info!("recording {}", if on { "ON" } else { "OFF" });
        } else if on && hub.ready().is_err() {
            refused(x, hub);
        }
    }
    if pressed(&x.session, &x.actions.calibrate) {
        hub.toggle_calibration();
    }
    holds.qr_shown = held(&x.session, &x.actions.qr_show);
    match (holds.qr_shown, holds.qr_down) {
        (true, None) => holds.qr_down = Some((Instant::now(), false)),
        (true, Some((at, false))) if at.elapsed() >= ADDRESS_HOLD => {
            holds.qr_down = Some((at, true));
            let address = super::painter::local_ip().map(|ip| format!("{ip}:8080"));
            super::sound::say(None, super::voice::address(address.as_deref()));
        }
        (false, Some(_)) => holds.qr_down = None,
        _ => {}
    }
    if pressed(&x.session, &x.actions.mark_failed) {
        if hub.recording() {
            hub.mark_failed();
        } else {
            let battery = crate::misc::power::battery();
            super::sound::say(None, super::voice::battery(battery));
        }
    }
    if pressed(&x.session, &x.actions.readiness) {
        say_readiness(hub);
    }
    match hub.poll_trigger() {
        Some(Feedback::Refused) => refused(x, hub),
        Some(what) => tell(x, hub, what),
        None => {}
    }
    Ok(())
}

/// A press of the settings button: whether the rig is ready, or how the take
/// is going.
fn say_readiness(hub: &Hub) {
    let first = hub.readiness();
    let ids = match (hub.recording(), first) {
        (true, first) => super::voice::during_take(first.as_ref()),
        (false, None) => {
            match task_name(hub) {
                Some(task) => {
                    super::sound::say_then(vec![super::voice::READY, super::voice::TASK_IS], task)
                }
                None => super::sound::say(None, vec![super::voice::READY]),
            }
            Vec::new()
        }
        (false, Some(reason)) => super::voice::not_ready(Some(&reason)),
    };
    if !ids.is_empty() {
        super::sound::say(None, ids);
    }
}

/// Without the pose sampler, each frame's poses are the recording's.
fn publish_poses(x: &Xr, hub: &Hub, time: xr::Time) {
    if x.sampling {
        return;
    }
    for (part, space) in x.tracked.iter().enumerate() {
        let capture = crate::misc::capture::Capture::default();
        let location = space.relate(&x.stage, time);
        hub.publish_pose(capture, part, located(location));
    }
}

/// Each controller's last pose that had one in it, held across dropouts.
struct Hands {
    poses: [Option<[f64; 7]>; 2],
    stillness: [Stillness; 2],
}

impl Hands {
    fn new() -> Self {
        Self { poses: [None, None], stillness: [Stillness::new(); 2] }
    }

    /// Which controllers to draw as lost this frame.
    fn track(&mut self, x: &Xr, time: xr::Time) -> [bool; 2] {
        let mut lost = [false; 2];
        for (part, space) in x.tracked.iter().enumerate().take(2) {
            let Ok(location) = space.locate(&x.stage, time) else {
                continue;
            };
            let flags = location.location_flags;
            match has_pose(flags) {
                true => {
                    self.poses[part] = Some(controller_pose(location.pose));
                    self.stillness[part].saw(location.pose.orientation);
                    lost[part] = !is_tracked(flags);
                }
                false => lost[part] = !self.stillness[part].resting,
            }
        }
        lost
    }
}

/// The meshes one frame draws, in the stage frame.
struct Models {
    /// Each side's two fingers, at the gripper's current opening.
    fingers: [Vec<(&'static str, glam::Mat4)>; 2],
    /// `(side, pose)` of each controller shell.
    controllers: Vec<(usize, glam::Mat4)>,
}

fn models(hub: &Hub, poses: &[Option<[f64; 7]>; 2]) -> Models {
    let sides = hub.sides();
    let mut fingers: [Vec<(&'static str, glam::Mat4)>; 2] = [Vec::new(), Vec::new()];
    let mut controllers = Vec::new();
    for (side, pose) in poses.iter().enumerate() {
        let Some(t) = pose else { continue };
        let p = glam::DVec3::new(t[0], t[1], t[2]);
        let q = glam::DQuat::from_xyzw(t[3], t[4], t[5], t[6]);
        controllers.push((side, xr_from_ros(p, q)));
        let angle = hub.gripper_angle(side, sides[side]).unwrap_or(0.0);
        let links = crate::misc::mount::hand_links(side, (p, q), angle);
        for (link, mesh) in FINGERS[side] {
            fingers[side].push((mesh, xr_from_ros_link(links[link].pos, links[link].rot)));
        }
    }
    Models { fingers, controllers }
}

/// Where each quad hangs. A pose is kept from the last frame that could place
/// it, because `place::band` has no answer while the head looks straight up
/// or down.
#[derive(Default)]
struct Layout {
    band: Option<xr::Posef>,
    /// Storage, ID, network, then the hints.
    subs: [Option<xr::Posef>; 4],
    cams: [Option<xr::Posef>; 3],
    menu: Option<xr::Posef>,
    qr: Option<xr::Posef>,
}

impl Layout {
    /// Without a head camera fitted, its cell is not hung.
    fn place(&mut self, eye_views: &[xr::View], head_camera: bool) {
        let head = {
            let p = |v: usize| {
                let q = eye_views[v].pose.position;
                glam::Vec3::new(q.x, q.y, q.z)
            };
            (p(0) + p(1)) / 2.0
        };
        let facing = eye_views[0].pose.orientation;
        let facing = glam::Quat::from_xyzw(facing.x, facing.y, facing.z, facing.w);
        let hung = |drop, offset: f32| {
            place::band(head, facing, BAND_DISTANCE, drop).map(|(pos, rot)| {
                let pos = pos + rot * glam::Vec3::X * offset;
                xr::Posef {
                    orientation: xr::Quaternionf { x: rot.x, y: rot.y, z: rot.z, w: rot.w },
                    position: xr::Vector3f { x: pos.x, y: pos.y, z: pos.z },
                }
            })
        };
        if let Some(pose) = hung(BAND_DROP, 0.0) {
            self.band = Some(pose);
        }
        let under = |rect| place::under_band(rect, BAND_WIDTH).0;
        let at = [
            (STATUS_DROP, under(panel::PANEL_STORAGE)),
            (STATUS_DROP, under(panel::PANEL_ID)),
            (STATUS_DROP, under(panel::PANEL_NET)),
            (HINTS_DROP, 0.0),
        ];
        for (i, (drop, offset)) in at.into_iter().enumerate() {
            if let Some(pose) = hung(drop, offset) {
                self.subs[i] = Some(pose);
            }
        }
        let strip: Vec<usize> =
            CAM_STRIP.into_iter().filter(|&cell| head_camera || cell != SLOT_HEAD).collect();
        if !head_camera {
            self.cams[SLOT_HEAD] = None;
        }
        for (index, cell) in strip.iter().enumerate() {
            let across = index as f32 - (strip.len() as f32 - 1.0) / 2.0;
            if let Some(pose) = hung(CAMS_DROP, across * (CAM_WIDTH + CAM_GAP)) {
                self.cams[*cell] = Some(pose);
            }
        }
        if let Some(pose) = hung(MENU_DROP, 0.0) {
            self.menu = Some(pose);
        }
        // Locked to the headset in all three axes rather than hung
        // upright like the panel, so a headset lying on a table still
        // shows the code square in its lenses for a phone to scan.
        self.qr = {
            let pos = head + facing * glam::Vec3::NEG_Z * BAND_DISTANCE;
            Some(xr::Posef {
                orientation: eye_views[0].pose.orientation,
                position: xr::Vector3f { x: pos.x, y: pos.y, z: pos.z },
            })
        };
    }
}

/// The jaws and controller shells, or `None` when GL cannot build them: the
/// panel works without.
fn gripper_renderer() -> Option<mesh::Renderer> {
    let mut meshes = Vec::new();
    for (name, bytes) in
        crate::misc::mount::MESHES.iter().chain(crate::misc::mount::CONTROLLER_MESHES.iter())
    {
        match mesh::parse_stl(bytes) {
            Some(m) => meshes.push((*name, m)),
            None => warn!("{name} is not a binary STL"),
        }
    }
    match mesh::Renderer::new(&meshes) {
        Ok(r) => {
            info!(
                "gripper renderer ready ({} triangles)",
                meshes.iter().map(|(_, m)| m.triangles()).sum::<usize>()
            );
            Some(r)
        }
        Err(e) => {
            warn!("gripper render unavailable: {e}");
            None
        }
    }
}

/// Draws the grippers into both eyes' swapchains, and returns each eye's
/// view for the projection layer.
fn draw_eyes(
    x: &mut Xr,
    renderer: Option<&mesh::Renderer>,
    eye_views: &[xr::View],
    models: &Models,
    gripper_rgba: [[f32; 4]; 2],
    lost: [bool; 2],
) -> anyhow::Result<Vec<(xr::Posef, xr::Fovf)>> {
    let (w, h) = x.eye_size;
    let mut projection_views = Vec::new();
    for (eye, (swapchain, fbos)) in x.swapchains.iter_mut().enumerate() {
        let index = swapchain.acquire_image().context("acquire")? as usize;
        swapchain.wait_image(xr::Duration::INFINITE).context("wait_image")?;
        let view = eye_views[eye];
        gl::bind_and_clear(fbos[index]);
        if let Some(renderer) = renderer {
            let o = view.pose.orientation;
            let p = view.pose.position;
            let eye_pose = glam::Mat4::from_rotation_translation(
                glam::Quat::from_xyzw(o.x, o.y, o.z, o.w),
                glam::Vec3::new(p.x, p.y, p.z),
            );
            let fov = view.fov;
            let view_proj = mesh::projection(
                [fov.angle_left, fov.angle_right, fov.angle_up, fov.angle_down],
                0.02,
                50.0,
            ) * eye_pose.inverse();
            for (side, side_items) in models.fingers.iter().enumerate() {
                renderer.draw(w, h, view_proj, side_items, gripper_rgba[side]);
            }
            for &(side, m) in &models.controllers {
                let rgba = match lost[side] {
                    false => CONTROLLER_RGBA,
                    true => CONTROLLER_LOST_RGBA,
                };
                renderer.draw(w, h, view_proj, &[(CONTROLLER_SHELLS[side], m)], rgba);
            }
        }
        gl::unbind();
        swapchain.release_image().context("release")?;
        projection_views.push((view.pose, view.fov));
    }
    Ok(projection_views)
}

/// Ends the frame with passthrough, then the grippers and the panel's quads.
/// While the QR shows it covers the rest; while the centre menu shows, the
/// band and status quads give way to it.
fn end_frame(
    x: &Xr,
    frame_stream: &mut xr::FrameStream<xr::OpenGlEs>,
    time: xr::Time,
    projection_views: &[(xr::Posef, xr::Fovf)],
    layout: &Layout,
    qr_shown: bool,
    centre_shown: bool,
) -> xr::Result<()> {
    let (w, h) = x.eye_size;
    let eye_rect = xr::Rect2Di {
        offset: xr::Offset2Di { x: 0, y: 0 },
        extent: xr::Extent2Di { width: w as i32, height: h as i32 },
    };
    let proj_views: Vec<xr::CompositionLayerProjectionView<xr::OpenGlEs>> = projection_views
        .iter()
        .enumerate()
        .map(|(eye, (pose, fov))| {
            xr::CompositionLayerProjectionView::new().pose(*pose).fov(*fov).sub_image(
                xr::SwapchainSubImage::new().swapchain(&x.swapchains[eye].0).image_rect(eye_rect),
            )
        })
        .collect();
    let projection = xr::CompositionLayerProjection::new()
        .layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA)
        .space(&x.stage)
        .views(&proj_views);

    let flipped_y = |top: usize, height: usize| (panel::TEX_H - top - height) as i32;
    let quad = |pose: xr::Posef, width: f32, rect: (usize, usize, usize, usize)| {
        let (rx, ry, rw, rh) = rect;
        xr::CompositionLayerQuad::<xr::OpenGlEs>::new()
            .layer_flags(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA)
            .space(&x.stage)
            .pose(pose)
            .size(xr::Extent2Df { width, height: width * rh as f32 / rw as f32 })
            .sub_image(xr::SwapchainSubImage::new().swapchain(&x.ui_swapchain).image_rect(
                xr::Rect2Di {
                    offset: xr::Offset2Di { x: rx as i32, y: flipped_y(ry, rh) },
                    extent: xr::Extent2Di { width: rw as i32, height: rh as i32 },
                },
            ))
    };

    let cams: Vec<_> = CAM_STRIP
        .iter()
        .filter_map(|&cell| layout.cams[cell].map(|pose| (cell, pose)))
        .map(|(cell, pose)| {
            let (cx, cy) = panel::CELL_ORIGIN[cell];
            quad(pose, CAM_WIDTH, (cx, cy, panel::CELL_W, panel::CELL_H))
        })
        .collect();
    let band = layout.band.map(|pose| quad(pose, BAND_WIDTH, panel::PANEL_TASK));
    let subs: Vec<_> = [
        (
            layout.subs[0],
            panel::PANEL_STORAGE,
            place::under_band(panel::PANEL_STORAGE, BAND_WIDTH).1,
        ),
        (layout.subs[1], panel::PANEL_ID, place::under_band(panel::PANEL_ID, BAND_WIDTH).1),
        (layout.subs[2], panel::PANEL_NET, place::under_band(panel::PANEL_NET, BAND_WIDTH).1),
        (layout.subs[3], panel::PANEL_HINTS, BAND_WIDTH * 0.8),
    ]
    .into_iter()
    .filter_map(|(pose, rect, width)| pose.map(|pose| quad(pose, width, rect)))
    .collect();
    let menu = layout
        .menu
        .filter(|_| centre_shown)
        .map(|pose| quad(pose, MENU_WIDTH, (0, panel::CENTRE_Y, panel::W, panel::CENTRE_H)));
    let qr = layout.qr.filter(|_| qr_shown).map(|pose| quad(pose, QR_WIDTH, panel::PANEL_QR));

    let mut layers: Vec<&xr::CompositionLayerBase<xr::OpenGlEs>> = Vec::new();
    if let Some(base) = x.passthrough_base() {
        layers.push(base);
    }
    if !projection_views.is_empty() {
        layers.push(&projection);
        match qr.as_ref() {
            Some(qr) => layers.push(qr),
            None => {
                layers.extend(cams.iter().map(|p| &**p));
                match menu.as_ref() {
                    Some(menu) => layers.push(menu),
                    None => {
                        if let Some(band) = band.as_ref() {
                            layers.push(band);
                        }
                        layers.extend(subs.iter().map(|p| &**p));
                    }
                }
            }
        }
    }
    frame_stream.end(time, xr::EnvironmentBlendMode::OPAQUE, &layers)
}

fn run(hub: &'static Hub, had_session: &mut bool) -> anyhow::Result<()> {
    let (mut x, mut frame_wait, mut frame_stream) =
        Xr::start(hub, (panel::W as u32, panel::TEX_H as u32), had_session)?;
    super::sound::warm();
    let renderer = gripper_renderer();
    let (input, ticks) = painter::spawn(hub);
    let mut shown = Panel::new(x.ui_images.len());
    let mut holds = Holds::default();
    let mut hands = Hands::new();
    let mut layout = Layout::default();
    let mut running = true;
    let mut logged = false;

    loop {
        if !x.poll(&mut running)? {
            return Ok(());
        }
        if !running {
            shown.drain(&ticks, false);
            std::thread::sleep(Duration::from_millis(100));
            continue;
        }

        let state = match frame_wait.wait() {
            Ok(state) => state,
            Err(e) if e == xr::sys::Result::ERROR_SESSION_NOT_RUNNING => {
                running = false;
                continue;
            }
            Err(e) => return Err(anyhow::Error::new(e).context("frame wait")),
        };
        match frame_stream.begin() {
            Ok(()) => {}
            Err(e) if e == xr::sys::Result::ERROR_SESSION_NOT_RUNNING => {
                running = false;
                continue;
            }
            Err(e) => return Err(anyhow::Error::new(e).context("frame begin")),
        }

        let time = state.predicted_display_time;
        let mut projection_views = Vec::new();
        if state.should_render {
            handle_input(&x, hub, &mut holds)?;
            input.set(painter::Input { qr: holds.qr_shown });
            shown.drain(&ticks, true);
            shown.play_cues(&x, hub);
            shown.texture.upload(&mut x)?;

            publish_poses(&x, hub, time);
            let lost = hands.track(&x, time);
            let (_, eye_views) = x
                .session
                .locate_views(xr::ViewConfigurationType::PRIMARY_STEREO, time, &x.stage)
                .context("locate_views")?;
            let models = models(hub, &hands.poses);
            layout.place(&eye_views, !hub.head_absent());
            projection_views = draw_eyes(
                &mut x,
                renderer.as_ref(),
                &eye_views,
                &models,
                shown.gripper_rgba,
                lost,
            )?;
            if !logged {
                logged = true;
                info!("openxr rendering + poses streaming");
            }
        } else {
            shown.drain(&ticks, false);
        }

        let ended = end_frame(
            &x,
            &mut frame_stream,
            time,
            &projection_views,
            &layout,
            shown.qr_shown,
            shown.centre_shown,
        );
        match ended {
            Ok(()) => {}
            Err(e) if e == xr::sys::Result::ERROR_SESSION_NOT_RUNNING => running = false,
            Err(e) => return Err(anyhow::Error::new(e).context("frame end")),
        }
    }
}
