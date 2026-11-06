mod uvc;

pub use uvc::{
    find_controls, find_mjpeg_stream, Control, FrameCounter, StallWatch,
    MIN_ISO_BYTES_PER_MICROFRAME, STALL_TIMEOUT, TARGET_INTERVAL,
};

#[cfg(target_os = "android")]
mod usbfs;
#[cfg(target_os = "android")]
pub use usbfs::{ControlHandle, Device, Packet};
#[cfg(target_os = "android")]
pub use uvc::{probe_commit, read_control, write_control};
