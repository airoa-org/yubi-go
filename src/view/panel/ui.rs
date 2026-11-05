//! The panel's content: a view-model of plain words and numbers, turned into
//! SVG fragments for the template in `assets/ui/panel.svg` and rendered by
//! [`super::svg`]. Layout that repeats or measures lives here; everything
//! static is the template's.

mod band;
mod cells;
mod centre;
mod status;

use super::paint::*;
use super::svg::layout;
use band::band;
use cells::cell;
use centre::{calib_rows, refusal_rows};
use reactive_svg::{svg, Element};
use status::{hints, id, net, storage};

use crate::hub::upload::Level;
pub use crate::hub::upload::State as Upload;
use crate::hub::upload::NO_FILE;
pub use crate::misc::power::{Battery, Eta};
use std::time::Duration;

pub const W: usize = 1024;
pub const CELL_W: usize = THUMB_W;
pub const CELL_H: usize = CELL_LABEL_H + THUMB_H + CELL_RATE_H;
pub const CELL_ORIGIN: [(usize, usize); 3] = [(0, 0), (HALF, 0), (0, CELL_H)];
pub const CAMS_H: usize = 2 * CELL_H;
pub const CENTRE_Y: usize = CAMS_H;
pub const CENTRE_H: usize = 260;
pub const BAND_Y: usize = CAMS_H + CENTRE_H;
pub const BAND_H: usize = 292;
pub const TEX_H: usize = CAMS_H + CENTRE_H + BAND_H;
#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub const PANEL_TASK: (usize, usize, usize, usize) = (0, BAND_Y, W, BAND_TASK_H);
pub const PANEL_STORAGE: (usize, usize, usize, usize) = (0, BAND_Y + BAND_TASK_H + 4, 260, 114);
pub const PANEL_ID: (usize, usize, usize, usize) = (272, BAND_Y + BAND_TASK_H + 4, 272, 114);
pub const PANEL_NET: (usize, usize, usize, usize) = (556, BAND_Y + BAND_TASK_H + 4, 468, 114);
pub const PANEL_QR: (usize, usize, usize, usize) = (208, 0, CAMS_H, CAMS_H);
pub const PANEL_HINTS: (usize, usize, usize, usize) = (0, BAND_Y + 254, W, 38);
pub const HALF: usize = 512;
pub(super) const THUMB_W: usize = 448;
pub(super) const THUMB_H: usize = 252;
pub(super) const CELL_LABEL_H: usize = 26;
const CELL_RATE_H: usize = 26;
pub(crate) const BAND_TASK_H: usize = 126;
const BAND_X: f32 = 32.0;
const NET_X: f32 = 576.0;
pub(super) const ROW_H: f32 = 22.0;
pub(super) const BASE: f32 = 16.0;

pub(super) const ACCENT: &str = "#40c8e0";
pub(super) const IDLE: &str = "#9299aa";
pub(super) const PRIMARY: &str = "#e2e8f2";
const ALERT: &str = "#eb3c3c";
const WARN: &str = "#e0a830";
pub(super) const SCRIM: &str = "#0c0e14";
pub(super) const TRACK: &str = "#2c3240";

pub fn cell_label(cell: usize) -> &'static str {
    match cell {
        0 => "Left wrist",
        1 => "Right wrist",
        _ => "Head",
    }
}

// --- view-model ------------------------------------------------------------

#[derive(Clone, Copy, Default)]
pub struct Cam {
    pub width: u32,
    pub height: u32,
    pub hz: f32,
    pub nominal: f32,
    pub live: bool,
    pub starved: bool,
    /// Bytes still flowing while no frames arrive — the stalled wrist.
    pub bytes_flowing: bool,
}

#[derive(Clone, Copy, Default)]
pub struct Pose {
    pub hz: f32,
    pub live: bool,
    pub extrapolated: bool,
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Clone, Copy)]
pub enum Jaw {
    Stopped,
    Uncalibrated,
    Open(f32),
}

#[derive(Clone, Copy)]
pub struct Disk {
    pub free: u64,
    pub used: u64,
    /// How long the free space records for, or `None` at the floor.
    pub recording_left: Option<Duration>,
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
pub enum Stage {
    Uploaded,
    Recording,
    Waiting,
}

pub struct Episode {
    pub name: String,
    pub at: String,
    pub task: String,
    pub bytes: u64,
    pub secs: Option<u64>,
    pub stage: Stage,
}

#[derive(Clone, Copy, Default)]
pub struct Buttons {
    pub recording: bool,
    pub markable: bool,
}

pub struct Calib<'a> {
    /// The step the operator is on, or `None` when the sweep failed.
    pub prompt: Option<&'a str>,
    pub held: [f32; 2],
    pub messages: &'a [String],
}

pub struct State<'a> {
    pub cams: [Cam; 3],
    pub thumbs: [Option<&'a [u8]>; 3],
    pub rec_failed: bool,
    pub buttons: Buttons,
    pub task: Option<&'a str>,
    pub step: Option<&'a str>,
    pub completed: bool,
    pub jaws: [Jaw; 2],
    pub encoders: [Pose; 2],
    pub controllers: [Pose; 2],
    pub hmd: Pose,
    pub battery: Option<Battery>,
    pub disk: Option<Disk>,
    pub production: bool,
    /// Minutes since the backend stopped answering, `None` while it answers.
    pub offline_min: Option<u64>,
    pub unbound: usize,
    pub upload: Upload,
    pub episodes_on_disk: usize,
    pub queued: usize,
    pub robot_id: &'a str,
    pub address: Option<&'a str>,
    /// Newest first; the panel draws at most three.
    pub episodes: &'a [Episode],
    pub calib: Option<Calib<'a>>,
    pub refused: &'a [String],
    /// The operator holds either trigger, so the monitor QR covers the
    /// cells: the code for `address`, or a "no network" page without one.
    pub qr: bool,
}

// --- words -----------------------------------------------------------------

fn gb(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    match gb < 10.0 {
        true => format!("{gb:.1}GB"),
        false => format!("{gb:.0}GB"),
    }
}

fn span(d: Duration) -> String {
    let mins = d.as_secs() / 60;
    match (mins / 60, mins % 60) {
        (0, 0) => "<1 min".to_string(),
        (0, m) => format!("{m} min"),
        (h, m) if h < 10 => format!("{h}h {m:02}m"),
        (h, _) => format!("{}h", h.min(99)),
    }
}

/// Minutes, after a space.
fn ago(mins: u64) -> String {
    match mins {
        0 => String::new(),
        n => format!(" {n} min"),
    }
}

impl Upload {
    fn word(&self, queued: usize) -> String {
        match self {
            Upload::Sending { done, total } => {
                format!("Sending {}/{total}", done + 1)
            }
            _ if queued > 0 => format!("Queued {queued}"),
            Upload::NoTarget(why) if why == NO_FILE => "No upload".into(),
            Upload::NoTarget(_) => "No target".into(),
            Upload::Idle(_) => "All sent".into(),
            Upload::Unreachable(_) => "Unreachable".into(),
            Upload::Retrying(_) => "Retrying".into(),
        }
    }
}

// --- the panel -------------------------------------------------------------

pub struct Ui {
    root: reactive_svg::Root,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            root: reactive_svg::Root::new(
                W,
                TEX_H,
                super::svg::head(),
                super::svg::fonts().clone(),
            ),
        }
    }
}

impl Ui {
    /// What changed since the last draw.
    pub fn draw(&mut self, s: &State) -> Vec<reactive_svg::Patch> {
        self.root.render(elements(s))
    }
}

fn elements<'a>(s: &State<'a>) -> Vec<Element<'a>> {
    let mut out = Vec::new();
    if !s.qr {
        for n in 0..3 {
            cell(&mut out, s, n);
        }
    }
    if s.rec_failed {
        out.push(svg("rec_failed", layout("rec_failed")));
    }

    // The centre surface: a running sweep beats the refusal, because it is
    // one of the reasons.
    let centre = if let Some(calib) = &s.calib {
        Some(layout("centre_back").to_string() + &calib_rows(calib))
    } else if !s.refused.is_empty() {
        Some(layout("refusal").to_string() + &refusal_rows(s.refused))
    } else {
        None
    };
    out.extend(centre.map(|c| svg("centre", c)));

    out.push(svg("band", layout("band")));
    if s.buttons.recording {
        let label = text(926.0, 879.0, ALERT, "REC");
        out.push(svg("rec", layout("rec").to_string() + &label));
    }
    out.push(svg("task", band(s)));
    out.push(svg("storage", storage(s)));
    out.push(svg("net", net(s)));
    out.push(svg("id", id(s)));
    out.push(svg("hints", hints(s)));
    if s.qr {
        out.push(svg("qr", qr_page(s.address)));
    }
    out
}
