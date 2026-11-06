//! Everything this app serves over the network: `rosbridge` on `:9090` for
//! live state, `http` on `:8080` for the side interface and MJPEG, and the one
//! `accept` thread they share.

pub mod accept;
pub mod http;
pub mod mdns;
pub mod rosbridge;

pub use accept::serve;
