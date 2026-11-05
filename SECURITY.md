# Security

## Reporting a vulnerability

Report vulnerabilities privately to [ext-oss@airoa.org](mailto:ext-oss@airoa.org). Do not open a
public issue.

## The LAN servers are unauthenticated

The headset serves `:8080` (the setup page, previews and episodes) and `:9090` (rosbridge) with
no authentication and no TLS, by design. Anyone who can reach the headset on the network can read
its episodes, change its configuration, delete one episode or all of them, and reset the
configuration. Only starting and stopping a recording is refused, unless the request comes from
the headset itself. So run YUBI Go only on a network you trust, and do not expose these ports to
the internet.

Both servers refuse (403) a request whose `Host` header is not an IP address, `localhost` or the
headset's own mDNS name `yubi-go-<robot_id>.local`, because DNS rebinding would otherwise let a
web page read and delete episodes. rosbridge does not check `Origin`, so any page that reaches the
headset can read its live topics. So reach the headset by IP or by its `.local` name: any other DNS name for it is
refused.

## Keys on the headset

Bucket keys and the backend API key are stored unencrypted in `storage.yaml` and `config.json`
on the headset. Anyone with adb access can read them. So give the bucket key only the
permissions that the uploader uses, and rotate the keys if a headset is lost:

- `s3:PutObject` on the upload prefix, because the uploader sends each file as a single or
  multipart upload;
- `s3:AbortMultipartUpload` on the upload prefix, because the uploader cancels a multipart
  upload when a part fails or a recording starts;
- `s3:ListBucket` on the bucket, because the idle check is a `ListObjectsV2` request for zero
  keys. Without it, the check gets 403 and the panel shows `credentials rejected (403)`.

The uploader never reads or deletes objects.
