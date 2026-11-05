#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.10"
# dependencies = ["pyarrow", "numpy"]
# ///
"""Exports one take three ways and checks that the datasets agree.

    tools/export-parity.py http://HEADSET:8080 episode_1793435763607

1. on the headset, through POST /exports (the hardware encoder),
2. with yubi-export built from this clone, here,
3. with a static Linux build of it in an x86-64 Ubuntu container, when
   `cargo zigbuild` and a running Docker (`colima start`) are there.

Check out the commit the headset was built from first, so the two exports run
the same code. Needs ffmpeg. Work goes to target/export-parity/<take>.

The tables come from the same Rust code everywhere, so they differ only where
Android, macOS and musl round sin/cos differently (1e-16 on 2026-10-31). The
videos come from different encoders, so they are held to the same frames, the
same timing and a PSNR floor instead.
"""

import io
import json
import shutil
import subprocess
import sys
import tarfile
import time
import urllib.request
from pathlib import Path

import numpy as np
import pyarrow.parquet as pq

# Tables: three orders of magnitude above the 1e-16 seen, far below any pose
# precision that matters (a micrometre, or 1e-9 of a quaternion component).
TABLE_TOL = 1e-9
# Image stats: one 8-bit level is 0.004. Two encoders differ by a few levels at
# the extremes; 0.05 is about 13.
IMAGE_STATS_TOL = 0.05
# Headset against x264 measured 36-38 dB on one recorded episode (2026-10-31).
# Below 30 dB is visibly different, such as a wrong crop or colour range.
PSNR_FLOOR = 30.0

IMAGE = "ubuntu:24.04-ffmpeg-yubi-export-parity"
ROOT = Path(__file__).resolve().parent.parent

failures = []


def check(ok, what):
    print(("  ok    " if ok else "  FAIL  ") + what)
    if not ok:
        failures.append(what)


def get(url, data=None, method=None):
    req = urllib.request.Request(url, data=data, method=method)
    with urllib.request.urlopen(req, timeout=60) as res:
        return res.read()


def run(cmd, **kw):
    subprocess.run(cmd, check=True, **kw)


def export_on_headset(base, take, out):
    name = "parity-%d" % time.time()
    body = json.loads(get(base + "/exports", json.dumps({"name": name, "episodes": [take]}).encode(), "POST"))
    if "id" not in body:
        sys.exit("the headset refused the export: %s" % body)
    eid = body["id"]
    while True:
        s = json.loads(get(base + "/exports"))
        job = s["job"]
        if job and job.get("id") == eid and job["state"] == "running":
            print("\r  headset: %s\033[K" % job["step"], end="", flush=True)
            time.sleep(1)
            continue
        print()
        if job and job.get("id") == eid and job["state"] == "failed":
            sys.exit("the headset's export failed: %s" % job["error"])
        break
    with tarfile.open(fileobj=io.BytesIO(get("%s/exports/%s.tar" % (base, eid)))) as tar:
        tar.extractall(out.parent, filter="data")
    get("%s/exports/%s" % (base, eid), method="DELETE")
    (out.parent / eid).rename(out)


def export_on_linux(work, take):
    have = subprocess.run(["docker", "image", "inspect", IMAGE], capture_output=True).returncode == 0
    if not have:
        dockerfile = b"FROM ubuntu:24.04\nRUN apt-get update && apt-get install -y ffmpeg && rm -rf /var/lib/apt/lists/*\n"
        run(["docker", "build", "--platform", "linux/amd64", "-t", IMAGE, "-"], input=dockerfile)
    run(["docker", "run", "--rm", "--platform", "linux/amd64", "-v", "%s:/w" % work, "-w", "/w", IMAGE,
         "./yubi-export-linux", "episodes", "linux", take])


def build(work, linux):
    run(["cargo", "build", "--release", "--locked", "-p", "yubi-export"], cwd=ROOT)
    shutil.copy(ROOT / "target/release/yubi-export", work / "yubi-export-local")
    if linux:
        musl = "x86_64-unknown-linux-musl"
        run(["cargo", "zigbuild", "--release", "--locked", "-p", "yubi-export", "--target", musl], cwd=ROOT)
        shutil.copy(ROOT / "target" / musl / "release/yubi-export", work / "yubi-export-linux")


def files(root):
    return sorted(str(p.relative_to(root)) for p in root.rglob("*") if p.is_file())


def compare_tables(a, b, pair):
    for f in files(a):
        if not f.endswith(".parquet"):
            continue
        ta, tb = pq.read_table(a / f), pq.read_table(b / f)
        if ta.column_names != tb.column_names or ta.num_rows != tb.num_rows:
            check(False, "%s %s: columns or rows differ" % (pair, f))
            continue
        worst, where = 0.0, ""
        for c in ta.column_names:
            x, y = ta[c].to_pylist(), tb[c].to_pylist()
            if x == y:
                continue
            d = float(np.max(np.abs(np.array(x, dtype=float) - np.array(y, dtype=float))))
            if d > worst:
                worst, where = d, c
        check(worst <= TABLE_TOL, "%s %s: max difference %.2g%s" % (pair, f, worst, " in " + where if where else ""))


def compare_stats(a, b, pair):
    la = [json.loads(l) for l in open(a / "meta/episodes_stats.jsonl")]
    lb = [json.loads(l) for l in open(b / "meta/episodes_stats.jsonl")]
    worst = {True: 0.0, False: 0.0}
    for ea, eb in zip(la, lb):
        for k, sa in ea["stats"].items():
            sb = eb["stats"][k]
            for s in ("min", "max", "mean", "std"):
                d = float(np.max(np.abs(np.array(sa[s], dtype=float) - np.array(sb[s], dtype=float))))
                image = k.startswith("observation.image.") and not k.endswith("is_fresh")
                worst[image] = max(worst[image], d)
    check(len(la) == len(lb), "%s episodes_stats: %d and %d episodes" % (pair, len(la), len(lb)))
    check(worst[False] <= TABLE_TOL, "%s episodes_stats: signals differ by %.2g" % (pair, worst[False]))
    check(worst[True] <= IMAGE_STATS_TOL, "%s episodes_stats: images differ by %.3f" % (pair, worst[True]))


def probe(mp4):
    out = subprocess.run(
        ["ffprobe", "-v", "error", "-select_streams", "v", "-show_entries",
         "stream=width,height,pix_fmt,color_range:frame=pts_time", "-of", "json", str(mp4)],
        capture_output=True, check=True, text=True).stdout
    j = json.loads(out)
    s = j["streams"][0]
    return (s["width"], s["height"], s["pix_fmt"], s.get("color_range")), [float(f["pts_time"]) for f in j["frames"]]


def psnr(a, b):
    out = subprocess.run(["ffmpeg", "-hide_banner", "-i", str(a), "-i", str(b), "-lavfi", "psnr", "-f", "null", "-"],
                         capture_output=True, text=True).stderr
    tail = out.rsplit("average:", 1)[-1].split()[0]
    return float("inf") if tail == "inf" else float(tail)


def compare_videos(sets):
    names = list(sets)
    for f in files(sets[names[0]]):
        if not f.endswith(".mp4"):
            continue
        probes = {n: probe(sets[n] / f) for n in names}
        fmt, times = probes[names[0]]
        expect = [i / 30 for i in range(len(times))]
        check(all(abs(t - e) < 1e-4 for t, e in zip(times, expect)), "%s: frame i at i/30 s (%s)" % (f, names[0]))
        check(fmt[2:] == ("yuvj420p", "pc"), "%s: full-range 4:2:0 (%s)" % (f, names[0]))
        for n in names[1:]:
            check(probes[n] == probes[names[0]], "%s: %s has %s's frames, timing and format" % (f, n, names[0]))
        for i, x in enumerate(names):
            for y in names[i + 1:]:
                p = psnr(sets[x] / f, sets[y] / f)
                check(p >= PSNR_FLOOR, "%s: %s/%s %.1f dB" % (f, x, y, p))


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    base, take = sys.argv[1].rstrip("/"), sys.argv[2]
    if "://" not in base:
        base = "http://" + base
    linux = (subprocess.run(["cargo", "zigbuild", "--help"], capture_output=True).returncode == 0
             and subprocess.run(["docker", "info"], capture_output=True).returncode == 0)
    if not linux:
        print("no cargo zigbuild or no running Docker: skipping the Linux build")
    work = ROOT / "target" / "export-parity" / take
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)

    print("fetching the take from the headset")
    run([sys.executable, str(ROOT / "assets/web/pull.py"), base, str(work / "episodes"), take],
        stdout=subprocess.DEVNULL)
    print("building yubi-export")
    build(work, linux)

    print("exporting on the headset")
    export_on_headset(base, take, work / "headset")
    print("exporting with the local build")
    run([str(work / "yubi-export-local"), "episodes", "local", take], cwd=work)
    if linux:
        print("exporting with the Linux build")
        export_on_linux(work, take)

    builds = ["local", "linux"] if linux else ["local"]
    sets = {n: work / n for n in ["headset"] + builds}
    print("comparing")
    listing = files(sets["headset"])
    for n in builds:
        check(files(sets[n]) == listing, "%s writes the headset's files (%d)" % (n, len(listing)))
        for f in ("meta/info.json", "meta/tasks.jsonl", "meta/episodes.jsonl"):
            check((sets[n] / f).read_bytes() == (sets["headset"] / f).read_bytes(), "%s %s is identical" % (n, f))
        compare_tables(sets["headset"], sets[n], "headset/" + n)
        compare_stats(sets["headset"], sets[n], "headset/" + n)
    compare_videos(sets)

    print()
    if failures:
        sys.exit("%d checks failed (datasets in %s)" % (len(failures), work))
    print("all checks passed (datasets in %s)" % work)


if __name__ == "__main__":
    main()
