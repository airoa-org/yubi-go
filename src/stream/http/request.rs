//! Reading one request off the socket, bounded, and the cross-site check
//! every change passes before it is routed.

use std::io::{BufRead, BufReader, Read};
use std::net::TcpStream;

/// Longest request head accepted. Every URL this server answers is under 100
/// bytes; an unbounded read on a public port is a memory exhaustion bug with a
/// friendly name, and there is no framework underneath to have stopped one.
const MAX_LINE: u64 = 16 * 1024;

/// Largest request body accepted. A configuration is a few hundred bytes and
/// `Config::merge` bounds every field, so this is two orders of margin. A longer
/// body is truncated and then refused as malformed JSON, rather than trusting a
/// `Content-Length` from the network about how much to allocate.
const MAX_BODY: usize = 16 * 1024;

/// The parts of a request this server acts on.
pub(super) struct Request {
    pub method: String,
    pub path: String,
    /// The `Range` header, for the routes that serve files.
    pub range: Option<String>,
    /// `Origin` and `Host`, for [`cross_site`].
    pub origin: Option<String>,
    pub host: Option<String>,
    pub body: Vec<u8>,
}

pub(super) fn read_request(sock: &TcpStream) -> std::io::Result<Request> {
    parse_request(&mut BufReader::new(sock.try_clone()?).take(MAX_LINE + MAX_BODY as u64))
}

/// The request line, the headers a route reads, and a bounded body.
pub(super) fn parse_request(reader: &mut impl BufRead) -> std::io::Result<Request> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let path = parts.next().unwrap_or("/").to_string();
    let mut length = 0usize;
    let mut range = None;
    let (mut origin, mut host) = (None, None);
    for _ in 0..64 {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let (name, value) = (name.trim(), value.trim());
            if name.eq_ignore_ascii_case("content-length") {
                length = value.parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("range") {
                range = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("origin") {
                origin = Some(value.to_string());
            } else if name.eq_ignore_ascii_case("host") {
                host = Some(value.to_string());
            }
        }
    }
    let mut body = Vec::new();
    if length > 0 {
        reader.take(length.min(MAX_BODY) as u64).read_to_end(&mut body)?;
    }
    Ok(Request { method, path, range, origin, host, body })
}

/// A change asked for by a page on another site (CSRF). A browser sends
/// `Origin` with every request but a plain GET, and no GET here changes
/// anything, so an `Origin` that does not name this server is another site's
/// page writing to the headset. curl and the scripts send no `Origin`.
pub(super) fn cross_site(req: &Request) -> bool {
    if req.method == "GET" || req.method == "HEAD" {
        return false;
    }
    other_origin(req.origin.as_deref(), req.host.as_deref())
}

/// Whether `origin` is present and names a server other than `host`.
fn other_origin(origin: Option<&str>, host: Option<&str>) -> bool {
    let Some(origin) = origin else { return false };
    match (origin.split_once("://"), host) {
        (Some((_, named)), Some(host)) => !named.eq_ignore_ascii_case(host),
        _ => true,
    }
}

/// A `Host` that is not this headset's address, which is DNS rebinding: a
/// site whose name now resolves to the headset sends its own name, and a
/// matching `Origin`, so [`cross_site`] alone would let its page read
/// episodes and delete them. The headset is reached by IP, by `localhost`
/// through `adb forward`, or by the `.local` name it announces
/// ([`crate::stream::mdns::shown`]). No `Host` at all is HTTP/1.0 from a
/// script, which no browser sends.
pub(crate) fn foreign_host(host: Option<&str>) -> bool {
    host.is_some_and(|host| !names_us(host, crate::stream::mdns::shown()))
}

fn names_us(host: &str, mdns: Option<&str>) -> bool {
    let (name, port) = match host.rsplit_once(':') {
        Some((name, port)) if !port.contains(']') => (name, Some(port)),
        _ => (host, None),
    };
    if port.is_some_and(|p| p.parse::<u16>().is_err()) {
        return false;
    }
    let v6 = name.strip_prefix('[').and_then(|n| n.strip_suffix(']'));
    v6.is_some_and(|ip| ip.parse::<std::net::Ipv6Addr>().is_ok())
        || name.parse::<std::net::Ipv4Addr>().is_ok()
        || name.eq_ignore_ascii_case("localhost")
        || mdns.is_some_and(|m| name.eq_ignore_ascii_case(m))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::http::route::{route, Route};

    fn parsed(raw: &str) -> Request {
        parse_request(&mut std::io::BufReader::new(raw.as_bytes())).unwrap()
    }

    /// The page posts with its own origin; another site's page cannot, and
    /// curl sends none.
    #[test]
    fn a_change_from_another_site_is_refused() {
        let ours = "POST /config HTTP/1.1\r\nHost: 192.0.2.7:8080\r\n\
                    Origin: http://192.0.2.7:8080\r\n\r\n";
        assert!(!cross_site(&parsed(ours)));
        let forwarded = "DELETE /episodes/episode_1 HTTP/1.1\r\nHost: localhost:18080\r\n\
                         Origin: http://LOCALHOST:18080\r\n\r\n";
        assert!(!cross_site(&parsed(forwarded)));
        let curl = "POST /reset/data HTTP/1.1\r\nHost: 192.0.2.7:8080\r\n\r\n";
        assert!(!cross_site(&parsed(curl)));
        for origin in ["http://evil.example", "http://192.0.2.7:9000", "null"] {
            let raw = format!(
                "POST /config HTTP/1.1\r\nHost: 192.0.2.7:8080\r\nOrigin: {origin}\r\n\r\n"
            );
            assert!(cross_site(&parsed(&raw)), "{origin}");
        }
        let no_host = "POST /storage HTTP/1.1\r\nOrigin: http://192.0.2.7:8080\r\n\r\n";
        assert!(cross_site(&parsed(no_host)));
        let read = "GET /state HTTP/1.1\r\nHost: 192.0.2.7:8080\r\n\
                    Origin: http://evil.example\r\n\r\n";
        assert!(!cross_site(&parsed(read)), "a read changes nothing");
    }

    /// A page whose name rebinds to the headset sends its own name as `Host`;
    /// the headset's own addresses, `adb forward` and no `Host` at all pass.
    #[test]
    fn a_host_that_is_not_the_headset_is_refused() {
        let mdns = Some("yubi-go-075.local");
        for ours in [
            "192.0.2.7",
            "192.0.2.7:8080",
            "localhost:18080",
            "LOCALHOST",
            "[::1]",
            "[fe80::1]:8080",
            "yubi-go-075.local:8080",
            "YUBI-GO-075.LOCAL",
        ] {
            assert!(names_us(ours, mdns), "{ours}");
        }
        for foreign in [
            "evil.example",
            "evil.example:8080",
            "yubi-go-075.local.evil.example",
            "yubi-go-076.local",
            "192.0.2.7.nip.io:8080",
            "192.0.2.7:http",
            "[::1",
            "::1",
            "",
        ] {
            assert!(!names_us(foreign, mdns), "{foreign}");
        }
        assert!(!names_us("yubi-go-075.local", None), "a name never announced");
        assert!(!foreign_host(None), "HTTP/1.0 from a script");
    }

    /// A body is read to its `Content-Length` and no further, and a length
    /// nobody could mean is a cap rather than an allocation the network asks for.
    #[test]
    fn a_post_body_is_read_and_bounded() {
        let raw = "POST /config HTTP/1.1\r\n\
                   Content-Type: application/json\r\nContent-Length: 13\r\n\r\n\
                   {\"tasks\":[]}xxxxxxxx";
        let req = parse_request(&mut std::io::BufReader::new(raw.as_bytes())).unwrap();
        assert_eq!(route(&req), Route::Config);
        assert_eq!(req.body, b"{\"tasks\":[]}x");

        let raw = format!(
            "POST /config HTTP/1.1\r\nContent-Length: 900000000\r\n\r\n{}",
            "x".repeat(MAX_BODY + 500)
        );
        let req = parse_request(&mut std::io::BufReader::new(raw.as_bytes())).unwrap();
        assert_eq!(req.body.len(), MAX_BODY);
    }

    #[test]
    fn a_request_line_parses_without_its_headers() {
        let raw = "GET /state HTTP/1.1\r\nHost: x\r\n\r\n";
        let req = parse_request(&mut std::io::BufReader::new(raw.as_bytes())).unwrap();
        assert_eq!(route(&req), Route::State);
    }
}
