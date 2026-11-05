# /// script
# requires-python = ">=3.10,<3.11"
# dependencies = [
#     "lerobot @ git+https://github.com/huggingface/lerobot@0cf864870cf29f4738d3ade893e6fd13fbd7cdb5",
#     "datasets==3.6.0",
#     "huggingface-hub==0.32.3",
#     "rerun-sdk==0.23.1",
#     "numpy==1.26.4",
# ]
# ///
"""Open a YUBI Go LeRobot export in Rerun: cameras, gripper joints and poses.

    curl -s http://HEADSET:8080/rerun_view.py | uv run - EXPORT

EXPORT is the export's address (http://HEADSET:8080/exports/NAME.tar, downloaded again on every
run into ~/lerobot/NAME), a downloaded NAME.tar (unpacked once beside it), or a dataset folder.

Every frame comes from LeRobotDataset at a pinned lerobot commit that reads v2.1, the
loader a trainer of this data uses, so what Rerun shows is what training reads: the
timestamp check, and each camera frame decoded at its row's timestamp. The other
pins are versions that commit works with; unpinned, `datasets` 4 breaks this lerobot's loader.
Each LeRobot episode is its own recording; pick one in Rerun's recording list.
"""

import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

import numpy as np
import rerun as rr
from lerobot.common.datasets.lerobot_dataset import LeRobotDataset


def unpack(f, into, fresh=False):
    """The export folder in the tar file `f`, unpacked into `into` unless it is there already.
    `fresh` replaces a folder that is there."""
    with tarfile.open(fileobj=f) as t:
        # Not the first entry, because macOS tar puts AppleDouble files (._name) first. Not the
        # first directory either, because the headset's tar holds files only.
        name = next(top for m in t if not (top := m.name.split("/")[0]).startswith("._"))
        if fresh and (into / name).is_dir():
            shutil.rmtree(into / name)
        if not (into / name).is_dir():
            # Unpacked beside it and moved in whole, so a folder there is a finished unpack.
            part = Path(tempfile.mkdtemp(prefix=".partial-", dir=into))
            t.extractall(part, filter="data")
            (part / name).rename(into / name)
            shutil.rmtree(part)
    return into / name


arg = sys.argv[1]
if arg.startswith(("http://", "https://")):
    home = Path("~/lerobot").expanduser()
    home.mkdir(exist_ok=True)
    # Downloaded every time, because a name comes back once its export is deleted on the
    # headset, and two headsets can use the same name, so a folder already there can hold
    # another export.
    with tempfile.TemporaryFile() as f:
        with urllib.request.urlopen(arg) as r:
            shutil.copyfileobj(r, f)
        f.seek(0)
        root = unpack(f, home, fresh=True)
elif arg.endswith(".tar"):
    tar = Path(arg).expanduser()
    with tar.open("rb") as f:
        root = unpack(f, tar.parent)
else:
    root = Path(arg).expanduser()
ds = LeRobotDataset("local/" + root.name, root=root)
poses = [f"observation.pose.{name}.absolute" for name in ("hmd", "left_hand_root", "right_hand_root")]
poses = [k for k in poses if k in ds.features]
joints = ds.features["observation.joint_states"]["names"]
# Streamed live, the viewer follows the decoder, faster than real time, and jumps to each
# new episode as it arrives; written to files first, every episode opens complete.
out = Path(tempfile.mkdtemp(prefix="rerun_view-"))
files = []

for i in range(ds.num_episodes):
    rec = rr.RecordingStream(root.name, recording_id=f"{root.name}/{i}")
    files.append(out / f"episode_{i:06d}.rrd")
    rec.save(files[-1])
    rec.log("task", rr.TextDocument("\n".join(ds.meta.episodes[i]["tasks"])), static=True)
    rec.log("world", rr.ViewCoordinates.RIGHT_HAND_Z_UP, static=True)
    for key in poses:
        # Transform3D() clears every field it is not given, and static data hides temporal
        # data, so it would pin each pose to the origin.
        rec.log("world/" + key.removeprefix("observation.pose.").removesuffix(".absolute"),
                rr.Transform3D.from_fields(axis_length=0.05), static=True)

    paths = {key: [] for key in poses}
    for idx in range(ds.episode_data_index["from"][i].item(), ds.episode_data_index["to"][i].item()):
        row = ds[idx]
        rec.set_time("time", duration=row["timestamp"].item())
        for cam in ds.meta.camera_keys:
            picture = (row[cam].permute(1, 2, 0).numpy() * 255).astype(np.uint8)
            rec.log("camera/" + cam.removeprefix("observation.image."), rr.Image(picture).compress(jpeg_quality=90))
        for j, joint in enumerate(joints):
            rec.log(f"gripper/{joint}", rr.Scalars(row["observation.joint_states"][j].item()))
        for key in poses:
            p = row[key].numpy()
            paths[key].append(p[:3])
            # Bare xyzw: rr.Quaternion converts with numpy 2's `asarray(copy=)`, so under
            # the pinned numpy 1.26 it drops every rotation.
            rec.log("world/" + key.removeprefix("observation.pose.").removesuffix(".absolute"),
                    rr.Transform3D(translation=p[:3], quaternion=p[3:]))

    # Not under the pose's own entity, which would carry the path along with it.
    for key, path in paths.items():
        rec.log("world/path/" + key.removeprefix("observation.pose.").removesuffix(".absolute"),
                rr.LineStrips3D([np.array(path)], radii=0.002), static=True)
    # Closes the file: the viewer waits on an open .rrd for more data and never opens.
    rec.disconnect()

subprocess.run([sys.executable, "-m", "rerun", *map(str, files)])
