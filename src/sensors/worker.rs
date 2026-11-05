#[cfg(target_os = "android")]
mod worker_impl {
    use crate::app::hub;
    use crate::sensors::{gripper, usb, webcam};
    use anyhow::Context as _;
    use log::{error, info, warn};
    use nusb::MaybeFuture;
    use std::time::{Duration, Instant};

    const WRIST_SIZE: (u16, u16) = (640, 480);
    /// Two 1920x1080 eyes side by side.
    const STEREO_SIZE: (u16, u16) = (3840, 1080);
    /// 30 fps, the head camera's recorded rate. At 60 the h264 setting's JPEG
    /// decode could not keep up and kept only every 3rd or 4th frame, ~17 fps.
    const STEREO_INTERVAL: u32 = 333_333;
    /// Picks alt 9 (1984 bytes), which carries 1080p at 30 fps.
    /// Alt 7 (1280) carried 9.8 MB/s of its 10.2 until a replug, then
    /// delivered only damaged frames (2026-10-22). Alt 10 (2880) plus two
    /// wrists at 1600 each passes the 6000 bytes per microframe USB 2.0
    /// allows periodic transfers (80% of 7500).
    const STEREO_MIN_ISO_BYTES: usize = 1984;
    /// The side-by-side stereo module (an Ailipu/ELP "3D USB Camera"): left eye
    /// in the left half. Nothing in its UVC descriptors says stereo, so its
    /// VID:PID is how an episode comes to know.
    const STEREO_MODULE: (u16, u16) = (0x32e4, 0x0035);

    /// URBs kept in flight per camera, each `usbfs::PACKETS` microframes:
    /// 16 x 32 x 125 us is ~64 ms that the reap thread may be late before
    /// the camera has nowhere to send. A margin chosen by hand.
    const URBS: usize = 16;

    /// Frames the publish thread may fall behind by before the reap callback
    /// drops them (`queue_drops`). Eight is ~80 ms at the wrists' ~100 fps,
    /// so a decoder that is too slow shows up in `queue_drops` within a second.
    const HANDOFF_QUEUE: usize = 8;

    pub(crate) fn stream_camera(cam: usb::UsbCamera) {
        let name = cam.name.clone();
        let result = match cam.kind {
            usb::Kind::Video => {
                static RANKS: std::sync::Mutex<[bool; crate::hub::USB_CAM_SLOTS.len()]> =
                    std::sync::Mutex::new([false; crate::hub::USB_CAM_SLOTS.len()]);
                let rank = {
                    let remembered = match cam.chain.as_deref() {
                        Some(chain) => {
                            let rank = crate::misc::usb::rank_of(crate::hub::EPISODE_DIR, chain);
                            if rank.is_none() {
                                warn!(
                                    "[{name}] hub port {chain} is not in camera_ports.json, \
                                     which is full; taking the first free rank. Delete the \
                                     file and pick the cameras again if the hub was rewired"
                                );
                            }
                            rank
                        }
                        None => {
                            warn!("[{name}] hub port unknown; taking the first free rank");
                            None
                        }
                    };
                    // A taken rank falls back to a free one rather than dropping
                    // the camera: the slots follow `camera_ports` in config.json
                    // (`Hub::adopt_cameras`), not the rank. With none free, the
                    // device stays open and waits: let go, the scan would reopen
                    // and log it every 2 s.
                    let mut waiting = false;
                    loop {
                        let mut used = RANKS.lock().unwrap();
                        let free = match remembered {
                            Some(i) if !used[i] => Some(i),
                            _ => used.iter().position(|u| !u),
                        };
                        if let Some(i) = free {
                            used[i] = true;
                            break i;
                        }
                        drop(used);
                        if !crate::misc::usb::attached(&cam.path) {
                            info!("[{name}] unplugged while waiting for a camera role");
                            return;
                        }
                        if !waiting {
                            error!("[{name}] every camera role is taken; waiting for one");
                            waiting = true;
                        }
                        // The scan's own interval, which is when `attached` can change.
                        std::thread::sleep(Duration::from_secs(2));
                    }
                };
                // Frees the rank however the stream ends, a panic included.
                struct Rank(usize);
                impl Drop for Rank {
                    fn drop(&mut self) {
                        RANKS.lock().unwrap_or_else(|e| e.into_inner())[self.0] = false;
                    }
                }
                let _rank = Rank(rank);
                let port = cam.port();
                info!(
                    "[{name}] camera {} on USB port {}",
                    rank + 1,
                    port.as_deref().unwrap_or("?")
                );
                hub().set_cam_port(rank, port);
                hub().set_cam_name(rank, &name);
                let result = stream_camera_inner(cam, rank);
                let closed = crate::misc::status::Status {
                    level: crate::misc::status::ERROR,
                    message: "stream closed".to_string(),
                    ..Default::default()
                };
                hub().set_cam_status(rank, "", closed.clone());
                hub().set_cam_status(rank, "/controls", closed);
                hub().set_cam_port(rank, None);
                result
            }
            usb::Kind::Serial => {
                // Keyed by port, not by `name`: the device path changes with
                // every replug, which would leave the old entry in every take.
                let port = cam.port();
                let key = format!("encoder/{}", port.as_deref().unwrap_or(&name));
                let result = stream_serial_inner(cam, port.unwrap_or_default(), &key);
                hub().set_status(crate::misc::status::Status {
                    level: crate::misc::status::ERROR,
                    message: "stream closed".to_string(),
                    ..crate::misc::status::Status::new(key)
                });
                result
            }
        };
        match &result {
            Ok(()) => crate::misc::usb::event(&hub().dir(), &format!("[{name}] closed")),
            Err(e) => {
                error!("[{name}] {e:#}");
                crate::misc::usb::event(&hub().dir(), &format!("[{name}] closed: {e:#}"));
            }
        }
    }

    /// Makes the device eligible for reopening when its worker ends, a panic
    /// included, or the scan skips it until the app restarts.
    struct Worker {
        name: String,
        opened: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
        requested: std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
    }

    impl Drop for Worker {
        fn drop(&mut self) {
            info!("[{}] worker exited — eligible for reopen", self.name);
            self.opened.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.name);
            self.requested.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.name);
        }
    }

    /// Cleared when the stream ends, by whatever path it ends.
    struct Streaming(std::sync::Arc<std::sync::atomic::AtomicBool>);

    impl Drop for Streaming {
        fn drop(&mut self) {
            self.0.store(false, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The camera's exposure, gain and colour controls once a second, off the
    /// reap thread: a GET_CUR waits on the device, and that thread must not.
    /// A control that fails once is dropped rather than asked again each second.
    fn watch_controls(
        dev: webcam::ControlHandle,
        mut controls: Vec<webcam::Control>,
        rank: usize,
        streaming: std::sync::Arc<std::sync::atomic::AtomicBool>,
        name: String,
    ) {
        use crate::misc::status::Status;
        while streaming.load(std::sync::atomic::Ordering::Relaxed) && !controls.is_empty() {
            let mut status = Status::new("");
            controls.retain(|c| match webcam::read_control(&dev, c) {
                Ok(v) => {
                    status.values.push((c.name.to_string(), v.to_string()));
                    true
                }
                Err(e) => {
                    warn!("[{name}] {} unreadable, no longer asked: {e}", c.name);
                    false
                }
            });
            hub().set_cam_status(rank, "/controls", status);
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn stream_serial_inner(mut cam: usb::UsbCamera, port: String, key: &str) -> anyhow::Result<()> {
        let fd = cam.take_fd().context("device already consumed")?;
        let device = nusb::Device::from_fd(fd).wait().context("nusb from_fd")?;
        let config = device.active_configuration().context("active_configuration")?;
        let info = gripper::find_acm(&config).context("no CDC-ACM interfaces found")?;
        info!("[{}] CDC-ACM {info:?}", cam.name);

        let comm =
            device.detach_and_claim_interface(info.comm_interface).wait().context("claim comm")?;
        gripper::configure(&comm, &info)?;
        let data =
            device.detach_and_claim_interface(info.data_interface).wait().context("claim data")?;
        data.set_alt_setting(info.data_alt_setting).wait().context("set_alt_setting")?;
        let mut ep = data
            .endpoint::<nusb::transfer::Bulk, nusb::transfer::In>(info.ep_in)
            .context("open endpoint")?;

        let len = info.ep_max_packet;
        for _ in 0..4 {
            ep.submit(ep.allocate(len));
        }
        let mut counter = gripper::SerialCounter::default();
        let mut seq = [0u64; 2];
        let mut shown = 0u32;
        let name = cam.name.clone();
        let mut last = (Instant::now(), 0u64, 0u64);
        let mut status_at = (Instant::now(), 0u64, 0u64);
        let (mut unparsed, mut banners, mut last_line) = (0u64, 0u64, String::new());
        let mut silent = 0u32;
        loop {
            let Some(completion) = ep.wait_next_complete(Duration::from_secs(1)) else {
                silent += 1;
                warn!("[{}] serial silent for {silent} s", cam.name);
                // Two seconds: a live encoder streams 200 lines/s, so a gap
                // that long is a chip that is not running.
                if silent >= 2 {
                    // Reboot the chip, then let the scan reopen it — the same
                    // way a stalled camera comes back.
                    let outcome = match gripper::reboot(&comm, &info) {
                        Ok(()) => "rebooted",
                        Err(e) => {
                            warn!("[{}] reboot: {e}", cam.name);
                            "reboot failed"
                        }
                    };
                    anyhow::bail!("serial silent for {silent} s — {outcome}, reopening");
                }
                continue;
            };
            silent = 0;
            if let Err(e) = completion.status {
                anyhow::bail!("serial transfer: {e}");
            }
            counter.push_with(&completion.buffer[..completion.actual_len], |line| {
                if shown < 4 {
                    shown += 1;
                    info!("[{name}] serial line {shown}: {line:?}");
                }
                last_line.clear();
                last_line.push_str(line);
                if line.starts_with('#') {
                    banners += 1;
                    return;
                }
                let Some(side) = gripper::side_of(line) else {
                    unparsed += 1;
                    return;
                };
                let id = gripper::device_id(line).unwrap_or_default();
                match line.rsplit(',').next().and_then(|v| v.trim().parse().ok()) {
                    Some(v) => {
                        hub().publish_gripper(Some(seq[side]), side, id, v);
                        seq[side] += 1;
                    }
                    None => unparsed += 1,
                }
            });
            let mut buf = completion.buffer;
            buf.clear();
            buf.set_requested_len(len);
            ep.submit(buf);

            if status_at.0.elapsed() >= Duration::from_secs(1) {
                let dt = status_at.0.elapsed().as_secs_f64();
                let status = crate::misc::status::Status {
                    hardware_id: port.clone(),
                    ..crate::misc::status::Status::new(key)
                }
                .with("lines_per_s", format!("{:.1}", (counter.lines - status_at.1) as f64 / dt))
                .with("bytes_per_s", format!("{:.0}", (counter.bytes - status_at.2) as f64 / dt))
                .with("lines", counter.lines)
                .with("bytes", counter.bytes)
                .with("unparsed_lines", unparsed)
                .with("banner_lines", banners)
                .with("published", format!("{seq:?}"))
                .with("last_line", &last_line);
                hub().set_status(status);
                status_at = (Instant::now(), counter.lines, counter.bytes);
            }

            if last.0.elapsed() >= Duration::from_secs(5) {
                let dt = last.0.elapsed().as_secs_f64();
                info!(
                    "[{}] serial {:.0} lines/s {:.0} B/s (total {} lines)",
                    cam.name,
                    (counter.lines - last.1) as f64 / dt,
                    (counter.bytes - last.2) as f64 / dt,
                    counter.lines,
                );
                last = (Instant::now(), counter.lines, counter.bytes);
            }
        }
    }

    fn stream_camera_inner(mut cam: usb::UsbCamera, rank: usize) -> anyhow::Result<()> {
        let fd = cam.take_fd().context("device already consumed")?;
        let mut dev = webcam::Device::new(fd);
        let descriptors = dev.descriptors().context("read descriptors")?;
        for (i, chunk) in descriptors.chunks(32).enumerate() {
            let hex: String = chunk.iter().map(|b| format!("{b:02x}")).collect();
            info!("[{}] desc {:04x} {}", cam.name, i * 32, hex);
        }
        // 18 bytes of device descriptor first, which also holds the VID:PID below.
        let config = descriptors
            .get(18..)
            .and_then(nusb::descriptors::ConfigurationDescriptor::new)
            .context("no configuration descriptor")?;

        let id = (
            u16::from_le_bytes([descriptors[8], descriptors[9]]),
            u16::from_le_bytes([descriptors[10], descriptors[11]]),
        );
        let (want, interval, min_iso) = if id == STEREO_MODULE {
            (STEREO_SIZE, STEREO_INTERVAL, STEREO_MIN_ISO_BYTES)
        } else {
            (WRIST_SIZE, webcam::TARGET_INTERVAL, webcam::MIN_ISO_BYTES_PER_MICROFRAME)
        };
        let info = webcam::find_mjpeg_stream(&config, want.0, want.1, interval, min_iso)
            .context("no MJPEG VideoStreaming interface found")?;
        info!(
            "[{}] MJPEG {}x{} frame_index={} ep={:#04x} type={:?} alt={} bytes/microframe={}",
            cam.name,
            info.frame.width,
            info.frame.height,
            info.frame.frame_index,
            info.ep_address,
            info.ep_transfer_type,
            info.ep_alt_setting,
            info.ep_max_packet,
        );
        if info.ep_transfer_type != nusb::descriptors::TransferType::Isochronous {
            anyhow::bail!(
                "{:?} video endpoint {:#04x}: only isochronous UVC cameras are supported",
                info.ep_transfer_type,
                info.ep_address,
            );
        }

        hub().set_cam_format(
            rank,
            info.frame.width,
            info.frame.height,
            1e7 / info.frame.interval as f32,
            id == STEREO_MODULE,
        );
        dev.claim_interface(info.vs_interface)
            .with_context(|| format!("claim VS interface {}", info.vs_interface))?;
        let negotiated = webcam::probe_commit(&dev, &info)?;
        info!(
            "[{}] committed: interval={} maxPayload={} maxFrame={}",
            cam.name,
            negotiated.frame_interval,
            negotiated.max_payload_transfer_size,
            negotiated.max_video_frame_size
        );
        dev.set_alt_setting(info.vs_interface, info.ep_alt_setting).context("set_alt_setting")?;
        dev.start_iso(info.ep_address, info.ep_max_packet, URBS).context("submit iso URBs")?;

        let controls = webcam::find_controls(&config);
        info!("[{}] controls: {:?}", cam.name, controls.iter().map(|c| c.name).collect::<Vec<_>>());
        let streaming = Streaming(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)));
        // GET_CUR goes to the VideoControl interface, which streaming never
        // claims; unclaimed, every read answers EBUSY (seen on a Quest 3S, 2026-10-21).
        let vc = controls.first().map(|c| (c.index & 0xff) as u8);
        let controls = match vc.map(|vc| dev.claim_interface(vc)) {
            Some(Err(e)) => {
                warn!("[{}] VideoControl interface not claimed, controls not read: {e}", cam.name);
                Vec::new()
            }
            _ => controls,
        };
        if !controls.is_empty() {
            let reader = dev.control_handle().context("dup the device for control reads")?;
            let (on, name) = (streaming.0.clone(), cam.name.clone());
            // The stereo module ships with backlight compensation at 80, which
            // clipped 10% of a desk view to white. 40 is by eye under ceiling
            // lights (2026-10-22): 1 left the same view at mean luma 9.
            let backlight = (id == STEREO_MODULE)
                .then(|| controls.iter().find(|c| c.name == "backlight_compensation").cloned())
                .flatten();
            std::thread::spawn(move || {
                if let Some(c) = backlight {
                    match webcam::write_control(&reader, &c, 40) {
                        Ok(()) => info!("[{name}] backlight_compensation set to 40"),
                        Err(e) => warn!("[{name}] backlight_compensation not set: {e}"),
                    }
                }
                watch_controls(reader, controls, rank, on, name)
            });
        }

        type Reaped = (u64, crate::misc::capture::Capture, std::sync::Arc<[u8]>);
        let (tx, rx) = std::sync::mpsc::sync_channel::<Reaped>(HANDOFF_QUEUE);
        let worker = cam.name.clone();
        std::thread::spawn(move || {
            while let Ok((at_ns, capture, jpeg)) = rx.recv() {
                hub().on_frame_at(rank, at_ns, capture, jpeg);
            }
            info!("[{worker}] publish thread done");
        });
        let mut queue_drops = 0u64;
        // The frames passed on. `counter.frames()` counts damaged ones too, so
        // a camera that sends only those would never count as stalled.
        let mut delivered = 0u64;

        let mut counter = webcam::FrameCounter::default();
        let mut last_report = Instant::now();
        let (mut last_frames, mut last_bytes) = (0u64, 0u64);
        let mut last_flow = (Instant::now(), 0u64, 0u64);
        let started = Instant::now();
        let mut stall = webcam::StallWatch::new(started);

        loop {
            dev.reap_iso(|packet| {
                let webcam::Packet::Data(packet, at) = packet else {
                    return counter.lost_packet();
                };
                counter.set_at(at);
                counter.push_payload_with(packet, |timing, seq, frame| {
                    let at_ns = crate::misc::clock::boottime_ns();
                    let uvc = crate::misc::capture::UvcFrame {
                        first: timing.first.unwrap_or(timing.last),
                        last: timing.last,
                        bytes: frame.len() as u32,
                        errors: timing.counts.errors,
                        bad_headers: timing.counts.bad_headers,
                        damaged_frames: timing.counts.damaged_frames,
                        lost_packets: timing.counts.lost_packets,
                        queue_drops,
                    };
                    // Stamped where the frame's first packet arrived, as uvcvideo
                    // stamps the V4L2 buffer that the YUBI ROS stack (yubi-sw)
                    // records through usb_cam.
                    let capture = crate::misc::capture::Capture {
                        at_ns: Some(uvc.first.reaped_ns),
                        seq: Some(seq),
                        uvc_pts: timing.pts,
                        uvc_scr: timing.scr,
                        uvc: Some(uvc),
                    };
                    delivered += 1;
                    if tx.try_send((at_ns, capture, frame.into())).is_err() {
                        queue_drops += 1;
                    }
                })
            })
            .context("reap")?;

            if stall.poll(Instant::now(), delivered, counter.payload_bytes) {
                anyhow::bail!(
                    "{:.2} MB/s and no frame for {}s (total_frames={}) — stalled",
                    counter.payload_bytes as f64 / started.elapsed().as_secs_f64() / 1e6,
                    webcam::STALL_TIMEOUT.as_secs(),
                    counter.frames(),
                );
            }

            if last_flow.0.elapsed() >= Duration::from_secs(1) {
                let dt = last_flow.0.elapsed().as_secs_f32();
                let fresh = counter.payload_bytes - last_flow.1;
                hub().set_byte_rate(rank, fresh as f32 / dt / 1e6);
                let status = crate::misc::status::Status::new("")
                    .with("device", &cam.name)
                    .with("vid_pid", format!("{:04x}:{:04x}", id.0, id.1))
                    .with("width", info.frame.width)
                    .with("height", info.frame.height)
                    .with("frame_interval_100ns", negotiated.frame_interval)
                    .with("max_payload", negotiated.max_payload_transfer_size)
                    .with("iso_bytes_per_microframe", info.ep_max_packet)
                    .with("fps", format!("{:.2}", (counter.frames() - last_flow.2) as f32 / dt))
                    .with("mb_per_s", format!("{:.3}", fresh as f32 / dt / 1e6))
                    .with("frames", counter.frames())
                    .with("errors", counter.errors)
                    .with("bad_headers", counter.bad_headers)
                    .with("damaged_frames", counter.damaged_frames)
                    .with("lost_packets", counter.lost_packets)
                    .with("queue_drops", queue_drops);
                hub().set_cam_status(rank, "", status);
                last_flow = (Instant::now(), counter.payload_bytes, counter.frames());
            }

            if last_report.elapsed() >= Duration::from_secs(5) {
                let dt = last_report.elapsed().as_secs_f64();
                let frames = counter.frames();
                info!(
                    "[{}] t={}s fps={:.1} {:.2} MB/s total_frames={} errs={} bad_hdrs={} \
                     damaged={} queued_drop={}",
                    cam.name,
                    started.elapsed().as_secs(),
                    (frames - last_frames) as f64 / dt,
                    (counter.payload_bytes - last_bytes) as f64 / dt / 1e6,
                    frames,
                    counter.errors,
                    counter.bad_headers,
                    counter.damaged_frames,
                    queue_drops,
                );
                (last_frames, last_bytes) = (frames, counter.payload_bytes);
                last_report = Instant::now();
            }
        }
    }

    pub(crate) fn watch_usb(vm: *mut std::ffi::c_void, activity: *mut std::ffi::c_void) {
        let (vm, activity) = (vm as usize, activity as usize);
        std::thread::spawn(move || {
            use std::sync::{Arc, Mutex};
            let raw_vm = vm as *mut _;
            let vm = unsafe { jni::JavaVM::from_raw(raw_vm) };
            usb::remember_vm(unsafe { jni::JavaVM::from_raw(raw_vm) });
            let opened = Arc::new(Mutex::new(std::collections::HashSet::new()));
            let requested = Arc::new(Mutex::new(std::collections::HashSet::new()));
            let mut cam_perm = (false, false);
            loop {
                let result = vm.attach_current_thread(|env| -> Result<_, jni::errors::Error> {
                    let jactivity = unsafe { jni::objects::JObject::from_raw(env, activity as _) };
                    if !cam_perm.0 {
                        cam_perm.0 = usb::ensure_camera_permission(env, &jactivity, !cam_perm.1)?;
                        cam_perm.1 = true;
                    }
                    usb::scan(
                        env,
                        &jactivity,
                        &opened.lock().unwrap(),
                        &mut requested.lock().unwrap(),
                    )
                });
                match result {
                    Ok((cams, pending)) => {
                        for cam in cams {
                            let name = cam.name.clone();
                            opened.lock().unwrap().insert(name.clone());
                            let (opened, requested) = (opened.clone(), requested.clone());
                            std::thread::spawn(move || {
                                let _worker = Worker { name, opened, requested };
                                stream_camera(cam);
                            });
                        }
                        if opened.lock().unwrap().is_empty() && pending == 0 {
                            info!("no USB video devices found; waiting (hub plugged in?)");
                        }
                    }
                    Err(e) => error!("usb scan failed: {e}"),
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        });
    }
}
#[cfg(target_os = "android")]
pub(crate) use worker_impl::watch_usb;
