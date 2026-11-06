//! When a sample was taken, as the sensor that took it said.
//!
//! Every producer fills one of these — the camera worker, the pose thread,
//! the gripper reader — and the mcap writer is what reads it, which is why it
//! is shared rather than the writer's own: `sensors/` and `view/` both name
//! it, and a type two subtrees name lives here.
//!
//! `at_ns` is the sensor's own capture instant on boottime; `seq` its own
//! ordinal; the UVC pair is the camera's clock verbatim. All optional,
//! because a source that cannot say leaves it blank rather than guessing —
//! the writer falls back to arrival time and says so in `publish_time`.

#[derive(Default, Clone, Copy)]
pub struct Capture {
    pub at_ns: Option<u64>,
    pub seq: Option<u64>,
    pub uvc_pts: Option<u32>,
    pub uvc_scr: Option<u64>,
    pub uvc: Option<UvcFrame>,
}

/// When one UVC frame's first and last packets reached the host, and the
/// stream's health counters when it closed. Fitting the camera's PTS against
/// these reap times is what puts the exposure on the host clock: Horizon OS
/// leaves a URB's `start_frame` at 0 (a Quest 3S, 2026-10-21), so the bus frame the
/// SCR names cannot be matched. The counters are cumulative since the stream
/// opened, as `frame` is.
#[derive(Default, Clone, Copy)]
pub struct UvcFrame {
    pub first: UrbAt,
    pub last: UrbAt,
    pub bytes: u32,
    pub errors: u64,
    pub bad_headers: u64,
    pub damaged_frames: u64,
    pub lost_packets: u64,
    pub queue_drops: u64,
}

/// One isochronous packet's place: the boottime its URB was reaped, and the
/// packet's index within it (each packet is one 125 µs microframe).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct UrbAt {
    pub reaped_ns: u64,
    pub packet: u16,
}
