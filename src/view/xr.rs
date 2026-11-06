use anyhow::Context as _;
use log::{info, warn};
use openxr as xr;
use openxr::sys::Handle as _;

use super::gl::{self, Egl};
use crate::hub::{Hub, InputChange};
use crate::misc::capture::Capture;
use crate::misc::clock::boottime_ns;
use crate::misc::pose::{components, ComponentTracking, Located, Velocity};
use crate::misc::status::{self, Status};

/// Poses per second from the sampler thread. Chosen by hand: above ~120 Hz
/// `xrLocateSpace` interpolates rather than reporting new samples.
const POSE_HZ: u64 = 120;

pub struct Actions {
    pub set: xr::ActionSet,
    pub rec_toggle: xr::Action<bool>,
    pub mark_failed: xr::Action<bool>,
    pub calibrate: xr::Action<bool>,
    pub qr_show: xr::Action<bool>,
    pub readiness: xr::Action<bool>,
    pub feedback: xr::Action<xr::Haptic>,
    pub inputs: Inputs,
}

/// Every input the Touch controller offers an app, per hand, recorded as
/// `sensor_msgs/Joy` in `mcap::JOY_AXES` / `JOY_BUTTONS` order. These bind
/// alongside the gesture actions above; OpenXR lets two actions share a path.
pub struct Inputs {
    hands: [xr::Path; 2],
    trigger: xr::Action<f32>,
    squeeze: xr::Action<f32>,
    thumbstick: xr::Action<xr::Vector2f>,
    buttons: Vec<xr::Action<bool>>,
}

/// `(action, left path, right path)` per Joy button. Only the left controller
/// has a menu button; the right one's is the system button, which no app sees.
const BUTTONS: [(&str, &str, Option<&str>); 9] = [
    ("lower_click", "x/click", Some("a/click")),
    ("upper_click", "y/click", Some("b/click")),
    ("thumbstick_click", "thumbstick/click", Some("thumbstick/click")),
    ("menu_click", "menu/click", None),
    ("lower_touch", "x/touch", Some("a/touch")),
    ("upper_touch", "y/touch", Some("b/touch")),
    ("trigger_touch", "trigger/touch", Some("trigger/touch")),
    ("thumbstick_touch", "thumbstick/touch", Some("thumbstick/touch")),
    ("thumbrest_touch", "thumbrest/touch", Some("thumbrest/touch")),
];

impl Inputs {
    /// One Joy per controller the runtime reports active; an asleep or absent
    /// controller writes nothing rather than zeros. Every input that changed
    /// since the last sync also goes out with its own change time, converted
    /// to boottime by `xr_offset` (see [`xr_to_boottime`]).
    pub fn publish(&self, session: &xr::Session<xr::OpenGlEs>, hub: &Hub, xr_offset: Option<i64>) {
        for (side, &hand) in self.hands.iter().enumerate() {
            let Ok(trigger) = self.trigger.state(session, hand) else { continue };
            if !trigger.is_active {
                continue;
            }
            let squeeze = self.squeeze.state(session, hand).ok();
            let stick = self.thumbstick.state(session, hand).ok();
            let buttons: Vec<_> =
                self.buttons.iter().map(|a| a.state(session, hand).ok()).collect();

            let mut changes = Vec::new();
            if let Some(offset) = xr_offset {
                let mut changed = |name: &'static str, value: f32, at: xr::Time| {
                    let at_ns = (at.as_nanos() + offset).max(0) as u64;
                    changes.push(InputChange { name, value, at_ns });
                };
                let mut analog = |name, s: Option<xr::ActionState<f32>>| {
                    if let Some(s) = s.filter(|s| s.changed_since_last_sync) {
                        changed(name, s.current_state, s.last_change_time);
                    }
                };
                analog("trigger", Some(trigger));
                analog("squeeze", squeeze);
                if let Some(s) = stick.filter(|s| s.changed_since_last_sync) {
                    changed("thumbstick_x", s.current_state.x, s.last_change_time);
                    changed("thumbstick_y", s.current_state.y, s.last_change_time);
                }
                for ((name, _, _), s) in BUTTONS.iter().zip(&buttons) {
                    if let Some(s) = s.filter(|s| s.changed_since_last_sync) {
                        changed(name, s.current_state as u8 as f32, s.last_change_time);
                    }
                }
            }

            let pressed = |i: usize| buttons[i].is_some_and(|s| s.current_state) as i32;
            let stick = stick.map_or(xr::Vector2f::default(), |s| s.current_state);
            let squeeze = squeeze.map_or(0.0, |s| s.current_state);
            let axes = [trigger.current_state, squeeze, stick.x, stick.y];
            hub.publish_joy(side, axes, std::array::from_fn(pressed));
            if !changes.is_empty() {
                hub.publish_input_changes(side, changes);
            }
        }
    }
}

/// Boottime minus XrTime, read back to back: add it to an XrTime to put it
/// on boottime. XrTime does not advance while the headset sleeps and boottime
/// does, so the offset is read fresh rather than once. `None` without
/// KHR_convert_timespec_time, which `Instance::now` needs.
pub fn xr_to_boottime(instance: &xr::Instance) -> Option<i64> {
    instance.exts().khr_convert_timespec_time.as_ref()?;
    let boottime = boottime_ns() as i64;
    Some(boottime - instance.now().ok()?.as_nanos())
}

pub struct Xr {
    pub instance: xr::Instance,
    pub session: xr::Session<xr::OpenGlEs>,
    pub stage: xr::Space,
    pub actions: Actions,
    pub tracked: Vec<xr::Space>,
    pub swapchains: Vec<(xr::Swapchain<xr::OpenGlEs>, Vec<u32>)>,
    pub ui_swapchain: xr::Swapchain<xr::OpenGlEs>,
    pub ui_images: Vec<u32>,
    pub eye_size: (u32, u32),
    pub sampling: bool,
    /// Owns the layer `pass_raw` names: dropping it destroys the layer.
    _passthrough: Option<(xr::Passthrough, xr::PassthroughLayerFB)>,
    pass_raw: Option<xr::sys::CompositionLayerPassthroughFB>,
    events: xr::EventDataBuffer,
    hub: &'static Hub,
}

pub fn is_tracked(flags: xr::SpaceLocationFlags) -> bool {
    flags.contains(
        xr::SpaceLocationFlags::POSITION_TRACKED | xr::SpaceLocationFlags::ORIENTATION_TRACKED,
    )
}

pub fn has_pose(flags: xr::SpaceLocationFlags) -> bool {
    flags.contains(
        xr::SpaceLocationFlags::POSITION_VALID | xr::SpaceLocationFlags::ORIENTATION_VALID,
    )
}

/// The stage-frame vector in the ROS world frame, as [`ros_pose`] maps a
/// position. The map is a proper rotation, so angular velocity maps the same.
fn ros_vector(v: xr::Vector3f) -> [f64; 3] {
    [(-v.z) as f64, (-v.x) as f64, v.y as f64]
}

/// What one `relate` answer says, in the recording's terms.
pub fn located(location: xr::Result<(xr::SpaceLocation, xr::SpaceVelocity)>) -> Located {
    let Ok((l, v)) = location else { return Located::LOST };
    fn component_tracking<T>(
        flags: xr::SpaceLocationFlags,
        valid: xr::SpaceLocationFlags,
        tracked: xr::SpaceLocationFlags,
        value: T,
    ) -> ComponentTracking<T> {
        match (flags.contains(valid), flags.contains(tracked)) {
            (false, _) => ComponentTracking::Lost,
            (true, true) => ComponentTracking::Tracked(value),
            (true, false) => ComponentTracking::Inferred(value),
        }
    }
    let (position, orientation) = components(controller_pose(l.pose));
    let flags = l.location_flags;
    let valid = |flag| v.velocity_flags.contains(flag);
    let velocity = Velocity {
        linear: valid(xr::SpaceVelocityFlags::LINEAR_VALID).then(|| ros_vector(v.linear_velocity)),
        angular: valid(xr::SpaceVelocityFlags::ANGULAR_VALID)
            .then(|| ros_vector(v.angular_velocity)),
    };
    let located = Located::new(
        component_tracking(
            flags,
            xr::SpaceLocationFlags::POSITION_VALID,
            xr::SpaceLocationFlags::POSITION_TRACKED,
            position,
        ),
        component_tracking(
            flags,
            xr::SpaceLocationFlags::ORIENTATION_VALID,
            xr::SpaceLocationFlags::ORIENTATION_TRACKED,
            orientation,
        ),
    );
    Located { velocity, ..located }
}

pub fn pressed(session: &xr::Session<xr::OpenGlEs>, action: &xr::Action<bool>) -> bool {
    action
        .state(session, xr::Path::NULL)
        .is_ok_and(|s| s.changed_since_last_sync && s.current_state)
}

pub fn held(session: &xr::Session<xr::OpenGlEs>, action: &xr::Action<bool>) -> bool {
    action.state(session, xr::Path::NULL).is_ok_and(|s| s.current_state)
}

fn ros_pose(position: xr::Vector3f, q: glam::DQuat) -> [f64; 7] {
    [(-position.z) as f64, (-position.x) as f64, position.y as f64, -q.z, -q.x, q.y, q.w]
}

pub fn controller_pose(pose: xr::Posef) -> [f64; 7] {
    let o = pose.orientation;
    let q = glam::DQuat::from_xyzw(o.x as f64, o.y as f64, o.z as f64, o.w as f64);
    ros_pose(pose.position, q)
}

struct Sampler {
    instance: xr::Instance,
    stage: xr::Space,
    spaces: [xr::Space; 3],
}

fn start_pose_sampler(
    instance: &xr::Instance,
    session: &xr::Session<xr::OpenGlEs>,
    grip: &xr::Action<xr::Posef>,
    hands: [xr::Path; 2],
    hub: &'static Hub,
) -> anyhow::Result<()> {
    let space = |hand| grip.create_space(session, hand, xr::Posef::IDENTITY);
    let sampler = Sampler {
        instance: instance.clone(),
        stage: session
            .create_reference_space(xr::ReferenceSpaceType::STAGE, xr::Posef::IDENTITY)?,
        spaces: [
            space(hands[0])?,
            space(hands[1])?,
            session.create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)?,
        ],
    };
    std::thread::spawn(move || {
        let period = std::time::Duration::from_nanos(1_000_000_000 / POSE_HZ);
        let mut next = std::time::Instant::now();
        let mut seq = 0u64;
        loop {
            let capture =
                Capture { at_ns: Some(boottime_ns()), seq: Some(seq), ..Default::default() };
            seq += 1;
            if let Ok(time) = sampler.instance.now() {
                for (part, space) in sampler.spaces.iter().enumerate() {
                    hub.publish_pose(capture, part, located(space.relate(&sampler.stage, time)));
                }
            }
            next += period;
            let now = std::time::Instant::now();
            match next > now {
                true => std::thread::sleep(next - now),
                false => next = now,
            }
        }
    });
    Ok(())
}

impl Xr {
    /// Sets `began` once the session is running: from then on a failure
    /// cannot be retried in this process (the loader is one-shot), and the
    /// pose sampler already holds the session.
    pub fn start(
        hub: &'static Hub,
        ui_size: (u32, u32),
        began: &mut bool,
    ) -> anyhow::Result<(Xr, xr::FrameWaiter, xr::FrameStream<xr::OpenGlEs>)> {
        let entry = unsafe { xr::Entry::load() }.context("load loader")?;
        if let Err(e) = entry.initialize_android_loader() {
            info!("android loader already initialized ({e})");
        }

        let available = entry.enumerate_extensions().context("enumerate_extensions")?;
        let mut wanted = xr::ExtensionSet::default();
        wanted.khr_android_create_instance = true;
        wanted.khr_opengl_es_enable = true;
        wanted.fb_passthrough = available.fb_passthrough;
        wanted.khr_convert_timespec_time = available.khr_convert_timespec_time;

        let instance = entry
            .create_instance(
                &xr::ApplicationInfo {
                    application_name: "YUBI Go",
                    application_version: 1,
                    engine_name: "",
                    engine_version: 0,
                    api_version: xr::Version::new(1, 0, 0),
                },
                &wanted,
                &[],
            )
            .context("create_instance")?;

        let props = instance.properties().context("properties")?;
        let system = instance.system(xr::FormFactor::HEAD_MOUNTED_DISPLAY).context("system")?;
        info!("openxr runtime {} — creating session", props.runtime_name);

        instance.graphics_requirements::<xr::OpenGlEs>(system).context("graphics_requirements")?;
        let egl = Egl::new()?;

        // A session made before the activity is ready stays IDLE for ever with
        // no error, so it is recreated until it reaches READY. The bound is
        // because the background also looks like this, and retrying there
        // fights the app in front. 4 tries of 3 s are chosen by hand.
        let mut events = xr::EventDataBuffer::new();
        let mut attempts = 0;
        let (session, frame_wait, frame_stream) = 'ready: loop {
            attempts += 1;
            if attempts > 4 {
                anyhow::bail!("session never reached READY in {attempts} attempts");
            }
            // SAFETY: the EGL handles stay valid for the life of the process,
            // because `Egl` never destroys them.
            let triple = unsafe {
                instance.create_session::<xr::OpenGlEs>(
                    system,
                    &xr::opengles::SessionCreateInfo::Android {
                        display: egl.display,
                        config: egl.config,
                        context: egl.context,
                    },
                )
            }
            .context("create_session")?;

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while std::time::Instant::now() < deadline {
                while let Some(event) = instance.poll_event(&mut events).context("poll_event")? {
                    if let xr::Event::SessionStateChanged(e) = event {
                        info!("openxr state -> {:?}", e.state());
                        if e.state() == xr::SessionState::READY {
                            break 'ready triple;
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            info!("openxr session never reached READY (activity not ready yet) — recreating");
            drop(triple);
            std::thread::sleep(std::time::Duration::from_millis(500));
        };
        session.begin(xr::ViewConfigurationType::PRIMARY_STEREO).context("session begin")?;
        *began = true;
        info!("openxr session running");

        let passthrough = wanted
            .fb_passthrough
            .then(|| {
                let feature = session
                    .create_passthrough(xr::PassthroughFlagsFB::IS_RUNNING_AT_CREATION)
                    .context("create_passthrough")?;
                let layer = session
                    .create_passthrough_layer(
                        &feature,
                        xr::PassthroughFlagsFB::IS_RUNNING_AT_CREATION,
                        xr::PassthroughLayerPurposeFB::RECONSTRUCTION,
                    )
                    .context("create_passthrough_layer")?;
                Ok::<_, anyhow::Error>((feature, layer))
            })
            .transpose()?;
        if passthrough.is_none() {
            warn!("no passthrough extension — headset will show black");
        }
        let pass_raw = passthrough.as_ref().map(|(_, l)| xr::sys::CompositionLayerPassthroughFB {
            ty: xr::sys::CompositionLayerPassthroughFB::TYPE,
            next: std::ptr::null(),
            flags: xr::CompositionLayerFlags::EMPTY,
            space: xr::sys::Space::NULL,
            layer_handle: l.as_raw(),
        });

        let set = instance.create_action_set("yubi", "yubi", 0).context("create_action_set")?;
        let path = |s: &str| instance.string_to_path(s).with_context(|| format!("path {s}"));
        let left_path = path("/user/hand/left")?;
        let right_path = path("/user/hand/right")?;
        let grip = set
            .create_action::<xr::Posef>("grip_pose", "Grip pose", &[left_path, right_path])
            .context("create_action")?;
        let action = |name: &str, label: &str| {
            set.create_action::<bool>(name, label, &[]).with_context(|| format!("action {name}"))
        };
        let actions = Actions {
            rec_toggle: action("rec_toggle", "Toggle recording")?,
            mark_failed: action("mark_failed", "Mark the episode failed")?,
            calibrate: action("calibrate", "Calibrate the grippers")?,
            qr_show: action("qr_show", "Show the monitor QR")?,
            readiness: action("readiness", "Say whether the rig is ready")?,
            feedback: set
                .create_action::<xr::Haptic>("feedback", "Feedback", &[left_path, right_path])
                .context("haptic action")?,
            inputs: Inputs {
                hands: [left_path, right_path],
                trigger: set.create_action("in_trigger", "Trigger", &[left_path, right_path])?,
                squeeze: set.create_action("in_squeeze", "Squeeze", &[left_path, right_path])?,
                thumbstick: set.create_action(
                    "in_thumbstick",
                    "Thumbstick",
                    &[left_path, right_path],
                )?,
                buttons: BUTTONS
                    .iter()
                    .map(|(name, _, _)| {
                        set.create_action(&format!("in_{name}"), name, &[left_path, right_path])
                    })
                    .collect::<Result<_, _>>()
                    .context("input actions")?,
            },
            set,
        };
        let profile = path("/interaction_profiles/oculus/touch_controller")?;
        let mut bindings = vec![
            xr::Binding::new(&grip, path("/user/hand/left/input/grip/pose")?),
            xr::Binding::new(&grip, path("/user/hand/right/input/grip/pose")?),
            xr::Binding::new(&actions.rec_toggle, path("/user/hand/right/input/a/click")?),
            xr::Binding::new(&actions.mark_failed, path("/user/hand/right/input/b/click")?),
            xr::Binding::new(&actions.calibrate, path("/user/hand/left/input/y/click")?),
            xr::Binding::new(&actions.qr_show, path("/user/hand/left/input/trigger/value")?),
            xr::Binding::new(&actions.qr_show, path("/user/hand/right/input/trigger/value")?),
            xr::Binding::new(&actions.readiness, path("/user/hand/left/input/menu/click")?),
            xr::Binding::new(&actions.feedback, path("/user/hand/left/output/haptic")?),
            xr::Binding::new(&actions.feedback, path("/user/hand/right/output/haptic")?),
        ];
        let inputs = &actions.inputs;
        for hand in ["left", "right"] {
            let at = |leaf: &str| path(&format!("/user/hand/{hand}/input/{leaf}"));
            bindings.push(xr::Binding::new(&inputs.trigger, at("trigger/value")?));
            bindings.push(xr::Binding::new(&inputs.squeeze, at("squeeze/value")?));
            bindings.push(xr::Binding::new(&inputs.thumbstick, at("thumbstick")?));
            for (action, (_, left, right)) in inputs.buttons.iter().zip(BUTTONS) {
                let leaf = if hand == "left" { Some(left) } else { right };
                if let Some(leaf) = leaf {
                    bindings.push(xr::Binding::new(action, at(leaf)?));
                }
            }
        }
        instance.suggest_interaction_profile_bindings(profile, &bindings).context("bindings")?;
        session.attach_action_sets(&[&actions.set]).context("attach_action_sets")?;

        let stage = session
            .create_reference_space(xr::ReferenceSpaceType::STAGE, xr::Posef::IDENTITY)
            .context("stage space")?;
        let view_space = session
            .create_reference_space(xr::ReferenceSpaceType::VIEW, xr::Posef::IDENTITY)
            .context("view space")?;
        let sampling = match wanted.khr_convert_timespec_time {
            true => {
                match start_pose_sampler(&instance, &session, &grip, [left_path, right_path], hub) {
                    Ok(()) => {
                        info!("pose sampler running at {POSE_HZ} Hz");
                        true
                    }
                    Err(e) => {
                        warn!(
                            "pose sampler unavailable ({e}) — falling back to per-frame sampling"
                        );
                        false
                    }
                }
            }
            false => {
                warn!("no KHR_convert_timespec_time — poses sampled at the display rate");
                false
            }
        };

        let mut tracked = Vec::new();
        for hand in [left_path, right_path] {
            tracked.push(grip.create_space(&session, hand, xr::Posef::IDENTITY)?);
        }
        tracked.push(view_space);

        let views = instance
            .enumerate_view_configuration_views(system, xr::ViewConfigurationType::PRIMARY_STEREO)
            .context("view config")?;
        let (w, h) =
            (views[0].recommended_image_rect_width, views[0].recommended_image_rect_height);
        let make = |w: u32, h: u32, usage: xr::SwapchainUsageFlags| {
            session
                .create_swapchain(&xr::SwapchainCreateInfo {
                    create_flags: xr::SwapchainCreateFlags::EMPTY,
                    usage_flags: usage,
                    format: gl::RGBA8,
                    sample_count: 1,
                    width: w,
                    height: h,
                    face_count: 1,
                    array_size: 1,
                    mip_count: 1,
                })
                .context("create_swapchain")
        };
        let mut swapchains = Vec::new();
        for _ in 0..2 {
            let swapchain = make(w, h, xr::SwapchainUsageFlags::COLOR_ATTACHMENT)?;
            let images = swapchain.enumerate_images().context("images")?;
            let fbos: Vec<u32> = images.iter().map(|&tex| gl::framebuffer_for(tex)).collect();
            swapchains.push((swapchain, fbos));
        }
        info!("openxr swapchains ready: 2x {w}x{h}");

        let ui_swapchain = make(
            ui_size.0,
            ui_size.1,
            xr::SwapchainUsageFlags::COLOR_ATTACHMENT | xr::SwapchainUsageFlags::SAMPLED,
        )?;
        let ui_images = ui_swapchain.enumerate_images().context("ui images")?;

        Ok((
            Xr {
                instance,
                session,
                stage,
                actions,
                tracked,
                swapchains,
                ui_swapchain,
                ui_images,
                eye_size: (w, h),
                sampling,
                _passthrough: passthrough,
                pass_raw,
                events,
                hub,
            },
            frame_wait,
            frame_stream,
        ))
    }

    pub fn passthrough_base<'a>(
        &'a self,
    ) -> Option<&'a xr::CompositionLayerBase<'a, xr::OpenGlEs>> {
        // SAFETY: `CompositionLayerBase` is the header every `XrCompositionLayer*`
        // struct starts with, and `pass_raw` lives as long as `self`.
        self.pass_raw.as_ref().map(|raw| unsafe {
            std::mem::transmute::<_, &'a xr::CompositionLayerBase<xr::OpenGlEs>>(raw)
        })
    }

    pub fn poll(&mut self, running: &mut bool) -> anyhow::Result<bool> {
        let offset = xr_to_boottime(&self.instance);
        while let Some(event) = self.instance.poll_event(&mut self.events).context("poll_event")? {
            match event {
                xr::Event::SessionStateChanged(e) => {
                    info!("openxr state -> {:?}", e.state());
                    let event = Status::new("xr/session")
                        .with("state", format!("{:?}", e.state()))
                        .with("xr_time_ns", e.time().as_nanos());
                    self.hub.publish_event(with_boottime(event, "boottime_ns", e.time(), offset));
                    match e.state() {
                        xr::SessionState::READY => {
                            self.session
                                .begin(xr::ViewConfigurationType::PRIMARY_STEREO)
                                .context("session begin")?;
                            *running = true;
                        }
                        xr::SessionState::STOPPING => {
                            self.session.end().context("session end")?;
                            *running = false;
                        }
                        xr::SessionState::EXITING => return Ok(false),
                        xr::SessionState::LOSS_PENDING => anyhow::bail!("session loss pending"),
                        _ => {}
                    }
                }
                xr::Event::InstanceLossPending(_) => anyhow::bail!("instance loss pending"),
                xr::Event::ReferenceSpaceChangePending(e) => {
                    // Every pose after `change_time` is in the moved space, so
                    // a recording needs this to stitch the two together.
                    warn!("openxr: {:?} space moves", e.reference_space_type());
                    let p = controller_pose(e.pose_in_previous_space());
                    let event =
                        Status { level: status::WARN, ..Status::new("xr/reference_space_change") }
                            .with("space", format!("{:?}", e.reference_space_type()))
                            .with("change_time_xr_ns", e.change_time().as_nanos())
                            .with("pose_valid", e.pose_valid())
                            .with("pose_in_previous_space", format!("{p:?}"));
                    let change = e.change_time();
                    self.hub.publish_event(with_boottime(
                        event,
                        "change_boottime_ns",
                        change,
                        offset,
                    ));
                }
                xr::Event::InteractionProfileChanged(_) => {
                    self.hub.publish_event(Status::new("xr/interaction_profile_changed"));
                }
                xr::Event::EventsLost(e) => {
                    warn!("openxr: {} events lost", e.lost_event_count());
                    self.hub.publish_event(
                        Status { level: status::WARN, ..Status::new("xr/events_lost") }
                            .with("count", e.lost_event_count()),
                    );
                }
                _ => {}
            }
        }
        Ok(true)
    }
}

/// An event's XrTime also as boottime, the clock the recording's `clock`
/// record converts from. Left out when the runtime cannot say "now".
fn with_boottime(event: Status, key: &str, at: xr::Time, offset: Option<i64>) -> Status {
    match offset {
        Some(offset) => event.with(key, at.as_nanos() + offset),
        None => event,
    }
}

pub fn adopt_android_context(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) {
    unsafe {
        ndk_context::release_android_context();
        ndk_context::initialize_android_context(vm, activity);
    }
}
