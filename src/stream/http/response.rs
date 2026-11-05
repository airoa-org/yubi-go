//! Writing responses: a whole body, a body by byte range, and a tar streamed
//! from files on disk.

use std::io::{Read, Write};
use std::net::TcpStream;

/// The bytes `start..end` a `Range` header asks for, or `None` when it asks
/// for none of them. Only the single-range forms: `a-b`, `a-` and `-n`.
fn byte_range(header: &str, len: u64) -> Option<(u64, u64)> {
    let (a, b) = header.trim().strip_prefix("bytes=")?.split_once('-')?;
    let (start, end) = if a.is_empty() {
        (len.saturating_sub(b.parse().ok()?), len)
    } else {
        let start = a.parse().ok()?;
        (start, if b.is_empty() { len } else { b.parse::<u64>().ok()?.saturating_add(1).min(len) })
    };
    (start < end).then_some((start, end))
}

/// The status line and headers for a body of `len` bytes that may be asked
/// for in part, and which bytes go out. `None` once a range nothing satisfies
/// has been refused with 416.
pub(super) fn ranged(
    sock: &mut TcpStream,
    range: Option<&str>,
    len: u64,
    content_type: &str,
) -> std::io::Result<Option<(u64, u64)>> {
    let common = "Connection: close\r\nCache-Control: no-store\r\nAccept-Ranges: bytes\r\n";
    let Some(range) = range else {
        write!(
            sock,
            "HTTP/1.1 200 OK\r\n{common}Content-Type: {content_type}\r\nContent-Length: {len}\r\n\r\n"
        )?;
        return Ok(Some((0, len)));
    };
    let Some((start, end)) = byte_range(range, len) else {
        write!(
            sock,
            "HTTP/1.1 416 Range Not Satisfiable\r\n{common}Content-Range: bytes */{len}\r\n\
             Content-Length: 0\r\n\r\n"
        )?;
        return Ok(None);
    };
    write!(
        sock,
        "HTTP/1.1 206 Partial Content\r\n{common}Content-Type: {content_type}\r\n\
         Content-Range: bytes {start}-{}/{len}\r\nContent-Length: {}\r\n\r\n",
        end - 1,
        end - start
    )?;
    Ok(Some((start, end)))
}

/// `files` as `(name in the tar, path on disk, bytes)`, in that order.
pub(super) fn send_tar(
    sock: &mut TcpStream,
    filename: &str,
    files: &[(String, std::path::PathBuf, u64)],
) -> std::io::Result<()> {
    if let Some((name, _, _)) = files.iter().find(|f| f.0.len() >= 100) {
        let message = format!("{name} is too long a name for a tar\n");
        return send(sock, 500, "text/plain", message.as_bytes());
    }
    // The ustar size field is 11 octal digits.
    if let Some((name, _, _)) = files.iter().find(|f| f.2 >= 8 << 30) {
        let message = format!("{name} is 8 GiB or more, too large for a tar\n");
        return send(sock, 500, "text/plain", message.as_bytes());
    }
    let len: u64 =
        files.iter().map(|(_, _, bytes)| 512 + bytes.div_ceil(512) * 512).sum::<u64>() + 1024;
    write!(
        sock,
        "HTTP/1.1 200 OK\r\nConnection: close\r\nCache-Control: no-store\r\n\
         Content-Type: application/x-tar\r\n\
         Content-Disposition: attachment; filename=\"{filename}\"\r\n\
         Content-Length: {len}\r\n\r\n"
    )?;
    let mut out = std::io::BufWriter::with_capacity(64 * 1024, sock);
    for (file, path, bytes) in files {
        let f = std::fs::File::open(path)?;
        let mtime = f
            .metadata()?
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs());
        out.write_all(&tar_header(file, *bytes, mtime))?;
        // The length was promised in the header, so a file that changed size
        // since has to end the response rather than misalign every entry after.
        if std::io::copy(&mut f.take(*bytes), &mut out)? != *bytes {
            return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "file shrank"));
        }
        out.write_all(&vec![0; (bytes.div_ceil(512) * 512 - bytes) as usize])?;
    }
    out.write_all(&[0; 1024])?;
    out.flush()
}

/// A ustar header for one regular file.
fn tar_header(path: &str, size: u64, mtime: u64) -> [u8; 512] {
    let mut h = [0u8; 512];
    let mut put = |at: usize, field: &[u8]| h[at..at + field.len()].copy_from_slice(field);
    put(0, path.as_bytes());
    put(100, b"0000644\0");
    put(108, b"0000000\0");
    put(116, b"0000000\0");
    put(124, format!("{size:011o}\0").as_bytes());
    put(136, format!("{mtime:011o}\0").as_bytes());
    put(148, b"        ");
    put(156, b"0");
    put(257, b"ustar\0");
    put(263, b"00");
    let sum: u32 = h.iter().map(|&b| b as u32).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

pub(super) fn content_type(file: &str) -> &'static str {
    match file.rsplit('.').next() {
        Some("json") => "application/json",
        Some("csv") => "text/csv",
        Some("mp4") => "video/mp4",
        _ => "application/octet-stream",
    }
}

pub(super) fn send(
    sock: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write!(
        sock,
        "HTTP/1.1 {status} {}\r\nConnection: close\r\nCache-Control: no-store\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\r\n",
        reason(status),
        body.len()
    )?;
    sock.write_all(body)
}

/// The reason phrase for every status this server sends.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        409 => "Conflict",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        _ => "Error",
    }
}

pub(super) fn send_json(sock: &mut TcpStream, status: u16, body: &str) -> std::io::Result<()> {
    send(sock, status, "application/json", body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_clamped_to_the_body_and_refused_past_it() {
        assert_eq!(byte_range("bytes=0-99", 1000), Some((0, 100)));
        assert_eq!(byte_range("bytes=900-", 1000), Some((900, 1000)));
        assert_eq!(byte_range("bytes=900-5000", 1000), Some((900, 1000)));
        assert_eq!(byte_range("bytes=-100", 1000), Some((900, 1000)));
        assert_eq!(byte_range("bytes=-5000", 1000), Some((0, 1000)));
        assert_eq!(byte_range("bytes=1000-", 1000), None);
        assert_eq!(byte_range("bytes=5-2", 1000), None);
        assert_eq!(byte_range("bytes=0-1,5-6", 1000), None);
        assert_eq!(byte_range("items=0-1", 1000), None);
    }

    /// Checked against the format rather than a tar binary: the checksum is
    /// the header's byte sum with its own field read as spaces.
    #[test]
    fn a_tar_header_carries_its_name_size_and_checksum() {
        let h = tar_header("episode_1/meta.json", 2898, 1_792_585_604);
        assert_eq!(&h[..19], b"episode_1/meta.json");
        assert_eq!(&h[124..136], b"00000005522\0");
        assert_eq!(&h[257..263], b"ustar\0");
        let mut blank = h;
        blank[148..156].copy_from_slice(b"        ");
        let sum: u32 = blank.iter().map(|&b| b as u32).sum();
        assert_eq!(std::str::from_utf8(&h[148..154]).unwrap(), format!("{sum:06o}"));
    }
}
