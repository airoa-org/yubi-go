//! What this app says to, and asks of, the AIRoA backend (production mode only).
//!
//! Two conversations that share one backlog. `episode` speaks when a take
//! starts, ends or is abandoned; `poll` speaks on a timer, asking who this
//! robot is and what to record next, and saying that the rig is alive.
//! `queue` is where either leaves what the network was not there to carry.

pub mod episode;
pub mod poll;
pub mod queue;
