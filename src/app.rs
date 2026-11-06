use std::sync::{Arc, OnceLock};

use android_activity::input::{InputEvent, Keycode};
use android_activity::{AndroidApp, InputStatus, MainEvent, PollEvent};
use log::{info, warn};

use crate::hub::{Hub, EPISODE_DIR};
use crate::misc::config::Config;
use crate::record::backend::poll;

pub(crate) fn hub() -> &'static Arc<Hub> {
    static HUB: OnceLock<Arc<Hub>> = OnceLock::new();
    HUB.get_or_init(|| {
        let hub = Arc::new(Hub::default());
        hub.start_writer();
        hub
    })
}

fn start_backend() {
    let started = std::time::Instant::now();
    poll::poll(EPISODE_DIR.to_string(), poll::BEAT, move || poll::Status {
        robot_type: Config::load(EPISODE_DIR).identity.robot_type,
        reported_at_ns: crate::misc::clock::realtime_ns(),
        battery: crate::misc::power::battery().map_or((-1, false), |b| (b.pct, b.charging)),
        uptime_sec: started.elapsed().as_secs_f64(),
        disk: crate::misc::disk::usage(EPISODE_DIR),
        recording: hub().recording(),
        gate_level: hub().gate_level(),
        gate: hub().gate_groups(),
    });
}

#[no_mangle]
fn android_main(app: AndroidApp) {
    // usvg warns once per CJK glyph run that falls through to the second
    // font — hundreds of lines a second, enough to roll logcat in seconds.
    let logger = android_logger::AndroidLogger::new(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("yubigo")
            .with_filter(android_logger::FilterBuilder::new().parse("info,usvg=off").build()),
    );
    if log::set_boxed_logger(Box::new(crate::misc::journal::Tee(logger))).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
    info!("YUBI Go starting");
    crate::view::play_boot();
    let (vm, activity) = (app.vm_as_ptr(), app.activity_as_ptr());
    hub().adopt_cameras(&Config::load(EPISODE_DIR));
    // Before starting the core: a fresh install has no payload but the APK's.
    let assets = app.asset_manager();
    let open = |name: &str| {
        let path = std::ffi::CString::new(format!("airoa/{name}")).ok()?;
        assets.open(&path)
    };
    match crate::misc::core::unpack(std::path::Path::new(EPISODE_DIR), open) {
        Ok(true) => info!("airoa_core payload unpacked from the APK"),
        Ok(false) => {}
        Err(e) => warn!("airoa_core payload not unpacked: {e}"),
    }
    use crate::stream::http::LICENSES;
    let licenses = |name: &str| assets.open(&std::ffi::CString::new(name).ok()?);
    let files = std::path::Path::new(EPISODE_DIR);
    if let Err(e) = crate::misc::core::unpack_bundle(files, &[], LICENSES, licenses) {
        warn!("{LICENSES} not unpacked: {e}");
    }
    crate::misc::core::start(EPISODE_DIR, crate::record::upload::GUEST_ROOT);
    let assets = app.asset_manager();
    // By bare name: android/build.sh puts it beside libyubi_go.so.
    crate::misc::speech::start(hub(), EPISODE_DIR, "libonnxruntime.so", move || {
        let open = |name: &str| {
            let path = std::ffi::CString::new(format!("tts/{name}")).ok()?;
            assets.open(&path)
        };
        match crate::misc::speech::unpack(std::path::Path::new(EPISODE_DIR), open) {
            Ok(true) => info!("speech engine unpacked from the APK"),
            Ok(false) => {}
            Err(e) => warn!("speech engine not unpacked: {e}"),
        }
    });
    crate::view::start(vm, activity, hub());
    match crate::stream::serve(hub(), 9090, 8080) {
        Ok(()) => {
            info!("rosbridge on :9090, side interface on :8080");
            match crate::stream::mdns::start(vm, activity, &mdns_name(), 8080) {
                Ok(()) => info!(
                    "mDNS: side interface at http://{}:8080",
                    crate::stream::mdns::shown().unwrap_or("?")
                ),
                Err(e) => warn!("mDNS unavailable: {e}"),
            }
        }
        Err(e) => warn!("servers unavailable: {e}"),
    }
    crate::sensors::worker::watch_usb(vm, activity);
    crate::sensors::battery::watch(vm, activity);
    start_backend();
    crate::record::upload::install(EPISODE_DIR.to_string(), || hub().busy());

    let mut quit = false;
    while !quit {
        app.poll_events(Some(std::time::Duration::from_millis(500)), |event| match event {
            PollEvent::Main(MainEvent::Destroy) => quit = true,
            PollEvent::Main(MainEvent::Resume { .. }) => crate::view::set_foreground(true),
            PollEvent::Main(MainEvent::Pause) => crate::view::set_foreground(false),
            _ => {}
        });
        if let Ok(mut iter) = app.input_events_iter() {
            // Everything else stays swallowed: an unhandled Back finishes the activity.
            while iter.next(|event| match event {
                InputEvent::KeyEvent(key)
                    if matches!(
                        key.key_code(),
                        Keycode::VolumeUp | Keycode::VolumeDown | Keycode::VolumeMute
                    ) =>
                {
                    InputStatus::Unhandled
                }
                _ => InputStatus::Handled,
            }) {}
        }
    }
    info!("YUBI Go exiting");
    hub().stop_and_wait();
    std::process::exit(0);
}

/// The robot's name in the backend, from the cached `/robot/me`, so the host is
/// `yubi-go-075.local` and not the robot id. The first start on a new headset
/// has no cache yet and uses the id until the next start.
fn mdns_name() -> String {
    crate::misc::config::Profile::load(EPISODE_DIR)
        .map(|p| p.robot_name)
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| Config::load(EPISODE_DIR).identity.robot_id)
}
