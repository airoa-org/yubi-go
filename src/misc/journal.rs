//! The app's own log lines, kept for the take being recorded so they land in
//! its mcap as `/rosout`, on the same clock as the data they describe. Logcat
//! alone is gone after a reboot, and it is shared with the whole system.
//!
//! Lines wait here rather than going through the hub: a line can be logged
//! while the hub's state lock is held, and the hub's own queue takes that lock
//! when it is full.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// The writer drains this on every message it receives, which is at least the
/// 120 Hz pose rate during a take; 2000 lines is minutes of the app's normal
/// ~2 lines/s, so it fills only if the writer has stopped.
const CAP: usize = 2000;

pub struct Line {
    pub at_ns: u64,
    pub level: u8,
    pub target: String,
    pub msg: String,
    pub file: String,
    pub line: u32,
}

static KEEP: AtomicBool = AtomicBool::new(false);
static LINES: Mutex<VecDeque<Line>> = Mutex::new(VecDeque::new());

/// Start or stop keeping lines; stopping drops what is waiting.
pub fn keep(on: bool) {
    KEEP.store(on, Ordering::Relaxed);
    if !on {
        LINES.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}

pub fn drain() -> Vec<Line> {
    LINES.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect()
}

/// `rcl_interfaces/msg/Log`'s levels.
fn level(l: log::Level) -> u8 {
    match l {
        log::Level::Error => 40,
        log::Level::Warn => 30,
        log::Level::Info => 20,
        log::Level::Debug | log::Level::Trace => 10,
    }
}

/// Wraps the real logger: every line it would print is also kept.
pub struct Tee<L>(pub L);

impl<L: log::Log> log::Log for Tee<L> {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.0.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        self.0.log(record);
        if !KEEP.load(Ordering::Relaxed) || !self.0.enabled(record.metadata()) {
            return;
        }
        let line = Line {
            at_ns: crate::misc::clock::boottime_ns(),
            level: level(record.level()),
            target: record.target().to_string(),
            msg: record.args().to_string(),
            file: record.file().unwrap_or_default().to_string(),
            line: record.line().unwrap_or(0),
        };
        let mut lines = LINES.lock().unwrap_or_else(|e| e.into_inner());
        if lines.len() >= CAP {
            lines.pop_front();
        }
        lines.push_back(line);
    }

    fn flush(&self) {
        self.0.flush()
    }
}
