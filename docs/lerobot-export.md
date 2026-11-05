# LeRobot export

The viewer turns any set of episodes into one LeRobot **v2.1** dataset on the headset, and offers
it as `<name>.tar`. The format follows LeRobot v2.1, as
[rebake](https://github.com/airoa-org/rebake) writes it, plus one `observation.state` and one
`action` column for trainers that read a single vector of each. Tools that read LeRobot v3.0
need one conversion step ([LeRobot v3.0](#lerobot-v30)).

## Folders

Episodes can be filed under folders such as `cups/day1`. The folder is kept in the episode's
`folder.txt`, and the files stay flat in `files/episodes/`, because the uploader, its state
database and core's GC all read that directory one level deep. `folder.txt` travels with the
episode to the bucket and through `pull.py`. No file means the top level.

- **New episodes** go to the folder in `config.json`'s `folder`, which the "new episodes go to"
  field on the `:8080` page's Setup tab sets. Empty is the top level.
- **Moving** episodes: tick them in the viewer and press **move**
  (`POST /episodes/<episode>/folder` with `{"folder": "a/b"}`).
- A folder is `/`-separated names. `.`, `..` and control characters are refused.

## Using it

1. Open `http://<headset>:8080/viewer`. The sidebar is a tree of folders, then episodes.
2. Tick episodes, or tick a folder to tick everything under it, and press **export LeRobot**.
   Name the dataset in the dialog and press **export on the headset**. The same dialog shows
   the command to export on your own computer instead (below).
3. **LeRobot exports**, near the top of the sidebar under **Live**, shows progress, then each dataset with a
   download link and any warnings.
4. An export takes every core, so it refuses to start while the rig is armed or recording.
   One export runs at a time, and it stops if a recording starts.

Over HTTP: `POST /exports` with `{"name": ..., "episodes": [...], "skip_failed": false, "skip_untracked": false}` starts one, `GET /exports`
lists them with the running job, `GET /exports/<id>.tar` downloads one and
`DELETE /exports/<id>` removes it. Datasets live in `files/exports/<id>/`.

The episodes in one dataset must share a robot type and a set of cameras, because a dataset has
one feature list.

## On your own computer

`yubi-export` runs the same export on a computer, from a clone of this repository. It is faster
than the headset and leaves the headset free to record. It runs on macOS and Linux.

1. Install Rust (<https://rustup.rs>) and ffmpeg with libx264 on `PATH`. On a Mac,
   `brew install ffmpeg`. On Ubuntu, `sudo apt install ffmpeg`.
2. Clone the repository. For a dataset that matches the headset's own export exactly, check
   out the commit the headset was built from. Each episode's `meta.json` names it under
   `programs[].source.git.hash`. A different commit still works, but its output can differ
   wherever the export code changed between the two.

   ```bash
   git clone https://github.com/airoa-org/yubi-go && cd yubi-go
   ```

3. Copy the episodes off the headset. `pull.py` is served by the headset and needs only
   Python 3.8+. It writes the names it chose to `episodes.txt`.

   ```bash
   curl -s http://HEADSET:8080/pull.py | python3 - http://HEADSET:8080 ~/yubi-go/episodes --folder cups/day1
   ```

4. Export them. The first run builds `yubi-export`, which takes a few minutes because it
   compiles the whole app.

   ```bash
   cargo run --release -p yubi-export -- ~/yubi-go/episodes ~/lerobot/cups $(cat ~/yubi-go/episodes/episodes.txt)
   ```

The viewer's **export LeRobot** dialog prints steps 3 and 4 as one command under
**On a computer**, filled in with the ticked episodes, the dataset name and the two
**leave out** switches. Run it in the clone. When the ticked episodes are a whole folder, the
command names the folder (`--folder pencil`) instead of every episode. The export then reads
`episodes.txt`, so the dataset holds exactly the ticked episodes, even when an older pull left
other episodes of that folder in `~/yubi-go/episodes`.

`yubi-export` takes named episodes, every episode under `--folder`, or every episode, and
`--skip-failed` and `--skip-untracked` as the dialog's switches. The output is a dataset
directory, not a tar.

Only the video step differs from the headset. ffmpeg decodes, x264 encodes at `crf 20` with
every frame a key frame, and the crop, resize, colour stats and mp4 muxing are the headset's
own code, so the tables match the headset's export and only the video encodes differ.

## What it writes

- One LeRobot episode per subtask (`meta.json` `segments`). An episode
  without subtasks is one LeRobot episode, labelled with the episode's label. Episodes go in
  oldest first.
- A subtask retried after B is one LeRobot episode per try, each with its own `task_success`.
  **leave out failed subtasks** in the dialog (on by default; `--skip-failed` for yubi-export,
  `"skip_failed": true` over HTTP) drops the failed tries and keeps the rest of the episode, so an
  episode that failed late still gives its finished subtasks.
- **leave out subtasks with a tracking loss** (on by default; `--skip-untracked`,
  `"skip_untracked": true`) drops every subtask during which a hand or the headset lost tracking,
  as the viewer's red shading shows it, and adds a warning naming the episode, the subtask and the
  part that was lost.
- Each subtask of a recording becomes one LeRobot episode. Its frames carry the subtask's number
  twice, with the same value: in `task_index`, which LeRobot reads, and in
  `primitive_action_index`, rebake's name for it. `tasks.jsonl` maps each number to the subtask's
  label, numbered in the order the labels first appear in the dataset.
- A recording of "fold the towel" with the subtasks "spread" and "fold in half" becomes two LeRobot
  episodes, and both carry `"short_horizon_task": "fold the towel"` and the recording's `uuid` in
  `episodes.jsonl`, so you can group them back into one recording. "fold the towel" gets no number
  in `tasks.jsonl`, because every entry there must be a label that frames carry, and some trainers
  refuse a dataset with an entry that no frame uses.
- Pose features `observation.pose.*.absolute`, `.relative`, `.is_fresh` and
  `action.*.relative`, and the left-to-right hand root, whose step is named
  `observation.pose.left_hand_to_right_hand.relative`.
- `observation.joint_states` and `action.joint_states` (the next tick's angles), from
  `/joint_states`. An episode where either gripper was never calibrated is refused.
- `observation.state` (11): the right hand root in the left hand root's frame, as xyz and the
  first two columns of its rotation matrix, then `observation.joint_states`.
- `action` (20): the left and right hand roots in `quest_origin`, each as xyz and the first two
  columns of its rotation matrix, then `action.joint_states`.
- `synched_timestamp_ns`, each row's tick on the recording clock. It is listed in `info.json`,
  though rebake leaves it out, because LeRobot v3.0 refuses columns that `info.json` does not list.
- `observation.image.{left,right,center}`: the head camera takes the centre slot. Every
  video is 640x480 H.264 with every frame a key frame, so a trainer can seek to any frame. The
  headset encodes at 7.4 Mbps on its hardware encoder. The head module's stereo frame
  contributes its left eye, centre-cropped to 4:3.
- `meta/info.json`, `tasks.jsonl`, `episodes.jsonl` (with the airoa-metadata block), and
  `episodes_stats.jsonl`.

## LeRobot v3.0

Current LeRobot releases read v3.0, so convert the export once with LeRobot's converter. It works
in place and keeps the v2.1 copy as `<name>_old`:

```bash
mkdir -p ~/lerobot-cache/yubigo && cp -r ~/lerobot/cups ~/lerobot-cache/yubigo/cups
HF_HUB_OFFLINE=1 python -m lerobot.datasets.v30.convert_dataset_v21_to_v30 \
    --repo-id yubigo/cups --root ~/lerobot-cache --push-to-hub=false
```

A trainer that takes one state and one action vector reads `observation.state` (11 values) and
`action` (20 values). The wrist cameras are `observation.image.left` and `observation.image.right`.
The episode's label is `short_horizon_task` in `episodes.jsonl`, and the subtask labels are in
`tasks.jsonl`.

`action` stores absolute poses, because a pose relative to a chunk's first row depends on where
the chunk starts, so it cannot be one column per frame. A trainer that wants relative actions
computes them from these rows.

## How it samples

- A 30 Hz grid from the latest first sample of any stream to the last sample. Rows run
  `timestamp = frame_index / 30` exactly, and video frame `i` sits at `i / 30` s.
- Poses are interpolated between the samples either side of a tick (linear position,
  normalised quaternion blend), and held across a gap over 25 ms.
- Joint angles are held from the last `/joint_states`, not interpolated, as rebake does. The
  encoders send about 200 readings a second, so a held angle is at most about 5 ms old.
- **Cameras show the frame nearest each tick**, which halves the worst-case image age
  (33 ms → 17 ms at 30 Hz).
- A frame and the jaws are placed by the built-in delays in `assets/calib/default.json`,
  never by a fit saved on the headset, so every headset and the export on a computer give the same result. The
  delays are 0, so frames and jaws sit at their stamps.
- `.relative` is the step from the previous tick in the previous tick's frame. The first tick
  of the episode is the identity.
- `.is_fresh` says a source took a new sample since the previous tick: the hand or headset
  pose, the gripper for the fingertips, the camera frame for images.
- The hand chain is rebuilt from the raw controller poses with today's CAD mount
  (`misc::mount`), so an episode exports alike whichever build recorded its `/tf`.

## Known limits

- The headset's bitrate (7.4 Mbps) is not yet checked for picture quality on the Quest's
  encoder.
- The export warns about pose jumps (over 7 cm off a constant-velocity guess) and pose gaps
  over 25 ms. With **leave out subtasks with a tracking loss** on, it also warns about each
  subtask it leaves out. Swapped wrist cameras are not detected.
