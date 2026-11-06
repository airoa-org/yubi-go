use std::io::Read;
use std::path::Path;
use std::time::Duration;

use log::warn;
use rusty_s3::actions::CreateMultipartUpload;
use rusty_s3::{Bucket, Credentials, S3Action, UrlStyle};

use super::config::UploadConfig;

/// How long to connect, and how long one object may take. The object timeout
/// is generous, so that slow rig wifi is not reported as wifi that is gone.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const OBJECT_TIMEOUT: Duration = Duration::from_secs(300);

/// How long a signature is valid: long enough for one object on a slow link,
/// and short enough that a URL captured off the wire is not a standing grant.
const PRESIGN_TTL: Duration = Duration::from_secs(900);

/// Above this, `put` switches to multipart so one dropped connection costs a
/// part, not the whole file. S3 requires every part but the last to be at
/// least 5 MiB; this clears that with room to spare.
const MULTIPART_THRESHOLD: u64 = 16 * 1024 * 1024;
const PART_SIZE: u64 = 16 * 1024 * 1024;
const PART_RETRIES: u32 = 3;

/// The error a send stops with when a take starts: not a failure, so the
/// take keeps its place in the queue and is sent again after it.
pub(super) const PAUSED: &str = "paused: recording";

pub(super) fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(CONNECT_TIMEOUT).timeout(OBJECT_TIMEOUT).build()
}

/// `stop` is asked before each multipart part.
pub(super) fn put_file(
    agent: &ureq::Agent,
    target: &UploadConfig,
    key: &str,
    file: &Path,
    stop: &dyn Fn() -> bool,
) -> Result<String, String> {
    let len = std::fs::metadata(file).map_err(|e| format!("{}: {e}", file.display()))?.len();
    let source = std::fs::File::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
    put(agent, target, key, len, source, stop)
}

pub(super) fn put_bytes(
    agent: &ureq::Agent,
    target: &UploadConfig,
    key: &str,
    body: &[u8],
) -> Result<String, String> {
    put(agent, target, key, body.len() as u64, std::io::Cursor::new(body.to_vec()), &|| false)
}

fn put(
    agent: &ureq::Agent,
    target: &UploadConfig,
    key: &str,
    len: u64,
    source: impl std::io::Read + Send + 'static,
    stop: &dyn Fn() -> bool,
) -> Result<String, String> {
    if len > MULTIPART_THRESHOLD {
        return put_multipart(agent, target, key, len, source, stop);
    }
    let url = presign_put(target, key)?;
    match agent.put(url.as_str()).set("Content-Length", &len.to_string()).send(source) {
        Ok(response) => Ok(response.header("etag").unwrap_or_default().to_string()),
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            Err(format!("PUT {key} → {code}: {}", body.trim()))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("PUT {key}: {}", transport(&t))),
    }
}

/// A transport error without its URL. ureq prints the request URL first, and
/// a presigned one carries the credential and the signature, which would then
/// reach `/state` and logcat.
pub(super) fn transport(t: &ureq::Transport) -> String {
    let mut out = t.kind().to_string();
    if let Some(message) = t.message() {
        out += &format!(": {message}");
    }
    if let Some(source) = std::error::Error::source(t) {
        out += &format!(": {source}");
    }
    out
}

/// The size of each part a file of `len` bytes splits into, in upload order.
/// Every part is `PART_SIZE` except the last, which takes the remainder —
/// always allowed to be under the 5 MiB minimum because only non-last parts
/// are bound by it.
fn part_sizes(len: u64) -> Vec<u64> {
    let mut sizes = Vec::new();
    let mut left = len;
    while left > 0 {
        let take = left.min(PART_SIZE);
        sizes.push(take);
        left -= take;
    }
    sizes
}

/// Send `source` as a multipart upload: create, stream each part with its own
/// short-lived presigned URL, complete. A part that fails is retried a few
/// times before the whole upload is aborted — so a dropped connection costs
/// one part's bytes, not the file's.
fn put_multipart(
    agent: &ureq::Agent,
    target: &UploadConfig,
    key: &str,
    len: u64,
    mut source: impl std::io::Read,
    stop: &dyn Fn() -> bool,
) -> Result<String, String> {
    let (bucket, credentials) = bucket(target)?;
    let upload_id = create_multipart(agent, &bucket, &credentials, key)?;

    let mut etags = Vec::new();
    for (number, size) in part_sizes(len).into_iter().enumerate() {
        let number = number as u16 + 1;
        if stop() {
            if let Err(e) = abort_multipart(agent, &bucket, &credentials, key, &upload_id) {
                warn!("upload: {key} multipart {upload_id} not aborted: {e} — the bucket may keep the parts");
            }
            return Err(PAUSED.to_string());
        }
        // Bounded to this part's size, so a multi-GB file never sits fully in
        // memory — only the part being sent (and re-sent, on retry) does.
        let mut part = Vec::with_capacity(size as usize);
        if let Err(e) = (&mut source).take(size).read_to_end(&mut part) {
            return abort_and_fail(
                agent,
                &bucket,
                &credentials,
                key,
                &upload_id,
                format!("part {number}: {e}"),
            );
        }
        match upload_part(agent, &bucket, &credentials, key, &upload_id, number, &part) {
            Ok(etag) => etags.push(etag),
            Err(e) => return abort_and_fail(agent, &bucket, &credentials, key, &upload_id, e),
        }
    }
    complete_multipart(agent, &bucket, &credentials, key, &upload_id, &etags)
}

fn abort_and_fail(
    agent: &ureq::Agent,
    bucket: &Bucket,
    credentials: &Credentials,
    key: &str,
    upload_id: &str,
    why: String,
) -> Result<String, String> {
    if let Err(e) = abort_multipart(agent, bucket, credentials, key, upload_id) {
        warn!(
            "upload: {key} multipart {upload_id} not aborted: {e} — the bucket may keep the parts"
        );
    }
    Err(format!("multipart {key}: {why}"))
}

fn create_multipart(
    agent: &ureq::Agent,
    bucket: &Bucket,
    credentials: &Credentials,
    key: &str,
) -> Result<String, String> {
    let url = bucket.create_multipart_upload(Some(credentials), key).sign(PRESIGN_TTL);
    match agent.post(url.as_str()).call() {
        Ok(response) => {
            let body = response.into_string().map_err(|e| format!("POST {key}?uploads: {e}"))?;
            CreateMultipartUpload::parse_response(&body)
                .map(|r| r.upload_id().to_string())
                .map_err(|e| format!("POST {key}?uploads: bad response: {e}"))
        }
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            Err(format!("POST {key}?uploads → {code}: {}", body.trim()))
        }
        Err(ureq::Error::Transport(t)) => Err(format!("POST {key}?uploads: {}", transport(&t))),
    }
}

/// Uploads one part, retrying a fresh presigned URL each attempt — a
/// multipart upload can run long enough to outlive the URL a single part was
/// first signed with.
fn upload_part(
    agent: &ureq::Agent,
    bucket: &Bucket,
    credentials: &Credentials,
    key: &str,
    upload_id: &str,
    number: u16,
    part: &[u8],
) -> Result<String, String> {
    let mut last = String::new();
    for attempt in 1..=PART_RETRIES {
        let url = bucket.upload_part(Some(credentials), key, number, upload_id).sign(PRESIGN_TTL);
        let sent =
            agent.put(url.as_str()).set("Content-Length", &part.len().to_string()).send(part);
        match sent {
            Ok(response) => return Ok(response.header("etag").unwrap_or_default().to_string()),
            Err(ureq::Error::Status(code, response)) => {
                let body = response.into_string().unwrap_or_default();
                last = format!("part {number} → {code}: {}", body.trim());
            }
            Err(ureq::Error::Transport(t)) => last = format!("part {number}: {}", transport(&t)),
        }
        if attempt < PART_RETRIES {
            warn!("upload: {key} part {number} attempt {attempt} failed: {last} — retrying");
        }
    }
    Err(last)
}

fn complete_multipart(
    agent: &ureq::Agent,
    bucket: &Bucket,
    credentials: &Credentials,
    key: &str,
    upload_id: &str,
    etags: &[String],
) -> Result<String, String> {
    let refs: Vec<&str> = etags.iter().map(String::as_str).collect();
    let action =
        bucket.complete_multipart_upload(Some(credentials), key, upload_id, refs.into_iter());
    let url = action.sign(PRESIGN_TTL);
    match agent.post(url.as_str()).send_string(&action.body()) {
        Ok(response) => {
            let body = response
                .into_string()
                .map_err(|e| format!("POST {key}?uploadId={upload_id}: {e}"))?;
            completed(&body).map_err(|e| format!("POST {key}?uploadId={upload_id} → 200: {e}"))
        }
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            Err(format!("POST {key}?uploadId={upload_id} → {code}: {}", body.trim()))
        }
        Err(ureq::Error::Transport(t)) => {
            Err(format!("POST {key}?uploadId={upload_id}: {}", transport(&t)))
        }
    }
}

/// The object's ETag from a CompleteMultipartUpload answer. S3 can send 200
/// and then an `<Error>` body, because it starts the answer before the parts
/// are joined; and the ETag is in the body, not in a header.
fn completed(body: &str) -> Result<String, String> {
    let inner = |tag: &str| -> Option<&str> {
        let open = format!("<{tag}>");
        let start = body.find(&open)? + open.len();
        Some(&body[start..start + body[start..].find(&format!("</{tag}>"))?])
    };
    if body.contains("<Error>") {
        let code = inner("Code").unwrap_or("no code");
        return Err(format!("{code}: {}", inner("Message").unwrap_or("").trim()));
    }
    if !body.contains("<CompleteMultipartUploadResult") {
        return Err(format!("not a completion: {}", body.trim()));
    }
    let etag = inner("ETag").ok_or("the completion names no ETag")?;
    Ok(etag
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&"))
}

fn abort_multipart(
    agent: &ureq::Agent,
    bucket: &Bucket,
    credentials: &Credentials,
    key: &str,
    upload_id: &str,
) -> Result<(), String> {
    let url = bucket.abort_multipart_upload(Some(credentials), key, upload_id).sign(PRESIGN_TTL);
    match agent.delete(url.as_str()).call() {
        Ok(_) => Ok(()),
        Err(ureq::Error::Status(code, response)) => {
            let body = response.into_string().unwrap_or_default();
            Err(format!("DELETE {key}?uploadId={upload_id} → {code}: {}", body.trim()))
        }
        Err(ureq::Error::Transport(t)) => {
            Err(format!("DELETE {key}?uploadId={upload_id}: {}", transport(&t)))
        }
    }
}

fn bucket(target: &UploadConfig) -> Result<(Bucket, Credentials), String> {
    let endpoint: url::Url =
        target.endpoint.parse().map_err(|e| format!("bad endpoint {}: {e}", target.endpoint))?;
    let region = match target.region.as_str() {
        "" => "us-east-1",
        region => region,
    };
    let bucket = Bucket::new(endpoint, UrlStyle::Path, target.bucket.clone(), region.to_string())
        .map_err(|e| format!("bad bucket {}: {e}", target.bucket))?;
    Ok((bucket, Credentials::new(target.access_key.clone(), target.secret_key.clone())))
}

pub(super) fn presign_put(target: &UploadConfig, key: &str) -> Result<url::Url, String> {
    let (bucket, credentials) = bucket(target)?;
    Ok(bucket.put_object(Some(&credentials), key).sign(PRESIGN_TTL))
}

pub(super) fn presign_probe(target: &UploadConfig) -> Result<url::Url, String> {
    let (bucket, credentials) = bucket(target)?;
    let mut list = bucket.list_objects_v2(Some(&credentials));
    list.with_max_keys(0);
    Ok(list.sign(PRESIGN_TTL))
}

#[cfg(test)]
pub(super) mod tests {
    use std::io::Write;
    use std::net::TcpListener;
    use std::path::PathBuf;

    use super::super::config::PathRule;
    use super::*;

    #[test]
    fn a_transport_error_does_not_carry_the_signature() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/k?X-Amz-Signature=deadbeef");
        let Err(ureq::Error::Transport(t)) = agent().get(&url).call() else {
            panic!("nothing listens on {port}");
        };
        assert!(t.to_string().contains("deadbeef"), "ureq's own text names the URL");
        let said = transport(&t);
        assert!(!said.contains("deadbeef"), "{said}");
        assert!(said.contains("Connection Failed"), "{said}");
    }

    #[test]
    fn a_file_under_the_threshold_is_a_single_part() {
        assert_eq!(part_sizes(0), Vec::<u64>::new());
        assert_eq!(part_sizes(1), vec![1]);
        assert_eq!(part_sizes(PART_SIZE), vec![PART_SIZE]);
    }

    #[test]
    fn a_file_over_the_threshold_splits_into_full_parts_and_a_remainder() {
        assert_eq!(part_sizes(PART_SIZE + 1), vec![PART_SIZE, 1]);
        assert_eq!(part_sizes(PART_SIZE * 2), vec![PART_SIZE, PART_SIZE]);
        assert_eq!(
            part_sizes(PART_SIZE * 2 + 1024),
            vec![PART_SIZE, PART_SIZE, 1024],
            "the last part is under the 5 MiB minimum, which is fine — only non-last parts are bound by it"
        );
    }

    /// A minimal S3 stand-in: it answers `part_count + 2` requests in order
    /// (create, one PUT per part, complete) and hands back every request
    /// line it saw, so the test can assert on the sequence rather than just
    /// the final result.
    fn fake_multipart_server(part_count: usize) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        let requests = part_count + 2;
        std::thread::spawn(move || {
            for _ in 0..requests {
                let Ok((mut sock, _)) = listener.accept() else { break };
                let mut buf = vec![0u8; PART_SIZE as usize + 4096];
                let mut got = 0;
                let (method, path) = loop {
                    let n = sock.read(&mut buf[got..]).unwrap_or(0);
                    got += n;
                    let text = String::from_utf8_lossy(&buf[..got]);
                    let Some((head, body)) = text.split_once("\r\n\r\n") else {
                        if n == 0 {
                            break (String::new(), String::new());
                        }
                        continue;
                    };
                    let want = head
                        .lines()
                        .find_map(|l| l.strip_prefix("Content-Length: "))
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    if body.len() >= want || n == 0 {
                        let mut first = head.lines().next().unwrap_or("").split_whitespace();
                        break (
                            first.next().unwrap_or("").to_string(),
                            first.next().unwrap_or("").to_string(),
                        );
                    }
                };
                let _ = tx.send(format!("{method} {path}"));
                let response = if path.contains("uploads=1") {
                    let xml = "<InitiateMultipartUploadResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><UploadId>test-upload-id</UploadId></InitiateMultipartUploadResult>";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
                        xml.len()
                    )
                } else if method == "PUT" {
                    "HTTP/1.1 200 OK\r\nETag: \"part-etag\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string()
                } else {
                    let xml = "<CompleteMultipartUploadResult><Key>big.bin</Key><ETag>&quot;final-etag&quot;</ETag></CompleteMultipartUploadResult>";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
                        xml.len()
                    )
                };
                let _ = sock.write_all(response.as_bytes());
            }
        });
        (base, rx)
    }

    #[test]
    fn a_large_file_is_sent_as_several_parts_then_completed() {
        let base = temp("multipart");
        let file = base.join("big.bin");
        let size = PART_SIZE * 2 + 1024;
        std::fs::write(&file, vec![7u8; size as usize]).unwrap();
        let expected = part_sizes(size).len();
        assert_eq!(expected, 3, "two full parts plus a remainder");

        let (endpoint, rx) = fake_multipart_server(expected);
        let target = UploadConfig { endpoint, ..minio_target() };

        let etag = put_file(&agent(), &target, "big.bin", &file, &|| false).unwrap();
        assert_eq!(etag, "\"final-etag\"", "the final ETag is complete's, not a part's");

        let seen: Vec<String> =
            (0..expected + 2).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert!(seen[0].contains("POST") && seen[0].contains("uploads=1"), "{seen:?}");
        for r in &seen[1..=expected] {
            assert!(r.starts_with("PUT") && r.contains("partNumber="), "{r}");
        }
        let complete = &seen[expected + 1];
        assert!(complete.starts_with("POST") && complete.contains("uploadId="), "{complete}");
        assert!(!complete.contains("uploads=1"), "{complete}");
    }

    #[test]
    fn a_take_starting_mid_file_stops_the_upload_between_parts() {
        let base = temp("multipart_paused");
        let file = base.join("big.bin");
        std::fs::write(&file, vec![7u8; (PART_SIZE * 2 + 1024) as usize]).unwrap();
        // Create, one part, then the abort.
        let (endpoint, rx) = fake_multipart_server(1);
        let target = UploadConfig { endpoint, ..minio_target() };

        let asked = std::cell::Cell::new(0);
        let stop = || {
            asked.set(asked.get() + 1);
            asked.get() > 1
        };
        let sent = put_file(&agent(), &target, "big.bin", &file, &stop);
        assert_eq!(sent, Err(PAUSED.to_string()));
        let seen: Vec<String> =
            (0..3).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        assert!(seen[1].starts_with("PUT") && seen[1].contains("partNumber=1"), "{seen:?}");
        assert!(seen[2].starts_with("DELETE") && seen[2].contains("uploadId="), "{seen:?}");
    }

    /// Bodies as AWS documents them for CompleteMultipartUpload.
    #[test]
    fn a_completion_is_judged_by_its_body() {
        let ok = r#"<?xml version="1.0" encoding="UTF-8"?>
<CompleteMultipartUploadResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Location>http://data.s3.amazonaws.com/big.bin</Location><Bucket>data</Bucket>
  <Key>big.bin</Key><ETag>"3858f62230ac3c915f300c664312c11f-9"</ETag>
</CompleteMultipartUploadResult>"#;
        assert_eq!(completed(ok).unwrap(), "\"3858f62230ac3c915f300c664312c11f-9\"");
        let escaped = ok.replace("<ETag>\"", "<ETag>&quot;").replace("\"</ETag>", "&quot;</ETag>");
        assert_eq!(completed(&escaped).unwrap(), "\"3858f62230ac3c915f300c664312c11f-9\"");

        let error = r#"<?xml version="1.0" encoding="UTF-8"?>

<Error><Code>InternalError</Code><Message>We encountered an internal error. Please try again.</Message>
<RequestId>656c76696e6727732072657175657374</RequestId></Error>"#;
        let why = completed(error).unwrap_err();
        assert!(why.contains("InternalError") && why.contains("internal error"), "{why}");
        assert!(completed("").is_err(), "an empty 200 is not a completion");
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("yubigo_upload_{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    pub(crate) fn minio_target() -> UploadConfig {
        UploadConfig {
            endpoint: "http://minio:9000".into(),
            bucket: "data".into(),
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            region: String::new(),
            prefix: String::new(),
            path_rule: PathRule::Flat,
            delete_after_upload: false,
        }
    }

    #[test]
    fn the_presigned_url_is_path_style_with_the_key_encoded() {
        let url = presign_put(&minio_target(), "org=a/ts=1:2Z/episode.mcap").unwrap();
        assert_eq!(url.host_str(), Some("minio"));
        assert_eq!(url.port(), Some(9000));
        assert_eq!(url.path(), "/data/org%3Da/ts%3D1%3A2Z/episode.mcap");
    }

    #[test]
    fn the_presigned_url_carries_the_signature_in_the_query() {
        let url = presign_put(&minio_target(), "episode.mcap").unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(q["X-Amz-Algorithm"], "AWS4-HMAC-SHA256");
        assert_eq!(q["X-Amz-Expires"], PRESIGN_TTL.as_secs().to_string());
        assert!(q["X-Amz-Credential"].contains("AKIDEXAMPLE"));
        assert!(q["X-Amz-Credential"].contains("us-east-1/s3/aws4_request"));
        assert_eq!(q["X-Amz-Signature"].len(), 64);
        assert!(!url.as_str().contains(&minio_target().secret_key));
    }

    #[test]
    fn the_probe_asks_for_no_keys_at_all() {
        let url = presign_probe(&minio_target()).unwrap();
        let q: std::collections::HashMap<_, _> = url.query_pairs().collect();
        assert_eq!(q["max-keys"], "0");
        assert_eq!(q["list-type"], "2");
        assert_eq!(url.path(), "/data/", "the bucket itself, path style");
    }
}
