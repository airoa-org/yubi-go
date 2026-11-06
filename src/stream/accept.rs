//! One thread that answers both listeners.
//!
//! **Threads are the scarce resource on the headset**, and two blocking
//! `accept` loops would cost two resident ones to sit waiting for connections
//! that arrive minutes apart. Non-blocking listeners polled together cost one.
//!
//! What each *connection* costs is unchanged: a websocket client and an MJPEG
//! viewer are both long-lived and blocking, so they get a thread apiece and
//! keep it for as long as they are watching.

use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::hub::Hub;

/// How long the accept loop sleeps between looks. Connections here arrive when
/// a person opens a page or a monitoring client starts watching — minutes apart,
/// never in a burst — so a tenth of a second of latency is invisible and the
/// thread is asleep the rest of the time.
const ACCEPT_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// How long a new connection may stay silent before its request or websocket
/// handshake. Browsers and curl send it the moment they connect, and Chrome
/// drops a preconnected socket it has not used after 10 s, so 30 s closes only
/// a client that is never going to ask, which would otherwise hold a thread.
/// Only the request is read under it: HTTP handlers write and read nothing
/// more, and rosbridge sets its own once the handshake is done.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How long one write may move no byte before the connection is dropped. A
/// live client is written to at least every 100 ms (previews and rosbridge
/// publish at 10 Hz), and a download is written as fast as the network takes
/// it, so 10 s of a shut receive window is a client that stopped reading, such
/// as a phone asleep with the page open. TCP itself never gives up on one
/// while it still acknowledges window probes, and it would hold its thread.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Most connections held at once, each a thread. A browser opens at most 6 to
/// one host, and the viewer holds 4 of them (three cameras and the poses), so
/// 32 is five browsers at their limit with a monitoring client beside them.
const MAX_CONNECTIONS: usize = 32;

static OPEN: AtomicUsize = AtomicUsize::new(0);

/// One of [`MAX_CONNECTIONS`], given back when its thread ends or never starts.
struct Held;

impl Drop for Held {
    fn drop(&mut self) {
        OPEN.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Start both servers.
pub fn serve(hub: &'static Hub, ws_port: u16, http_port: u16) -> std::io::Result<()> {
    let ws = bind(ws_port)?;
    let web = bind(http_port)?;
    std::thread::spawn(move || loop {
        let mut idle = true;
        for (listener, kind) in [(&ws, Kind::Ws), (&web, Kind::Http)] {
            while let Ok((sock, _)) = listener.accept() {
                idle = false;
                // The listener is non-blocking; a connection taken off it is
                // not, and every handler below reads and writes as if it owned
                // the socket.
                if sock.set_nonblocking(false).is_err()
                    || sock.set_read_timeout(Some(REQUEST_TIMEOUT)).is_err()
                    || sock.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
                {
                    continue;
                }
                if OPEN.fetch_add(1, Ordering::Relaxed) >= MAX_CONNECTIONS {
                    OPEN.fetch_sub(1, Ordering::Relaxed);
                    log::warn!("refused a connection: {MAX_CONNECTIONS} already open");
                    continue;
                }
                let held = Held;
                // A thread that cannot start drops its closure, and with it
                // the socket and the slot.
                let started = std::thread::Builder::new().spawn(move || {
                    let _held = held;
                    match kind {
                        Kind::Ws => super::rosbridge::serve(hub, sock),
                        Kind::Http => {
                            let _ = super::http::handle(hub, sock);
                        }
                    }
                });
                if let Err(e) = started {
                    log::warn!("cannot start a connection thread: {e}");
                }
            }
        }
        if idle {
            std::thread::sleep(ACCEPT_POLL);
        }
    });
    Ok(())
}

/// Which handler a socket goes to. Named rather than inferred from a port
/// number two screens away.
#[derive(Clone, Copy)]
enum Kind {
    Ws,
    Http,
}

/// IPv6 first: a direct ethernet cable has no DHCP, but link-local IPv6 always
/// works. Dual-stack sockets still accept IPv4 (wifi).
fn bind(port: u16) -> std::io::Result<TcpListener> {
    let listener =
        TcpListener::bind(("::", port)).or_else(|_| TcpListener::bind(("0.0.0.0", port)))?;
    listener.set_nonblocking(true)?;
    Ok(listener)
}
