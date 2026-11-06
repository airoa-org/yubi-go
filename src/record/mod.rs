//! One take, from the input that starts it to the bucket it lands in.
//!
//! `record` owns the episode directory, `writer` the thread that fills it,
//! `mcap` the format, `click` the gesture that triggers it, `task` the FSM
//! that sequences it, `gate` whether it may start at all, `system_info` what
//! made it, `backend/` what the AIRoA backend is told about it (production
//! mode only), `upload/` how it leaves the device, `video` the H.264
//! encoders, and `camera_calib` each camera's lens, mount and delay.

pub mod backend;
pub mod camera_calib;
pub mod click;
pub mod export;
pub mod gate;
pub mod mcap;
pub mod mp4;
pub mod playback;
#[allow(clippy::module_inception)]
pub mod record;
pub mod still;
pub(crate) mod system_info;
pub(crate) mod task;
pub mod upload;
pub mod video;
pub(crate) mod writer;
