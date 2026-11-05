#[test]
fn smoke() {
    let hub: &'static yubi_go::hub::Hub = Box::leak(Box::default());
    yubi_go::stream::serve(hub, 19091, 18081).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(200));
    for (path, want) in [("/", "<!doctype html>"), ("/state", "\"backend_offline_min\"")] {
        let mut s = std::net::TcpStream::connect("127.0.0.1:18081").unwrap();
        use std::io::{Read, Write};
        write!(s, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:18081\r\n\r\n").unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.starts_with("HTTP/1.1 200 OK"), "{path}: {}", &out[..80.min(out.len())]);
        assert!(out.contains(want), "{path} missing {want}");
    }
    // and the websocket listener is still there
    std::net::TcpStream::connect("127.0.0.1:19091").unwrap();
}
