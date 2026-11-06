mod paint;
#[cfg(test)]
mod preview;
mod svg;
#[cfg(target_os = "android")]
mod thumb;
mod ui;

#[cfg(target_os = "android")]
pub use thumb::thumbnail;
#[cfg_attr(not(target_os = "android"), allow(unused_imports))]
pub use ui::{
    Buttons, Calib, Cam, Disk, Episode, Jaw, Pose, Stage, State, Ui, CELL_H, CELL_ORIGIN, CELL_W,
    CENTRE_H, CENTRE_Y, PANEL_HINTS, PANEL_ID, PANEL_NET, PANEL_QR, PANEL_STORAGE, PANEL_TASK,
    TEX_H, W,
};
