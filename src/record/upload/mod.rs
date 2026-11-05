//! Sends finished takes to a bucket, and clears local disk once they're
//! safely landed. `config` reads `storage.yaml` into settings; `s3` is the
//! wire protocol (presigning, single-part and multipart PUT); `episode`
//! orchestrates sending one take and probing the endpoint on top of `s3`;
//! `gc` reclaims local disk; `state` is the upload-state database and what
//! the panel reads off it; `worker` is the upload thread that drives it all.

mod config;
mod episode;
mod gc;
mod s3;
mod state;
mod worker;

pub(crate) use config::GUEST_ROOT;
pub use config::{save, shown, PathRule, StorageSettings, UploadConfig, NO_FILE};
#[cfg(test)]
pub(crate) use state::sending;
pub use state::{depth, state, unless_sending, was_sent, Level, State};
pub use worker::install;
