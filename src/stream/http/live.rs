//! The long-lived responses: MJPEG for a monitoring client and the viewer's
//! live view, and the live poses as server-sent events. Each holds its socket,
//! and a thread, for as long as the viewer watches.

use std::io::Write;
use std::net::TcpStream;

use crate::hub::Hub;

const BOUNDARY: &str = "yubigoframe";

/// How long a viewer's `/stream` waits between checks for a new frame. The hub
/// publishes previews at 10 Hz, so this is under the frame interval and costs
/// one sleeping thread per viewer rather than a condvar in the capture path.
const FRAME_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// The live view's most frames per second per camera, chosen by hand to look
/// smooth. The frames go out as the cameras made them, so what the view costs
/// the headset's Wi-Fi grows with this rate.
const LIVE_FPS: u32 = 30;

/// The live view while a take records: the Rig tab's preview rate
/// ([`crate::hub`]'s `PREVIEW_PERIOD`), so watching costs the recording no
/// more than the previews already do.
pub(super) const LIVE_FPS_RECORDING: u32 = 10;

/// The send buffer of every frame stream, chosen by hand at under two wrist
/// frames (the wrists' 4.6-5.9 MB/s at ~98 fps in `sensors::webcam::uvc` is
/// 47-60 kB a frame). Small is the backpressure: the kernel
/// cannot hoard stale frames, so writes block and the client sees fresh video
/// rather than an ever-growing queue.
const SEND_BUFFER: libc::c_int = 96 * 1024;

/// One MJPEG viewer, on its own thread.
///
/// Blocking and long-lived by design: a viewer holds the socket for as long as
/// it is watching, so a thread apiece is what this costs and there is no
/// cheaper shape that still writes frames as they arrive.
pub(super) fn serve_mjpeg(hub: &Hub, mut sock: TcpStream, slot: usize) -> std::io::Result<()> {
    set_send_buffer(&sock, SEND_BUFFER);
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nConnection: close\r\nCache-Control: no-store\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Content-Type: multipart/x-mixed-replace; boundary={BOUNDARY}\r\n\r\n"
    )?;
    let mut seen = 0;
    loop {
        let Some((frame, seq)) = next_frame(hub, slot, seen) else {
            if gone(&sock) {
                return Ok(());
            }
            std::thread::sleep(FRAME_POLL);
            continue;
        };
        seen = seq;
        write_frame(&mut sock, &frame)?;
    }
}

/// One JPEG as one part of a `multipart/x-mixed-replace` body.
fn write_frame(sock: &mut TcpStream, frame: &[u8]) -> std::io::Result<()> {
    write!(
        sock,
        "--{BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        frame.len()
    )?;
    sock.write_all(frame)?;
    sock.write_all(b"\r\n")
}

/// Lowest CPU priority for the calling thread. A live viewer is a convenience,
/// so when the headset is short of CPU the recording threads win it.
fn yield_to_recording() {
    #[cfg(target_os = "android")]
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 19);
    }
}

/// How long one live frame or pose update waits for the next: the rate asked
/// for, capped at [`LIVE_FPS`], and [`LIVE_FPS_RECORDING`] while a take runs.
fn live_period(hub: &Hub, fps: u32) -> std::time::Duration {
    let cap = if hub.recording() { LIVE_FPS_RECORDING } else { LIVE_FPS };
    std::time::Duration::from_secs(1) / fps.clamp(1, cap)
}

/// One camera, every whole frame up to the live rate. The frames are the
/// camera's own JPEGs, so nothing is encoded for this; the cost is the copy to
/// the socket, and the small send buffer drops the rate when Wi-Fi cannot keep
/// up rather than queueing stale frames.
pub(super) fn serve_live(
    hub: &Hub,
    mut sock: TcpStream,
    slot: usize,
    fps: u32,
) -> std::io::Result<()> {
    yield_to_recording();
    set_send_buffer(&sock, SEND_BUFFER);
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nConnection: close\r\nCache-Control: no-store\r\n\
         Content-Type: multipart/x-mixed-replace; boundary={BOUNDARY}\r\n\r\n"
    )?;
    let mut seen = 0;
    loop {
        let started = std::time::Instant::now();
        let Some((frame, seq)) = hub.live_frame(slot, seen) else {
            if gone(&sock) {
                return Ok(());
            }
            std::thread::sleep(FRAME_POLL);
            continue;
        };
        seen = seq;
        write_frame(&mut sock, &frame)?;
        std::thread::sleep(live_period(hub, fps).saturating_sub(started.elapsed()));
    }
}

/// The 3D view's poses and the gripper angles, now, as server-sent events at
/// the live rate.
pub(super) fn serve_live_poses(hub: &Hub, mut sock: TcpStream) -> std::io::Result<()> {
    yield_to_recording();
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nConnection: close\r\nCache-Control: no-store\r\n\
         Content-Type: text/event-stream\r\n\r\n"
    )?;
    loop {
        let started = std::time::Instant::now();
        write!(sock, "data: {}\n\n", live_poses(hub))?;
        std::thread::sleep(live_period(hub, LIVE_FPS).saturating_sub(started.elapsed()));
    }
}

/// The same tracks, names and frames as a recorded take's `view` (see
/// `record::playback::TRACKS`), one sample each, so the viewer draws both with
/// one piece of code: raw XR poses for the shells and the headset, hand roots
/// in `quest_origin`, finger links relative to their hand root. The fingers
/// are left out for a gripper with no calibrated angle, as the recorder does.
fn live_poses(hub: &Hub) -> String {
    use glam::{DQuat, DVec3};
    let poses = hub.poses();
    let sides = hub.sides();
    let mut out = serde_json::Map::new();
    let mut put = |name: String, p: DVec3, q: DQuat| {
        out.insert(name, serde_json::json!([p.x, p.y, p.z, q.x, q.y, q.z, q.w]));
    };
    let split =
        |t: [f64; 7]| (DVec3::new(t[0], t[1], t[2]), DQuat::from_xyzw(t[3], t[4], t[5], t[6]));
    if let Some(h) = poses[2] {
        let (p, q) = split(h);
        put("hmd".into(), p, q);
    }
    // The jaw angle does not need the controller, so a sleeping controller
    // still plots a calibrated angle rather than the raw encoder reading.
    let angles = [0, 1].map(|i| hub.gripper_angle(i, sides[i]));
    for (i, side) in ["left", "right"].iter().enumerate() {
        let Some(t) = poses[i] else { continue };
        let (p, q) = split(t);
        put(format!("{side}_controller"), p, q);
        let links = crate::misc::mount::hand_links(i, (p, q), angles[i].unwrap_or(0.0));
        let root = links[0];
        put(format!("{side}_hand_root"), root.pos, root.rot);
        if angles[i].is_none() {
            continue;
        }
        let back = root.rot.inverse();
        for (finger, link) in [("right", links[2]), ("left", links[4])] {
            put(
                format!("{side}_{finger}_finger_link"),
                back * (link.pos - root.pos),
                back * link.rot,
            );
        }
    }
    // The calibrated angle, else the encoder's reading, as the recorder
    // writes `/yubi/gripper/*/joint_state`.
    let grippers = [0, 1].map(|i| angles[i].or(sides[i].map(f64::from)));
    serde_json::json!({
        "t_ms": crate::misc::clock::realtime_ns() as f64 / 1e6,
        "recording": hub.recording(),
        "poses": out,
        "grippers": grippers,
    })
    .to_string()
}

/// Whether the viewer has hung up, asked without writing anything it would
/// see. A stream with no frames to send never writes, so it would never learn
/// the viewer left and would keep its thread for good. A browser sends nothing
/// after its request, so whatever is waiting is read and dropped: the hang-up
/// is only seen behind it.
fn gone(sock: &TcpStream) -> bool {
    use std::os::fd::AsRawFd;
    let mut buf = [0u8; 4096];
    // Bounded, so a client that never stops sending cannot hold the loop.
    for _ in 0..16 {
        let n = unsafe {
            libc::recv(sock.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT)
        };
        match n {
            0 => return true,
            n if n > 0 => continue,
            _ => {
                return !matches!(
                    std::io::Error::last_os_error().kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                )
            }
        }
    }
    false
}

/// The newest preview for one slot, if it is newer than `seen`.
///
/// `u64::MAX` for the slots this viewer is not watching, so the snapshot clones
/// exactly the one frame that is about to go out.
fn next_frame(hub: &Hub, slot: usize, seen: u64) -> Option<(std::sync::Arc<[u8]>, u64)> {
    let want = crate::hub::USB_CAM_SLOTS.iter().position(|&s| s == slot)?;
    let asking = std::array::from_fn(|i| if i == want { seen } else { u64::MAX });
    hub.ui_snapshot(asking).0[want].clone()
}

fn set_send_buffer(sock: &TcpStream, bytes: libc::c_int) {
    use std::os::fd::AsRawFd;
    unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &bytes as *const _ as *const libc::c_void,
            std::mem::size_of_val(&bytes) as libc::socklen_t,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream with no frames still notices that its viewer left.
    #[test]
    fn a_viewer_that_hung_up_is_noticed_without_a_write() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        assert!(!gone(&server), "still watching");
        // Bytes the server never read must not hide the hang-up behind them.
        (&client).write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
        drop(client);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while !gone(&server) {
            assert!(std::time::Instant::now() < deadline, "the hang-up was never seen");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// The live view draws with the episode viewer's code, so a live sample
    /// has to be what a take records for the same pose.
    #[test]
    fn a_live_sample_is_what_a_take_records() {
        let pose = [0.3, -0.2, 1.1, 0.0, 0.3826834, 0.0, 0.9238795];
        let hub = Hub::default();
        let located = crate::misc::pose::Located::tracked(pose);
        hub.publish_pose(crate::misc::capture::Capture::default(), 0, located);
        hub.publish_pose(crate::misc::capture::Capture::default(), 2, located);
        let live: serde_json::Value = serde_json::from_str(&live_poses(&hub)).unwrap();
        let get = |name: &str| -> Vec<f64> {
            live["poses"][name].as_array().unwrap().iter().map(|v| v.as_f64().unwrap()).collect()
        };
        assert_eq!(get("left_controller"), pose);
        assert_eq!(get("hmd"), pose);
        let root = crate::misc::mount::hand_root_pose(0, pose);
        for (got, want) in get("left_hand_root").iter().zip(root) {
            assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        }
        assert!(live["poses"].get("left_left_finger_link").is_none(), "no calibration, no jaws");
        assert!(live["poses"].get("right_controller").is_none());
        assert_eq!(live["grippers"], serde_json::json!([null, null]), "no encoder, no angle");
    }
}
