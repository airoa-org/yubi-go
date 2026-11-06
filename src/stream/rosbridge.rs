//! A rosbridge emulation on `:9090` for a monitoring client: the topics it
//! watches, published from the hub's state, and `/robot_description`.

use glam::{DQuat, DVec3};
use std::collections::HashSet;
use std::net::TcpStream;

use crate::hub::Hub;

const URDF: &str = include_str!("../../assets/yubi.urdf");

/// How often every subscribed topic is published: 10 Hz, chosen by hand for a
/// monitor a person watches.
const PUBLISH_PERIOD: std::time::Duration = std::time::Duration::from_millis(100);

/// How long one read waits for a client message before the publish check runs.
/// Half of [`PUBLISH_PERIOD`], so a publish goes out at most this late.
const READ_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// One websocket client, on its own thread — the accept loop is
/// [`crate::stream::serve`]'s.
pub(crate) fn serve(hub: &'static Hub, sock: TcpStream) {
    let _ = client(hub, sock);
}

/// Largest message or frame a client may send. Every op this server answers
/// is `subscribe` or `unsubscribe` with a topic name, under 200 bytes, so
/// 64 KiB is far past them, and tungstenite's default (64 MiB) is a buffer
/// any client on the network could make the headset allocate.
const MAX_MESSAGE: usize = 64 * 1024;

fn client(hub: &'static Hub, sock: TcpStream) -> Result<(), tungstenite::Error> {
    let config = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE))
        .max_frame_size(Some(MAX_MESSAGE));
    // Only a `Host` that is the headset's, as on `:8080`. `Origin` is not
    // checked: monitoring pages are served from other hosts, and the `:8080`
    // page itself is another origin to `:9090`. The error type is
    // tungstenite's `Callback`'s.
    #[allow(clippy::result_large_err)]
    let same_site = |req: &tungstenite::handshake::server::Request, resp| {
        let header = |name| req.headers().get(name).and_then(|v| v.to_str().ok());
        let host = header("host");
        if super::http::foreign_host(host) {
            log::warn!("rosbridge: refused host {host:?}");
            let mut refused = tungstenite::handshake::server::ErrorResponse::new(None);
            *refused.status_mut() = tungstenite::http::StatusCode::FORBIDDEN;
            return Err(refused);
        }
        Ok(resp)
    };
    let mut ws = tungstenite::accept_hdr_with_config(sock, same_site, Some(config)).map_err(
        |e| match e {
            tungstenite::HandshakeError::Failure(e) => e,
            tungstenite::HandshakeError::Interrupted(_) => {
                tungstenite::Error::Io(std::io::Error::other("handshake interrupted"))
            }
        },
    )?;
    ws.get_ref().set_read_timeout(Some(READ_POLL)).ok();
    let mut subs = HashSet::<String>::new();
    let mut last_pub = std::time::Instant::now();
    loop {
        match ws.read() {
            Ok(msg) if msg.is_text() => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(msg.to_text()?) {
                    let topic = v["topic"].as_str().unwrap_or_default().to_string();
                    match v["op"].as_str() {
                        Some("subscribe") => {
                            if topic == "/robot_description" {
                                let msg = serde_json::json!({
                                    "op": "publish",
                                    "topic": "/robot_description",
                                    "msg": {"data": URDF},
                                });
                                ws.send(tungstenite::Message::text(msg.to_string()))?;
                            } else if message_for(hub, &topic).is_some() {
                                // Only topics this server answers, so a client
                                // naming new ones forever cannot grow the set.
                                subs.insert(topic);
                            }
                        }
                        Some("unsubscribe") => {
                            subs.remove(&topic);
                        }
                        _ => {}
                    }
                }
            }
            Ok(_) => {}
            Err(tungstenite::Error::Io(e))
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => return Err(e),
        }

        if last_pub.elapsed() >= PUBLISH_PERIOD {
            last_pub = std::time::Instant::now();
            for topic in &subs {
                if let Some(msg) = message_for(hub, topic) {
                    ws.send(tungstenite::Message::text(format!(
                        r#"{{"op":"publish","topic":"{topic}","msg":{msg}}}"#
                    )))?;
                }
            }
        }
    }
}

fn message_for(hub: &Hub, topic: &str) -> Option<String> {
    match topic {
        "/record_manager/recording" => Some(format!(r#"{{"data":{}}}"#, hub.recording())),
        "/recording_gate/gate_level" => Some(format!(r#"{{"data":{}}}"#, hub.gate_level())),
        "/task_sequence_manager/episode_elapsed_sec" => {
            Some(format!(r#"{{"data":{:.1}}}"#, hub.task_display().episode_elapsed_sec))
        }
        "/task_sequence_manager/subtask_elapsed_sec" => {
            Some(format!(r#"{{"data":{:.1}}}"#, hub.task_display().subtask_elapsed_sec))
        }
        "/task_sequence_manager/next_task" => {
            Some(serde_json::json!({"data": hub.task_display().next_task}).to_string())
        }
        "/task_sequence_manager/status" => {
            Some(serde_json::json!({"data": hub.task_display().status}).to_string())
        }
        "/tf" | "/tf_static" => {
            let poses = hub.poses();
            let sides = hub.sides();
            let mut transforms = Vec::new();
            let mut push = |name: &str, p: [f64; 3], q: [f64; 4]| {
                transforms.push(format!(
                    r#"{{"header":{{"frame_id":"quest_origin"}},"child_frame_id":"{name}","transform":{{"translation":{{"x":{},"y":{},"z":{}}},"rotation":{{"x":{},"y":{},"z":{},"w":{}}}}}}}"#,
                    p[0], p[1], p[2], q[0], q[1], q[2], q[3]
                ));
            };
            if let Some(h) = poses[2] {
                push("quest_hmd", [h[0], h[1], h[2]], [h[3], h[4], h[5], h[6]]);
            }
            for (i, side) in ["left", "right"].iter().enumerate() {
                let Some(t) = poses[i] else { continue };
                let p = DVec3::new(t[0], t[1], t[2]);
                let q = DQuat::from_xyzw(t[3], t[4], t[5], t[6]);
                push(&format!("quest_{side}_controller"), [p.x, p.y, p.z], [q.x, q.y, q.z, q.w]);
                let angle = hub.gripper_angle(i, sides[i]).unwrap_or(0.0);
                let links = crate::misc::mount::hand_links(i, (p, q), angle);
                for (name, l) in crate::misc::mount::LINK_NAMES.iter().zip(links) {
                    push(
                        &format!("{side}_{name}"),
                        [l.pos.x, l.pos.y, l.pos.z],
                        [l.rot.x, l.rot.y, l.rot.z, l.rot.w],
                    );
                }
            }
            Some(format!(r#"{{"transforms":[{}]}}"#, transforms.join(",")))
        }
        _ => None,
    }
}

#[cfg(test)]
mod contract {
    use super::*;

    const POSE: [f64; 7] = [0.3, -0.2, 1.1, 0.0, 0.3826834, 0.0, 0.9238795];

    fn transform(json: &str, child: &str) -> (DVec3, DQuat) {
        let key = format!(r#""child_frame_id":"{child}""#);
        let start = json.find(&key).unwrap_or_else(|| panic!("no {child} in {json}"));
        let rest = &json[start..];
        let end = rest.find("}}}").expect("truncated transform") + 3;
        let number = |after: &str, from: &str| -> f64 {
            let at = from.find(after).unwrap_or_else(|| panic!("no {after} in {from}"));
            let tail = &from[at + after.len()..];
            let stop = tail.find(|c: char| !matches!(c, '0'..='9' | '-' | '.' | 'e')).unwrap();
            tail[..stop].parse().unwrap()
        };
        let block = &rest[..end];
        let (t, r) = block.split_once("\"rotation\"").unwrap();
        (
            DVec3::new(number("\"x\":", t), number("\"y\":", t), number("\"z\":", t)),
            DQuat::from_xyzw(
                number("\"x\":", r),
                number("\"y\":", r),
                number("\"z\":", r),
                number("\"w\":", r),
            ),
        )
    }

    /// The handshake obeys `:8080`'s rules: another site's page, or a name
    /// that rebinds to the headset, is refused before it can subscribe.
    #[test]
    fn a_handshake_from_another_site_is_refused() {
        use std::io::{Read, Write};
        let hub: &'static Hub = Box::leak(Box::default());
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let at = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for sock in listener.incoming().flatten() {
                std::thread::spawn(move || serve(hub, sock));
            }
        });
        let status = |headers: &str| {
            let mut s = TcpStream::connect(at).unwrap();
            write!(
                s,
                "GET / HTTP/1.1\r\n{headers}Upgrade: websocket\r\nConnection: Upgrade\r\n\
                 Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
            )
            .unwrap();
            let mut head = [0u8; 12];
            s.read_exact(&mut head).unwrap();
            String::from_utf8_lossy(&head[9..]).into_owned()
        };
        assert_eq!(status("Host: 192.0.2.7:9090\r\n"), "101");
        assert_eq!(status("Host: 192.0.2.7:9090\r\nOrigin: http://192.0.2.7:9090\r\n"), "101");
        let monitor = "Host: 192.0.2.7:9090\r\nOrigin: http://monitor.example\r\n";
        assert_eq!(status(monitor), "101", "a monitoring page on another host");
        assert_eq!(
            status("Host: evil.example:9090\r\nOrigin: http://evil.example:9090\r\n"),
            "403"
        );
    }

    #[test]
    fn gate_level_publishes_the_cores_verdict_and_fails_closed() {
        // The level itself is airoa_core's (see airoa_sdk's tests and `record::gate::tests`);
        // what this pins is that the topic carries it, and that a rig with no
        // verdict yet reads HARD_STOP rather than OK.
        let hub = Hub::default();
        let down = message_for(&hub, "/recording_gate/gate_level").unwrap();
        assert_eq!(down, r#"{"data":2}"#, "no verdict yet must not read as OK");

        for slot in [0, 1] {
            hub.on_frame(slot, crate::misc::capture::Capture::default(), b"\xff\xd8\xff\xd9");
        }
        for side in [0, 1] {
            hub.publish_gripper(None, side, "PL004", 1.0);
        }
        let live = message_for(&hub, "/recording_gate/gate_level").unwrap();
        assert_eq!(live, r#"{"data":2}"#, "live sensors alone do not open the gate");
    }

    #[test]
    fn task_topics_are_namespaced() {
        let hub = Hub::default();
        for topic in [
            "/task_sequence_manager/episode_elapsed_sec",
            "/task_sequence_manager/subtask_elapsed_sec",
            "/task_sequence_manager/next_task",
            "/task_sequence_manager/status",
        ] {
            assert!(message_for(&hub, topic).is_some(), "{topic} unanswered");
        }
        for topic in ["/episode_elapsed_sec", "/next_task"] {
            assert!(message_for(&hub, topic).is_none(), "{topic} is not a monitor's topic");
        }
    }

    #[test]
    fn hand_root_is_offset_from_the_controller() {
        let hub = Hub::default();
        hub.publish_pose(
            crate::misc::capture::Capture::default(),
            0,
            crate::misc::pose::Located::tracked(POSE),
        );
        hub.publish_pose(
            crate::misc::capture::Capture::default(),
            1,
            crate::misc::pose::Located::tracked(POSE),
        );
        let tf = message_for(&hub, "/tf").unwrap();

        for (side, name) in [(0usize, "left"), (1, "right")] {
            let (cp, cq) = transform(&tf, &format!("quest_{name}_controller"));
            let (hp, hq) = transform(&tf, &format!("{name}_hand_root"));

            let metres = (hp - cp).length();
            assert!(
                (0.04..0.09).contains(&metres),
                "{name}_hand_root is {metres:.5} m from the controller, want ~0.065"
            );
            let degrees = 2.0 * hq.dot(cq).abs().clamp(0.0, 1.0).acos().to_degrees();
            assert!(degrees > 5.0, "{name}_hand_root is not rotated at all ({degrees:.2} deg)");
            let recorded = crate::misc::mount::hand_root_pose(side, POSE);
            let delta = (DVec3::new(recorded[0], recorded[1], recorded[2]) - hp).length();
            assert!(delta < 1e-9, "{name}: live tf and recorded pose disagree by {delta}");
        }
    }

    #[test]
    fn hand_cam_link_hangs_off_hand_root() {
        const CAMERA_JOINT: DVec3 = DVec3::new(0.0, 0.0, 0.0426);
        let hub = Hub::default();
        hub.publish_pose(
            crate::misc::capture::Capture::default(),
            0,
            crate::misc::pose::Located::tracked(POSE),
        );
        let tf = message_for(&hub, "/tf").unwrap();
        let (cp, cq) = transform(&tf, "quest_left_controller");
        let (hp, hq) = transform(&tf, "left_hand_root");
        let (cam, _) = transform(&tf, "left_hand_cam_link");

        let want = hp + hq * CAMERA_JOINT;
        assert!((cam - want).length() < 1e-9, "cam {cam:?} want {want:?}");
        let off_controller = cp + cq * CAMERA_JOINT;
        assert!(
            (cam - off_controller).length() > 0.05,
            "hand_cam_link is sitting on the controller chain, not hand_root"
        );
    }
}
