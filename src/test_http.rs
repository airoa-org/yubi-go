//! A backend that answers, for the tests that have to see the request.
//!
//! Shared because two modules need it — `record::backend::poll` asks the backend
//! questions and `record::backend::episode` tells it things — and a second copy of a
//! socket loop is a second place for it to be subtly wrong.

use std::io::{Read, Write};
use std::net::TcpListener;

/// Bring airoa_core up. Idempotent, and every test that reaches the payload
/// comes through here, so one failure names the build step that is missing.
pub(crate) fn up() {
    let payload = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("airoa-sdk/build");
    airoa_sdk::start(&payload)
        .expect("the payload and its cwasm must both be built — see the error");
}

/// Answer every request with `body`, and hand each request back whole so a
/// test can assert on what was actually sent.
pub(crate) fn serve(body: &'static str) -> (String, std::sync::mpsc::Receiver<String>) {
    serve_routes(&[("", body)])
}

/// Answer each request with the body of the first route whose path its
/// request line starts with.
pub(crate) fn serve_routes(
    routes: &[(&'static str, &'static str)],
) -> (String, std::sync::mpsc::Receiver<String>) {
    let routes = routes.to_vec();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || loop {
        let Ok((mut sock, _)) = listener.accept() else { break };
        let mut buf = [0u8; 8192];
        let mut got = 0;
        loop {
            let n = sock.read(&mut buf[got..]).unwrap_or(0);
            got += n;
            let text = String::from_utf8_lossy(&buf[..got]);
            let complete = match text.split_once("\r\n\r\n") {
                None => false,
                Some((head, body)) => match head
                    .lines()
                    .find_map(|l| l.strip_prefix("Content-Length: "))
                    .and_then(|v| v.parse::<usize>().ok())
                {
                    Some(len) => body.len() >= len,
                    None if head.contains("transfer-encoding: chunked") => {
                        body.ends_with("0\r\n\r\n")
                    }
                    None => true,
                },
            };
            if complete || n == 0 || got == buf.len() {
                break;
            }
        }
        let request = String::from_utf8_lossy(&buf[..got]).into_owned();
        let path = request.split(' ').nth(1).unwrap_or("");
        let body = routes.iter().find(|(p, _)| path.starts_with(p)).map_or("", |(_, b)| *b);
        let _ = tx.send(request);
        let _ = write!(
            sock,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        );
    });
    (base, rx)
}

/// The files dir as airoa_core sees it: one host directory for the whole
/// test binary, because a guest path is preopened once and the first mount
/// wins. Tests that report through the one `report-queue.db` under it take
/// [`QUEUE_TURN`]: a report waiting in the queue holds back the next one.
pub(crate) fn files_root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("yubigo_files_root-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    airoa_sdk::add_preopen(&root, crate::misc::core::GUEST_ROOT, airoa_sdk::Access::ReadWrite);
    root
}

pub(crate) static QUEUE_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Point a files directory's `config.json` at a backend, so `creds` finds it.
pub(crate) fn point_at(dir: &str, base_url: &str) {
    let body = serde_json::json!({"backend": {"base_url": base_url, "api_key": "k"}});
    std::fs::write(std::path::Path::new(dir).join("config.json"), body.to_string()).unwrap();
}
