//! What more than one subtree needs, which principle 1 of `src/README.md`
//! forces up to here.
//!
//! Not a place for anything that would not fit elsewhere: a module belongs
//! here only when two subtrees share it. `clock` and `save` are
//! used by all of them; `config`, `calib`, `mount`, `disk` and `power` are
//! facts about this rig rather than about any one take; `capture` is the
//! timestamps every sample carries; `usb` is which devices the scan opened;
//! `core` is whether airoa_core is running; `pose` is what the tracking
//! runtime said about a part on one tick, which the recorder and the
//! display both read; `status` is a part's health as
//! the sensors report it and the recorder writes it; `journal` is the app's
//! log lines, which the logger fills and the recorder writes; `speech` is the
//! task names made into audio, which the headset says and `:8080` serves.

pub mod calib;
pub mod capture;
pub mod clock;
pub mod config;
pub mod core;
pub mod disk;
pub mod journal;
pub mod mount;
pub mod pose;
pub mod power;
pub mod save;
pub mod speech;
pub mod status;
pub mod usb;
