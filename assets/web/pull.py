"""Copy finished episodes off a YUBI Go headset.

    curl -s http://HEADSET:8080/pull.py | python3 - http://HEADSET:8080 [DEST [EPISODE ...]]
        [--folder FOLDER]

Every episode, or only the EPISODEs named, or those filed under FOLDER. The
names chosen go to DEST/episodes.txt, one per line, for yubi-export. Standard library only, so any
Python 3.8+ runs it as-is. Safe to rerun: an episode already in DEST is
skipped, and an interrupted file resumes where it stopped. Each episode's
meta.json is fetched last, so a directory without one is a copy that has not
finished, just as on the headset. Nothing is deleted from the headset.
"""

import json
import os
import sys
import urllib.request

CHUNK = 1 << 20


def fetch(url, path, size):
    """Download `url` to `path`, resuming from a `.part` file."""
    part = path + ".part"
    have = os.path.getsize(part) if os.path.exists(part) else 0
    if have > size:
        os.remove(part)
        have = 0
    if have < size:
        req = urllib.request.Request(url, headers={"Range": "bytes=%d-" % have} if have else {})
        with urllib.request.urlopen(req, timeout=30) as res:
            if have and res.status != 206:
                have = 0
            with open(part, "ab" if have else "wb") as out:
                while True:
                    block = res.read(CHUNK)
                    if not block:
                        break
                    out.write(block)
                    have += len(block)
                    print("\r  %s  %5.1f%%" % (os.path.basename(path), 100.0 * have / max(size, 1)),
                          end="", flush=True)
        print()
    if os.path.getsize(part) != size:
        raise IOError("%s: got %d of %d bytes" % (url, os.path.getsize(part), size))
    os.replace(part, path)


def main():
    args, folder = [], None
    it = iter(sys.argv[1:])
    for a in it:
        if a == "--folder":
            folder = next(it, None)
            if folder is None:
                sys.exit("--folder needs a folder")
            folder = folder.strip("/")
        else:
            args.append(a)
    sys.argv[1:] = args
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    base = sys.argv[1].rstrip("/")
    if "://" not in base:
        base = "http://" + base
    dest = sys.argv[2] if len(sys.argv) > 2 else "episodes"
    with urllib.request.urlopen(base + "/episodes", timeout=30) as res:
        episodes = json.load(res)
    only = set(sys.argv[3:])
    if only:
        episodes = [ep for ep in episodes if ep["name"] in only]
    if folder:
        episodes = [ep for ep in episodes
                    if (ep.get("folder") or "") == folder or (ep.get("folder") or "").startswith(folder + "/")]
    copied = 0
    for ep in episodes:
        folder = os.path.join(dest, ep["name"])
        files = sorted(ep["files"], key=lambda f: f["name"] == "meta.json")
        done = all(
            os.path.exists(os.path.join(folder, f["name"]))
            and os.path.getsize(os.path.join(folder, f["name"])) == f["bytes"]
            for f in files
        )
        if done:
            continue
        print("%s  %s  %.0f MB" % (ep["name"], ep.get("label") or "", ep["bytes"] / 1e6))
        os.makedirs(folder, exist_ok=True)
        for f in files:
            path = os.path.join(folder, f["name"])
            if os.path.exists(path) and os.path.getsize(path) == f["bytes"]:
                continue
            fetch("%s/episodes/%s/%s" % (base, ep["name"], f["name"]), path, f["bytes"])
        copied += 1
    print("%d copied, %d already here, in %s" % (copied, len(episodes) - copied, os.path.abspath(dest)))
    os.makedirs(dest, exist_ok=True)
    with open(os.path.join(dest, "episodes.txt"), "w") as out:
        out.writelines(name + "\n" for name in sorted(ep["name"] for ep in episodes))

if __name__ == "__main__":
    main()
