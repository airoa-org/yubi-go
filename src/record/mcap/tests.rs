use super::*;

#[test]
fn the_head_camera_is_yubi_gos_own_marker_topic() {
    assert_eq!(CAM_TOPICS[crate::hub::SLOT_HEAD], "/head_camera/image_raw/compressed");
    assert_eq!(CAM_TOPICS[0], "/left_camera/image_raw/compressed");
    assert_eq!(CAM_TOPICS[1], "/right_camera/image_raw/compressed");
}

fn path(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("yubigo_mcap_{name}-{}.mcap", std::process::id()));
    let _ = std::fs::remove_file(&path);
    path
}

const POSE: [f64; 7] = [0.3, -0.2, 1.1, 0.0, 0.3826834, 0.0, 0.9238795];

fn doubles(payload: &[u8]) -> Vec<f64> {
    (4..payload.len().saturating_sub(7))
        .step_by(8)
        .map(|i| f64::from_le_bytes(payload[i..i + 8].try_into().unwrap()))
        .collect()
}

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Reader<'a> {
        assert_eq!(&buf[..4], &[0x00, 0x01, 0x00, 0x00], "not little-endian CDR");
        Reader { buf, at: 4 }
    }
    fn align(&mut self, n: usize) {
        while !(self.at - 4).is_multiple_of(n) {
            assert_eq!(self.buf[self.at], 0, "padding at {} is not zero", self.at);
            self.at += 1;
        }
    }
    fn u32(&mut self) -> u32 {
        self.align(4);
        let v = u32::from_le_bytes(self.buf[self.at..self.at + 4].try_into().unwrap());
        self.at += 4;
        v
    }
    fn i32(&mut self) -> i32 {
        self.u32() as i32
    }
    fn u8(&mut self) -> u8 {
        self.at += 1;
        self.buf[self.at - 1]
    }
    fn u16(&mut self) -> u16 {
        self.align(2);
        self.at += 2;
        u16::from_le_bytes(self.buf[self.at - 2..self.at].try_into().unwrap())
    }
    fn u64(&mut self) -> u64 {
        self.align(8);
        self.at += 8;
        u64::from_le_bytes(self.buf[self.at - 8..self.at].try_into().unwrap())
    }
    fn f32(&mut self) -> f32 {
        f32::from_bits(self.u32())
    }
    fn f64(&mut self) -> f64 {
        self.align(8);
        let v = f64::from_le_bytes(self.buf[self.at..self.at + 8].try_into().unwrap());
        self.at += 8;
        v
    }
    fn string(&mut self) -> String {
        let n = self.u32() as usize;
        let s = String::from_utf8(self.buf[self.at..self.at + n - 1].to_vec()).unwrap();
        assert_eq!(self.buf[self.at + n - 1], 0, "string is not nul-terminated");
        self.at += n;
        s
    }
    fn header(&mut self) -> (u64, String) {
        let (sec, nsec) = (self.i32(), self.u32());
        (sec as u64 * 1_000_000_000 + nsec as u64, self.string())
    }
}

fn side(id: &str, closed: f64) -> crate::misc::calib::Side {
    crate::misc::calib::Side {
        device_id: id.into(),
        closed,
        open: closed + 0.8,
        captured_realtime_ns: 0,
    }
}

fn calibration_mcap(name: &str, sides: [Option<crate::misc::calib::Side>; 2]) -> Vec<u8> {
    let file = path(name);
    let mut out = SignalsMcap::create(&file, [false; 3]).unwrap();
    out.calibration(1_000, &crate::misc::calib::Calibration { sides }).unwrap();
    out.close().unwrap();
    std::fs::read(&file).unwrap()
}

/// The calibration topics in a file. Every mcap also opens with `/tf_static`
/// and the Joy layout, which these tests are not about.
fn topics(data: &[u8]) -> Vec<String> {
    mcap::MessageStream::new(data)
        .unwrap()
        .map(|m| m.unwrap().channel.topic.clone())
        .filter(|t| t != "/tf_static" && t != "/quest/controller/joy_layout")
        .collect()
}

fn payload(data: &[u8], topic: &str) -> Vec<u8> {
    let mut messages = mcap::MessageStream::new(data).unwrap().map(Result::unwrap);
    messages.find(|m| m.channel.topic == topic).unwrap().data.into_owned()
}

#[test]
fn the_calibration_goes_out_once_per_side() {
    let data =
        calibration_mcap("calibration", [Some(side("L-01", 1.25)), Some(side("R-02", -2.5))]);
    let one = |topic| Reader::new(&payload(&data, topic)).f64();
    assert_eq!(one("/yubi/gripper/left/encoder_offset"), 1.25);
    assert_eq!(one("/yubi/gripper/right/encoder_offset"), -2.5);
    let one = |topic| Reader::new(&payload(&data, topic)).string();
    assert_eq!(one("/yubi/gripper/left/id"), "L-01");
    assert_eq!(one("/device_ids"), r#"{"left":"L-01","right":"R-02"}"#);
    assert_eq!(topics(&data).len(), 6);
    let full = Reader::new(&payload(&data, "/yubi_go/calibration")).string();
    let full: serde_json::Value = serde_json::from_str(&full).unwrap();
    assert_eq!(full["left"]["device_id"], "L-01");
    assert!(full["left"]["open_rad"].is_number(), "the open reading says which way it turns");
}

#[test]
fn a_half_calibration_goes_out_for_its_side_only() {
    let data = calibration_mcap("calibration_half", [None, Some(side("R-02", -2.5))]);
    assert_eq!(
        topics(&data),
        [
            "/yubi/gripper/right/encoder_offset",
            "/yubi/gripper/right/id",
            "/device_ids",
            "/yubi_go/calibration"
        ]
    );
    let ids = Reader::new(&payload(&data, "/device_ids")).string();
    assert_eq!(ids, r#"{"left":null,"right":"R-02"}"#);
}

#[test]
fn no_calibration_writes_no_calibration_topics() {
    assert!(topics(&calibration_mcap("calibration_none", [None, None])).is_empty());
}

#[test]
fn calibration_topics_are_offered_as_transient_local() {
    let data = calibration_mcap("calibration_qos", [Some(side("L-01", 1.25)), None]);
    for m in mcap::MessageStream::new(&data).unwrap().map(Result::unwrap) {
        if m.channel.topic == "/tf_static" {
            continue;
        }
        let qos = &m.channel.metadata["offered_qos_profiles"];
        assert!(qos.contains("\n  durability: 1\n"), "{qos}");
    }
}

#[test]
fn a_tf_payload_decodes_field_by_field() {
    let path = path("cdr");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    out.gripper(1_000, None, 1, 1.4, Some(0.3)).unwrap();
    out.pose(2_500, Capture::default(), 1, Located::tracked(POSE)).unwrap();
    out.close().unwrap();

    let msgs = read_all(&path).unwrap();
    let mut r = Reader::new(&msgs.iter().find(|m| m.0 == "/tf").expect("no /tf").2);
    let mut seen = Vec::new();
    for _ in 0..r.u32() {
        let (stamp, parent) = r.header();
        let child = r.string();
        let p: Vec<f64> = (0..7).map(|_| r.f64()).collect();
        assert_eq!(stamp, 2_500, "{child} carries the wrong instant");
        seen.push((parent, child, [p[0], p[1], p[2]]));
    }
    assert_eq!(r.at, r.buf.len(), "trailing bytes: the message is longer than it decodes");

    let edges: Vec<(&str, &str)> = seen.iter().map(|(p, c, _)| (p.as_str(), c.as_str())).collect();
    assert_eq!(
        edges,
        [
            ("quest_origin", "right_hand_root"),
            ("right_hand_root", "right_right_finger_link"),
            ("right_hand_root", "right_left_finger_link"),
        ],
        "/tf is the moving frames only, each under its own parent"
    );
    let want = crate::misc::mount::hand_root_pose(1, POSE);
    assert_eq!(seen[0].2, [want[0], want[1], want[2]]);
    assert_eq!(seen[1].2, [-0.016, -0.015, 0.0], "a finger in hand_root's frame");
    assert_eq!(seen[2].2, [-0.016, 0.015, 0.0]);
}

#[test]
fn the_fixed_mounts_are_latched_once_on_tf_static() {
    let path = path("statics");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    out.pose(2_000, Capture::default(), 0, Located::tracked(POSE)).unwrap();
    out.pose(3_000, Capture::default(), 1, Located::tracked(POSE)).unwrap();
    out.pose(4_000, Capture::default(), 2, Located::tracked(POSE)).unwrap();
    out.close().unwrap();

    let msgs = read_all(&path).unwrap();
    let statics: Vec<_> = msgs.iter().filter(|m| m.0 == "/tf_static").collect();
    assert_eq!(statics.len(), 1, "one latched message, not one per pose");
    let mut r = Reader::new(&statics[0].2);
    let mut edges = Vec::new();
    for _ in 0..r.u32() {
        let (stamp, parent) = r.header();
        assert_eq!(stamp, 2_000, "stamped like the first message");
        edges.push((parent, r.string()));
        for _ in 0..7 {
            r.f64();
        }
    }
    let mut want: Vec<(String, String)> = [
        ("left_hand_root", "left_hand_cam_link"),
        ("left_hand_root", "quest_left_controller"),
        ("left_right_finger_link", "left_right_finger_tip_link"),
        ("left_left_finger_link", "left_left_finger_tip_link"),
        ("right_hand_root", "right_hand_cam_link"),
        ("right_hand_root", "quest_right_controller"),
        ("right_right_finger_link", "right_right_finger_tip_link"),
        ("right_left_finger_link", "right_left_finger_tip_link"),
    ]
    .map(|(p, c)| (p.to_string(), c.to_string()))
    .to_vec();
    want.sort();
    edges.sort();
    assert_eq!(edges, want, "the same eight fixed mounts yubi latches");
    let tf: Vec<u8> = msgs.iter().filter(|m| m.0 == "/tf").flat_map(|m| m.2.clone()).collect();
    let tf = String::from_utf8_lossy(&tf);
    for fixed in ["controller", "cam_link", "tip_link"] {
        assert!(!tf.contains(fixed), "/tf still carries a fixed frame ({fixed})");
    }
}

#[test]
fn writes_a_readable_file_with_cdr_payloads() {
    let path = path("readable");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    out.gripper(1_000, None, 0, 1.4, Some(0.5)).unwrap();
    out.pose(2_000, Capture::default(), 0, Located::tracked(POSE)).unwrap();
    out.frame(3_000, Capture::default(), 0, b"\xff\xd8\xff\xd9").unwrap();
    assert!(out.close().unwrap().is_empty());

    let topics: Vec<String> = read_all(&path).unwrap().into_iter().map(|m| m.0).collect();
    assert!(topics.contains(&"/tf".to_string()), "{topics:?}");
    assert!(topics.contains(&"/joint_states".to_string()), "{topics:?}");
    assert!(topics.contains(&"/yubi/gripper/left/joint_state".to_string()), "{topics:?}");
    assert!(topics.contains(&"/quest/controller/left/tracked".to_string()), "{topics:?}");
    assert!(topics.contains(&"/quest/controller/left/valid".to_string()), "{topics:?}");
    assert!(!topics.contains(&"/quest/hmd/valid".to_string()), "the hmd has no valid, as on yubi");
    assert!(topics.contains(&"/left_camera/image_raw/compressed".to_string()), "{topics:?}");
}

#[test]
fn a_half_lost_pose_keeps_the_other_half_and_its_velocity() {
    use crate::misc::pose::{ComponentTracking, Velocity};
    let path = path("raw_pose");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    let (_, o) = crate::misc::pose::components(POSE);
    let located = Located {
        velocity: Velocity { linear: Some([0.1, 0.2, 0.3]), angular: None },
        ..Located::new(ComponentTracking::Lost, ComponentTracking::Inferred(o))
    };
    out.pose(1_000, Capture { at_ns: Some(900), ..Capture::default() }, 1, located).unwrap();
    out.pose(2_000, Capture::default(), 1, Located::LOST).unwrap();
    out.close().unwrap();
    let msgs = read_all(&path).unwrap();
    let on = |t: &str| msgs.iter().filter(|m| m.0 == t).map(|m| m.2.clone()).collect::<Vec<_>>();

    let poses = on("/quest/controller/right/pose");
    assert_eq!(poses.len(), 1, "a tick with nothing valid writes no pose");
    let mut r = Reader::new(&poses[0]);
    assert_eq!(r.header(), (900, "quest_origin".to_string()));
    assert!((0..3).all(|_| r.f64().is_nan()), "the lost position is NaN");
    assert_eq!([r.f64(), r.f64(), r.f64(), r.f64()], o);
    assert_eq!(r.at, r.buf.len());

    let twists = on("/quest/controller/right/velocity");
    assert_eq!(twists.len(), 1);
    let mut r = Reader::new(&twists[0]);
    r.header();
    assert_eq!([r.f64(), r.f64(), r.f64()], [0.1, 0.2, 0.3]);
    assert!((0..3).all(|_| r.f64().is_nan()), "the invalid angular half is NaN");
    assert!(on("/tf").is_empty(), "tf still needs the whole pose");
}

#[test]
fn every_frame_carries_its_uvc_clocks_bus_position_and_health() {
    use crate::misc::capture::{UrbAt, UvcFrame};
    let path = path("uvc_frame");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    let uvc = UvcFrame {
        first: UrbAt { reaped_ns: 5_000_000_100, packet: 31 },
        last: UrbAt { reaped_ns: 5_000_000_900, packet: 4 },
        bytes: 4,
        errors: 1,
        bad_headers: 2,
        damaged_frames: 3,
        lost_packets: 4,
        queue_drops: 5,
    };
    let capture = Capture {
        seq: Some(77),
        uvc_pts: Some(0xdead_beef),
        uvc_scr: None,
        uvc: Some(uvc),
        ..Capture::default()
    };
    out.frame(6_000_000_000, capture, 2, b"\xff\xd8\xff\xd9").unwrap();
    out.close().unwrap();
    let msgs = read_all(&path).unwrap();
    let m = msgs.iter().find(|m| m.0 == "/head_camera/uvc_frame").expect("no uvc_frame");
    let mut r = Reader::new(&m.2);
    assert_eq!(r.header(), (6_000_000_000, "quest_hmd".to_string()), "the image's own header");
    assert_eq!(r.u64(), 77);
    assert_eq!((r.u8(), r.u32()), (1, 0xdead_beef));
    assert_eq!((r.u8(), r.u64()), (0, 0), "no SCR, and it says so");
    assert_eq!((r.i32(), r.u32(), r.u16()), (5, 100, 31));
    assert_eq!((r.i32(), r.u32(), r.u16()), (5, 900, 4));
    assert_eq!(r.u32(), 4);
    assert_eq!([r.u64(), r.u64(), r.u64(), r.u64(), r.u64()], [1, 2, 3, 4, 5]);
    assert_eq!(r.at, r.buf.len());
}

#[test]
fn inputs_status_events_and_log_lines_decode() {
    use crate::misc::status::{self, Status};
    let path = path("extras");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    out.joy(1_000, 0, &[0.5, 0.0, -1.0, 0.25], &[1, 0, 0, 0, 1, 0, 0, 0, 1]).unwrap();
    let s =
        Status { level: status::WARN, hardware_id: "1-1.2".into(), ..Status::new("camera/left") }
            .with("fps", "11.9");
    out.status(2_000, false, &[s]).unwrap();
    out.status(3_000, true, &[Status::new("fault/left wrist cam").with("state", "open")]).unwrap();
    let line = crate::misc::journal::Line {
        at_ns: 0,
        level: 30,
        target: "yubi_go::sensors".into(),
        msg: "stalled".into(),
        file: "src/sensors/worker.rs".into(),
        line: 9,
    };
    out.log(4_000, &line).unwrap();
    let change = |name, value, at_ns| crate::hub::InputChange { name, value, at_ns };
    let changes = [change("trigger", 0.75, 4_100), change("lower_click", 1.0, 4_200)];
    out.input_changes(5_000, 1, &changes).unwrap();
    out.close().unwrap();
    let msgs = read_all(&path).unwrap();
    let one =
        |t: &str| msgs.iter().find(|m| m.0 == t).unwrap_or_else(|| panic!("no {t}")).2.clone();

    let joy = one("/quest/controller/left/joy");
    let mut r = Reader::new(&joy);
    r.header();
    assert_eq!(r.u32(), 4);
    assert_eq!([r.f32(), r.f32(), r.f32(), r.f32()], [0.5, 0.0, -1.0, 0.25]);
    assert_eq!(r.u32(), 9);
    assert_eq!((0..9).map(|_| r.i32()).collect::<Vec<_>>(), [1, 0, 0, 0, 1, 0, 0, 0, 1]);

    let diagnostics = one("/diagnostics");
    let mut r = Reader::new(&diagnostics);
    r.header();
    assert_eq!(r.u32(), 1, "one status");
    assert_eq!(r.u8(), status::WARN);
    assert_eq!(
        (r.string(), r.string(), r.string()),
        ("camera/left".into(), "".into(), "1-1.2".into())
    );
    assert_eq!(r.u32(), 1);
    assert_eq!((r.string(), r.string()), ("fps".into(), "11.9".into()));
    assert_eq!(r.at, r.buf.len());

    let events = one("/yubi_go/events");
    let mut r = Reader::new(&events);
    r.header();
    r.u32();
    r.u8();
    assert_eq!(r.string(), "fault/left wrist cam");

    let rosout = one("/rosout");
    let mut r = Reader::new(&rosout);
    assert_eq!((r.i32(), r.u32()), (0, 4_000));
    assert_eq!(r.u8(), 30);
    assert_eq!((r.string(), r.string()), ("yubi_go::sensors".into(), "stalled".into()));
    assert_eq!((r.string(), r.string(), r.u32()), ("src/sensors/worker.rs".into(), "".into(), 9));

    let changes = one("/quest/controller/right/input_changes");
    let mut r = Reader::new(&changes);
    assert_eq!(r.header(), (5_000, String::new()));
    assert_eq!((r.u32(), r.string(), r.string()), (2, "trigger".into(), "lower_click".into()));
    assert_eq!((r.u32(), r.f32(), r.f32()), (2, 0.75, 1.0));
    assert_eq!(r.u32(), 2);
    assert_eq!([(r.i32(), r.u32()), (r.i32(), r.u32())], [(0, 4_100), (0, 4_200)]);
    assert_eq!(r.at, r.buf.len());

    let layout = one("/quest/controller/joy_layout");
    let layout: serde_json::Value = serde_json::from_str(&Reader::new(&layout).string()).unwrap();
    assert_eq!(layout["buttons"].as_array().unwrap().len(), 9);
}

#[test]
fn the_raw_encoder_reading_and_the_clock_anchor_are_kept() {
    let path = path("raw_encoder");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    out.clock(5, 1_000_000_005).unwrap();
    out.gripper(1_000, Some(3), 1, 4.0, Some(0.5)).unwrap();
    out.close().unwrap();
    let msgs = read_all(&path).unwrap();
    let raw = &msgs.iter().find(|m| m.0 == "/yubi_go/gripper/right/raw").expect("no raw").2;
    let mut r = Reader::new(raw);
    r.header();
    assert_eq!((r.u32(), r.string()), (1, "right_right_finger_joint".to_string()));
    assert_eq!((r.u32(), r.f64()), (1, 4.0), "the reading, not the calibrated 0.5");

    let data = std::fs::read(&path).unwrap();
    let summary = mcap::Summary::read(&data).unwrap().unwrap();
    let index = summary.metadata_indexes.iter().find(|m| m.name == "clock").expect("no clock");
    let clock = mcap::read::metadata(&data, index).unwrap();
    assert_eq!(clock.metadata["boottime_ns"], "5");
    assert_eq!(clock.metadata["realtime_ns"], "1000000005");
}

#[test]
fn hand_root_is_the_mounted_pose_not_the_controller() {
    let path = path("hand_root");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    out.gripper(1_000, None, 0, 1.4, Some(0.3)).unwrap();
    out.pose(2_000, Capture::default(), 0, Located::tracked(POSE)).unwrap();
    out.close().unwrap();

    let msgs = read_all(&path).unwrap();
    let tf = &msgs.iter().find(|m| m.0 == "/tf").expect("no /tf").2;
    let found = doubles(tf);
    assert!(
        !found.windows(3).any(|w| w == [POSE[0], POSE[1], POSE[2]]),
        "the raw controller pose is on /tf: hand_root was never mounted"
    );
    let want = crate::misc::mount::hand_root_pose(0, POSE);
    assert!(
        found.windows(3).any(|w| w == [want[0], want[1], want[2]]),
        "hand_root is not mount::hand_root_pose"
    );
}

#[test]
fn an_uncalibrated_gripper_writes_no_finger_frames() {
    let count = |calib: Option<f64>, name: &str| {
        let path = path(name);
        let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
        out.gripper(1_000, None, 0, 1.4, calib).unwrap();
        out.pose(2_000, Capture::default(), 0, Located::tracked(POSE)).unwrap();
        out.close().unwrap();
        let msgs = read_all(&path).unwrap();
        let tf = msgs.iter().find(|m| m.0 == "/tf").expect("no /tf").2.clone();
        u32::from_le_bytes(tf[4..8].try_into().unwrap())
    };
    assert_eq!(count(Some(0.3), "calibrated"), 3, "hand_root + two finger links");
    assert_eq!(count(None, "uncalibrated"), 1, "hand_root alone");
}

#[test]
fn the_finger_frames_follow_the_calibrated_angle() {
    let tips = |calib: f64, name: &str| {
        let path = path(name);
        let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
        out.gripper(1_000, None, 0, 1.4, Some(calib)).unwrap();
        out.pose(2_000, Capture::default(), 0, Located::tracked(POSE)).unwrap();
        out.close().unwrap();
        let msgs = read_all(&path).unwrap();
        doubles(&msgs.iter().find(|m| m.0 == "/tf").expect("no /tf").2)
    };
    let (shut, open) = (tips(0.0, "shut"), tips(0.9, "open"));
    assert_ne!(shut, open, "the finger frames ignored the gripper angle");
}

#[test]
fn each_side_is_published_alone_and_together() {
    let path = path("gripper");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    out.gripper(1_000, Some(7), 0, 1.53, Some(0.42)).unwrap();
    out.gripper(2_000, Some(8), 1, 0.91, Some(0.11)).unwrap();
    out.close().unwrap();

    let msgs = read_all(&path).unwrap();
    let state = |topic: &str| {
        let mut r = Reader::new(&msgs.iter().rev().find(|m| m.0 == topic).unwrap().2);
        r.header();
        let names: Vec<String> = (0..r.u32()).map(|_| r.string()).collect();
        let positions: Vec<f64> = (0..r.u32()).map(|_| r.f64()).collect();
        assert_eq!(r.u32(), 0, "velocity");
        assert_eq!(r.u32(), 0, "effort");
        assert_eq!(r.at, r.buf.len(), "trailing bytes in {topic}");
        names.into_iter().zip(positions).collect::<Vec<_>>()
    };
    assert_eq!(
        state("/yubi/gripper/left/joint_state"),
        [("left_right_finger_joint".to_string(), 0.42)]
    );
    assert_eq!(
        state("/yubi/gripper/right/joint_state"),
        [("right_right_finger_joint".to_string(), 0.11)]
    );
    assert_eq!(
        state("/joint_states"),
        [
            ("left_right_finger_joint".to_string(), 0.42),
            ("right_right_finger_joint".to_string(), 0.11),
        ]
    );
}

#[test]
fn a_compressed_image_decodes_as_a_jpeg_in_its_own_frame() {
    let path = path("image");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    let jpeg = b"\xff\xd8not really a picture\xff\xd9";
    out.frame(1_000, Capture { at_ns: Some(900), ..Capture::default() }, 1, jpeg).unwrap();
    out.close().unwrap();

    let msgs = read_all(&path).unwrap();
    let mut r = Reader::new(&msgs.iter().find(|m| m.0.ends_with("/compressed")).unwrap().2);
    let (stamp, frame_id) = r.header();
    assert_eq!(stamp, 900, "the header carries the capture instant, not the arrival");
    assert_eq!(frame_id, "right_hand_cam_link", "the image is not in its camera's frame");
    assert_eq!(r.string(), "jpeg");
    let n = r.u32() as usize;
    assert_eq!(&r.buf[r.at..r.at + n], jpeg, "the bytes are not the camera's");
    assert_eq!(r.at + n, r.buf.len(), "trailing bytes after the image");
}

#[test]
fn an_h264_frame_decodes_as_compressed_video_with_no_image_topic_beside_it() {
    let path = path("video");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    let h264 = [0, 0, 0, 1, 0x67, 0x42, 0, 0, 0, 1, 0x65, 0x88];
    out.video(1_000, Capture { at_ns: Some(1_500_000_900), ..Capture::default() }, 2, &h264)
        .unwrap();
    out.close().unwrap();

    let data = std::fs::read(&path).unwrap();
    assert_eq!(topics(&data), ["/head_camera/video", "/head_camera/uvc_frame"]);
    let msgs = read_all(&path).unwrap();
    let mut r = Reader::new(&msgs[msgs.len() - 2].2);
    assert_eq!((r.i32(), r.u32()), (1, 500_000_900), "the capture instant");
    assert_eq!(r.string(), "quest_hmd");
    let n = r.u32() as usize;
    assert_eq!(&r.buf[r.at..r.at + n], h264);
    r.at += n;
    assert_eq!(r.string(), "h264");
    assert_eq!(r.at, r.buf.len());
}

#[test]
fn a_source_that_goes_backwards_fails_the_close_check() {
    let path = path("backwards");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    out.frame(2_000, Capture::default(), 0, b"\xff\xd8\xff\xd9").unwrap();
    out.frame(1_000, Capture::default(), 0, b"\xff\xd8\xff\xd9").unwrap();
    let bad = out.close().unwrap();
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("left camera"), "{bad:?}");
}

#[test]
fn two_cameras_interleaving_is_not_a_fault() {
    let path = path("interleaved");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    out.frame(2_000, Capture::default(), 0, b"\xff\xd8\xff\xd9").unwrap();
    out.frame(1_000, Capture::default(), 1, b"\xff\xd8\xff\xd9").unwrap();
    assert!(out.close().unwrap().is_empty());
}

#[test]
fn a_camera_that_owed_frames_and_wrote_none_fails_the_close_check() {
    let path = path("owed");
    let mut out = SignalsMcap::create(&path, [true, false, false]).unwrap();
    out.pose(1_000, Capture::default(), 2, Located::tracked(POSE)).unwrap();
    let bad = out.close().unwrap();
    assert_eq!(bad.len(), 1, "{bad:?}");
    assert!(bad[0].contains("left"), "{bad:?}");
}

#[test]
fn capture_and_arrival_are_both_recorded() {
    let path = path("stamps");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    let capture = Capture { at_ns: Some(900), seq: Some(11), ..Capture::default() };
    out.frame(1_000, capture, 0, b"\xff\xd8\xff\xd9").unwrap();
    out.close().unwrap();

    let data = std::fs::read(&path).unwrap();
    let m = mcap::MessageStream::new(&data).unwrap().next().unwrap().unwrap();
    assert_eq!(m.log_time, 1_000, "log_time is when the app had it");
    assert_eq!(m.publish_time, 900, "publish_time is when the sensor took it");
    assert_eq!(m.sequence, 11, "the source's own ordinal, not ours");
}

#[test]
fn tracking_is_written_every_tick_so_a_gap_reads_as_a_gap() {
    let path = path("tracked");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    for (i, tracked) in [true, true, true, false, false, true].into_iter().enumerate() {
        let seen = if tracked { Located::tracked(POSE) } else { Located::inferred(POSE) };
        out.pose(1_000 + i as u64, Capture::default(), 2, seen).unwrap();
    }
    out.close().unwrap();
    let msgs = read_all(&path).unwrap();
    let flags: Vec<bool> =
        msgs.iter().filter(|m| m.0 == "/quest/hmd/tracked").map(|m| m.2[4] != 0).collect();
    assert_eq!(flags, [true, true, true, false, false, true]);
}

#[test]
fn a_lost_tick_says_so_on_tracked_and_valid_without_a_pose() {
    let path = path("lost");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    out.pose(1_000, Capture::default(), 0, Located::inferred(POSE)).unwrap();
    out.pose(2_000, Capture::default(), 0, Located::LOST).unwrap();
    out.pose(3_000, Capture::default(), 2, Located::LOST).unwrap();
    out.close().unwrap();
    let msgs = read_all(&path).unwrap();
    let flags = |topic: &str| -> Vec<bool> {
        msgs.iter().filter(|m| m.0 == topic).map(|m| m.2[4] != 0).collect()
    };
    assert_eq!(flags("/quest/controller/left/tracked"), [false, false]);
    assert_eq!(
        flags("/quest/controller/left/valid"),
        [true, false],
        "a pose is valid even untracked"
    );
    assert_eq!(flags("/quest/hmd/tracked"), [false]);
    assert!(flags("/quest/hmd/valid").is_empty(), "the hmd has no valid topic, as on yubi");
    assert_eq!(msgs.iter().filter(|m| m.0 == "/tf").count(), 1, "a lost tick writes no tf");
}

#[test]
fn position_and_orientation_are_flagged_apart() {
    use crate::misc::pose::ComponentTracking;
    let path = path("components");
    let mut out = SignalsMcap::create(&path, [false; 3]).unwrap();
    let (p, o) = crate::misc::pose::components(POSE);
    out.pose(
        1_000,
        Capture::default(),
        0,
        Located::new(ComponentTracking::Inferred(p), ComponentTracking::Tracked(o)),
    )
    .unwrap();
    out.pose(
        2_000,
        Capture::default(),
        0,
        Located::new(ComponentTracking::Lost, ComponentTracking::Tracked(o)),
    )
    .unwrap();
    out.pose(3_000, Capture::default(), 2, Located::tracked(POSE)).unwrap();
    out.close().unwrap();
    let msgs = read_all(&path).unwrap();
    let flags = |topic: &str| -> Vec<bool> {
        msgs.iter().filter(|m| m.0 == topic).map(|m| m.2[4] != 0).collect()
    };
    assert_eq!(flags("/quest/controller/left/tracked"), [false, false]);
    assert_eq!(flags("/quest/controller/left/valid"), [true, false]);
    assert_eq!(flags("/quest/controller/left/position/valid"), [true, false]);
    assert_eq!(flags("/quest/controller/left/position/tracked"), [false, false]);
    assert_eq!(flags("/quest/controller/left/orientation/valid"), [true, true]);
    assert_eq!(flags("/quest/controller/left/orientation/tracked"), [true, true]);
    assert_eq!(flags("/quest/hmd/position/valid"), [true]);
    assert_eq!(flags("/quest/hmd/orientation/tracked"), [true]);
    assert!(flags("/quest/hmd/valid").is_empty(), "yubi's hmd has no combined valid");
    assert_eq!(msgs.iter().filter(|m| m.0 == "/tf").count(), 2, "no tf without a position");
}

mod golden {
    use super::*;

    #[test]
    fn payload_bytes_are_pinned() {
        let dir = std::env::temp_dir().join(format!("yubigo_golden-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("g.mcap");
        let mut s = SignalsMcap::create(&path, [false; 3]).unwrap();
        let hmd = Located::tracked([1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 1.0]);
        s.pose(1_500_000_000, Capture::default(), 2, hmd).unwrap();
        s.gripper(2_000_000_000, Some(7), 0, 1.25, Some(0.5)).unwrap();
        s.frame(3_000_000_000, Capture::default(), 0, b"\xff\xd8XY\xff\xd9").unwrap();
        s.close().unwrap();
        let msgs = read_all(&path).unwrap();
        let hex = |t: &str| {
            msgs.iter()
                .find(|m| m.0 == t)
                .map(|m| m.2.iter().map(|b| format!("{b:02x}")).collect::<String>())
                .unwrap()
        };
        assert_eq!(hex("/tf"), "0001000001000000010000000065cd1d0d00000071756573745f6f726967696e000000000a00000071756573745f686d64000000000000000000f03f00000000000000400000000000000840000000000000000000000000000000000000000000000000000000000000f03f");
        assert_eq!(hex("/yubi/gripper/left/joint_state"), "000100000200000000000000010000000000000001000000180000006c6566745f72696768745f66696e6765725f6a6f696e74000100000000000000000000000000e03f0000000000000000");
        assert_eq!(hex("/quest/hmd/tracked"), "0001000001");
        assert_eq!(hex("/left_camera/image_raw/compressed"), "000100000300000000000000130000006c6566745f68616e645f63616d5f6c696e6b0000050000006a7065670000000006000000ffd85859ffd9");
    }
}
