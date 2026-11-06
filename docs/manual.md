# YUBI Go manual

YUBI Go is an app for Meta Quest 3 / 3S. It records demonstrations from two handheld grippers, the
YUBI Gloves of [Toyota/yubi-hw](https://github.com/Toyota/yubi-hw).
Each gripper carries a Quest controller, a wrist camera and an angle encoder. An optional third camera
is fixed to the headset. Everything plugs into the headset's USB-C port through one hub, and the
headset stores the episodes itself. No laptop is needed while recording.

## Contents

1. [Hardware](#1-hardware)
2. [Install and first setup](#2-install-and-first-setup)
3. [Standalone and production](#3-standalone-and-production)
4. [Controls](#4-controls)
5. [The panel](#5-the-panel)
6. [Recording an episode](#6-recording-an-episode)
7. [Why a start is refused](#7-why-a-start-is-refused)
8. [Calibrating the grippers](#8-calibrating-the-grippers)
9. [Assigning the cameras](#9-assigning-the-cameras)
10. [The :8080 page](#10-the-8080-page)
11. [Episodes on disk](#11-episodes-on-disk)
12. [Uploads](#12-uploads)
13. [Files in the app's folder](#13-files-in-the-apps-folder)
14. [Troubleshooting](#14-troubleshooting)
15. [Safety](#15-safety)
16. [Privacy and data](#16-privacy-and-data)

---

## 1. Hardware

| Part | What it is |
|---|---|
| Headset | Quest 3 or 3S. It runs the app, records, and serves the `:8080` page. |
| Hub | One USB hub on the headset's only USB-C port. While it is plugged in, adb works only over Wi-Fi (section 2.2). |
| Wrist cameras | Two USB webcams, one per gripper (left wrist, right wrist). |
| Head camera | A USB camera fixed to the headset. It is optional, and it can be a stereo module (`3D USB Camera`, `32e4:0035`), which records both eyes side by side at 3840×1080 (two 1920×1080 eyes) and 30 fps. |
| Gripper encoders | One AS5601 board per gripper. Each streams the jaw angle at about 200 lines/s over USB serial. |
| Controllers | One Quest controller per gripper. They give the wrist poses and the buttons. |

YUBI Go does not use the headset's own cameras.

## 2. Install and first setup

You need a computer with `adb` and the headset in developer mode ([`README.md`](../README.md)).

### 2.1 Install

Install the APK and grant the two camera permissions as the README's
[Install](../README.md#install) section shows. A few things go wrong often:

- **Keep the hub unplugged during `adb install`.** If you install with the hub attached, both
  gripper boards can stop responding until you replug them.
- To update, install the new APK the same way. The camera permissions stay, so only the
  `adb install` line is needed. If a permission is missing, the app asks for it at launch.
- `INSTALL_FAILED_UPDATE_INCOMPATIBLE` means a build signed by someone else is installed. Copy
  `/sdcard/Android/data/org.airoa.yubigo/files/` off the headset before `adb uninstall`, because
  uninstalling deletes the episodes.
- While YUBI Go is in the foreground, system prompts (such as the USB debugging prompt) open behind
  it. Press the Meta button to see and answer them.

### 2.2 adb over Wi-Fi (optional)

The hub takes the headset's only USB-C port, so adb works only over Wi-Fi while the hub is
plugged in. The README's [Install](../README.md#install) section has the commands. Run them while
the cable is still in, and run them again after every headset reboot, because Wi-Fi adb stops
when the headset reboots. Anyone on the same network can then connect to the headset's adb, so
use Wi-Fi adb only on a network you trust.

### 2.3 First launch

With no `config.json`, the app starts in standalone mode with the task `untitled`. That is enough
to record. To set the rig up, open the `:8080` page (section 10). The other first steps are:

1. Calibrate the grippers (section 8).
2. Choose or type a task on the `:8080` page.

The cameras assign themselves (section 9).

## 3. Standalone and production

The rig runs in one of two modes, and the panel shows the mode as `Standalone` or `Production`.
Production mode works with AIRoA's data-collection backend and its dashboard, which assign tasks
and collect episodes.

**Production mode needs AIRoA's backend and dashboard, which outsiders do not have.** Without
them, run the rig standalone. Everything in this manual marked "production" applies only to a rig
connected to that backend.

| | Standalone | Production |
|---|---|---|
| Needs AIRoA services | No | Yes: the backend API (`…:8000/api`) and an API key |
| Where the task comes from | `selected_task` in `config.json`, else the first entry in `tasks`, else `untitled` | The backend's assignment queue (cached in `assigned.json`): its head, or with `operator_picks_task` the oldest episode of the task the operator picked |
| Subtasks | The task's own `subtasks`, in order, else one step named after the task | The assignment's subtasks, in order |
| Operator login | Not needed | An operator must have this robot open in the dashboard's teleop screen, unless `require_operator` is `false` |
| When nothing is assigned | Not possible | The backend repeats the last episode. If the backend cannot be reached, the start is refused. |

**How the mode is chosen.** The rig is standalone when `config.json` says `"mode": "standalone"`.
It is also standalone when `backend.base_url` or `backend.api_key` is missing, even if the mode
says `production`. In that case the log says
`config.json: production mode with no backend.base_url and api_key — standalone`.

## 4. Controls

### 4.1 Buttons

| Input | What it does |
|---|---|
| **A** (right) | Starts recording. While recording, it stops and saves the episode as a **success**. |
| **B** (right) | During a subtask, it marks the subtask **failed** and waits, so you can reset the scene. A double-click then tries the same subtask again. While it waits, B ends the episode. On a task with one step, B ends the episode as a failure at once. When not recording, it says the headset battery aloud: the charge in 5% steps, whether it is charging, and about how long is left in 15 min steps. The time left is unknown for the first few minutes after launch, because the battery must first drop 2%. |
| **Y** (left) | Starts the gripper calibration. A second press abandons it. Y does nothing while recording. |
| **≡** (left menu button) | Says aloud whether the rig is ready. When it is ready, it also says the task, once the headset has made the task name's audio. When it is not, it says the first reason a start would be refused. While recording, it says "Recording. All good.", or the first fault. |
| **Either trigger, held** | Shows a large QR code for the `:8080` page, for as long as you hold it. After 1 s, it also says the address aloud, for example "192.168.1.5:8080". |

### 4.2 Gripper double-click

You can also drive a recording with the gripper itself, without the buttons.

- A **click** is: close the jaw fully (to 3° or less), then open it again (to 8° or more).
  A jaw that stays closed longer than 500 ms is a grasp, so its release is not a click.
- A **double-click** is two clicks on the same gripper less than 0.6 s apart.
- After a double-click, the next 0.5 s of input is ignored.
- Double-clicks are ignored during calibration.
- A gripper that is not calibrated does not click, because the click is measured on the
  calibrated angle.

| When | A double-click |
|---|---|
| Not recording | starts recording |
| While recording | accepts the current subtask and moves to the next |
| On the last subtask | ends the episode as a success |
| After B marked a subtask failed | tries the same subtask again |

A standalone task with no subtasks is one step, so the second double-click ends the episode. After
the start and advance tones, the headset says the step the hands are now on. It says nothing when
`spoken step names` is off, or while the headset is still making that name's audio.

### 4.3 Holding still

Some tasks keep both hands on objects when a step ends, so neither gripper is free to
double-click. For such a task, tick "next subtask when both hands hold still for 1 s" on its card
in the Tasks section. The switch is off by default, because a pause to think also ends the step.

- Both controllers must stay under 3 cm/s and 0.2 rad/s for 1 s, and neither jaw may move more
  than 2°.
- The step ends where the stillness began. When the second is up, the advance tone plays and the
  headset says the next step.
- On the last subtask, holding still ends the episode as a success, as a double-click does.
- A hold begins only after the headset has been quiet for 0.5 s, so freezing to listen to a
  spoken step does not count.
- The step a hold starts begins at the hands' first motion, so the time you stay frozen to hear
  its name is left out. Motion means either controller over 3 cm/s or 0.2 rad/s, or a jaw moving
  more than 2°. A step started by a double-click still begins where the second click opens. The
  recording keeps every sample: only the boundary in `meta.json` moves.
- The hands must move before another hold counts. A hold while the episode waits after **B** does
  nothing: only a double-click tries the step again.
- Only standalone tasks have the switch, because production tasks come from the dashboard.

### 4.4 Sounds and vibration

Each event plays a sound and vibrates the controllers. Some events also speak.

| Event | Sound and voice | Vibration |
|---|---|---|
| App started | a droplet, a swelling chord, then two soft glassy notes | none |
| Recording started | rising two tones, then the step | short, strong |
| Subtask accepted | one tone, then the next step | short, light |
| Recording stopped (success) | falling two tones | medium |
| B during a subtask | two low beeps, "Marked failed. Reset, then double-click to retry." | long, low |
| B that ends the episode (one-step task, or while waiting for a retry) | two low beeps, "Marked failed." | long, low |
| Subtask reaches 120 s | two low beeps, "Recording stopped. Subtask over two minutes." | long, low |
| Start refused | "Not ready." and the first reason | longer, strong |
| Fault while recording | three high pips (at most every 10 s), then the fault, for example "Left camera stalled. Replug the hub." | medium, strong |
| Recording cancelled | the fault pips, a falling harsh tone, then "Recording cancelled." and the reason | longest |
| Battery or disk running out (15 min, then 5 min left) | three falling tones, then for example "Battery low. About fifteen minutes left." | long |
| Subtask at 90 s (30 s before the 120 s limit) | "Thirty seconds left." | medium |
| Cameras assigned (section 9) | three rising notes, "Cameras assigned." | short, light |
| Charger plugged in or out | "Charger connected." / "Charger disconnected." | none |
| The charger keeps dropping out | "Power keeps dropping. Use a stronger charger." | none |

## 5. The panel

The panel floats in front of you over the passthrough view.

### 5.1 Camera row

Two tiles, `Left wrist` and `Right wrist`. With the head camera turned on (Setup tab, Cameras, or
`head_camera: true` in `config.json`; the default is off), a third tile, `Head`, sits between them.

| On the tile | Meaning |
|---|---|
| `3840x1080 30.0/30` (top right) | resolution, then measured / expected frame rate (here, the stereo head camera) |
| `Stalled, replug` | stalled: bytes arrive but no frames. Replug the hub. |
| `No signal` | no signal: the camera is absent or did not open |
| red wash on the tile | the camera is not live, or its rate is far too low |
| red frame around all tiles | the recording failed to write |

### 5.2 Under each tile

| Label | Meaning |
|---|---|
| `Tracked: 72Hz` | the controller (or headset) is tracked, at this rate |
| `Not tracked` | not tracked: the pose is a guess |
| `No data` | no data at all |
| jaw bar + Hz | how far the gripper is open, and the encoder rate |
| `No data` (jaw) | the encoder sends nothing: unplugged or stopped. This wins over `Calibrate`. |
| `Calibrate` (jaw) | the gripper needs calibration |
| `85% 1h 05m` / `85% full in …` / `charging` / `plugged in` | headset battery: time left on battery / time to full / charging / on power but not charging |

The headset's tracking and battery rows sit under the head tile, so a rig without
`head_camera: true` does not show them.

### 5.3 Task band

| Label | Meaning |
|---|---|
| `Task: <name>` | the current task |
| `Step: 1/2 pick` | the current subtask and its number. A task with one named subtask shows only the name (`Step: pick`). A task with no subtasks has no `Step:` line. After a failed try, `· double-click to retry` follows. |
| `No task assigned` | no task assigned |
| `REC` | recording |

### 5.4 Storage, network, identity

| Label | Meaning |
|---|---|
| `1h 05m to record` | recording time left on the disk |
| `No room to record` | no room to record |
| `X / Y free` | used / free space |
| `No upload` | uploads are off (no `storage.yaml`) |
| `No target` | `storage.yaml` exists but has no usable target. The line below says why (`Not uploading: …`). |
| `Sending 3/10`, `Queued 4`, `All sent` | uploading file 3 of 10 / 4 waiting / all sent |
| `Unreachable`, `Retrying` | upload target unreachable / retrying. The line below gives the error (`Bucket unreachable: …`, `Retry failed: …`). |
| `12 eps` | episodes on the headset |
| `unbound 2` | production only: episodes in a row recorded with no backend assignment behind them |
| `Backend down 5 min` | production only: the backend has not answered for that long |
| `Production` / `Standalone` | production / standalone |
| robot id, IP, `:8080` | where to open the page; the small QR code points there too |
| `No network` | no network |

### 5.5 Key hints

`Y Calibrate`, `A Record` / `Stop`, `B Failed` (mark failed, while recording) / `Battery` (say the
battery, when not recording).

### 5.6 Centre messages

The centre of the panel shows a running calibration, otherwise why a start was refused. A refusal
shows `Cannot start recording`, then `The rig is not ready`, then the reasons (section 7). While
the centre shows a message, it covers the task band and the status rows.

## 6. Recording an episode

1. Look at the panel. Every camera tile should show its rate, and both jaws should move when you
   squeeze.
2. Start: press **A**, or double-click a gripper.
   - A double-click start keeps the double-click itself: the recording begins where the first
     click started to close, and the episode's usable part begins where the second click opens.
     A start with **A** begins at the press.
3. Perform the task. If there are several subtasks, double-click at the end of each one.
   - If a subtask goes wrong, press **B**. The try is saved as a failed segment, and the episode
     waits. Reset the scene, then double-click to try the same subtask again. The reset is not
     part of any segment, and it does not count toward the 120 s limit.
   - On a task with one step, B ends the episode as a failure instead, because a retry would only
     repeat the episode.
4. End the episode:
   - **A**, or a double-click on the last subtask: the episode is saved. It is a success when every
     subtask ended in a success.
   - **B** while the episode waits after a failure: the episode is saved as a failure, with the
     subtasks done so far. A finished episode cannot be marked failed afterwards, because it may
     already be uploaded as a success.

### What ends an episode on its own

| Situation | Result |
|---|---|
| A camera, encoder, controller or the headset stops sending data, or a camera stalls | The episode is **cancelled and deleted**. Set `keep_faulted_episodes: true` in `config.json` to keep recording instead. |
| One subtask runs over 120 s | The episode stops and is saved as a failure. |
| A frame rate is far too low, or tracking is lost | The fault pips play (at most every 10 s), and recording continues. |
| The disk fills or a write fails | The episode is deleted. |
| The episode did not close cleanly (for example, a camera that was live recorded nothing) | The episode deletes itself, and the log says why. |

**Launching the app deletes every unfinished episode** (a folder without `meta.json`). If an episode was
cut off by a crash and you want its data, copy it off before you launch the app again.

## 7. Why a start is refused

The app checks the rig continuously. If anything is wrong, a start is refused: the controllers
vibrate, the voice says "Not ready." and the first reason, and the panel lists every reason as
`<source>: <problem>`.

| Source | Part |
|---|---|
| `left wrist cam` / `right wrist cam` / `head cam` | left wrist / right wrist / head camera |
| `left gripper` / `right gripper` | left / right gripper encoder |
| `left controller` / `right controller` / `headset` | controllers / headset tracking |
| `task` / `storage` / `battery` | task / storage / battery |
| `cameras` | the two wrist cameras as a pair |
| `grippers` | both gripper encoders, for calibration (`grippers: needs calibration`) |
| `hub` | every camera and encoder at once: they are all silent, so the hub is the likely cause (`hub: no data`) |
| `gate` | the rig as a whole, when no single part is to blame (`gate: settling`, `gate: airoa_core not running`) |

| Problem | Meaning | What to do |
|---|---|---|
| `no data` | no data | check the cable or the hub |
| `stalled, replug the hub` | the camera is stalled | replug the hub |
| `rate far too low` | a camera runs under 80% of its expected rate | check the cable; try another hub port |
| `needs calibration` | the gripper is not calibrated | press Y (section 8) |
| `turn a gripper to match` | the app does not yet know which wrist camera is which | turn one gripper for a few seconds (section 9) |
| `not tracked` | the pose has been a guess for 1 s or more | pick up the controller, look at it |
| `lost` | no pose at all for 1 s or more | wake the controller, or check its battery |
| `nothing assigned` | no task (production) | assign an episode on the dashboard |
| `disk too full` | under 2 GB free | delete or pull episodes |
| `battery too low` | under 5 minutes of battery | charge |
| `settling` | waiting for the part to be stable again | wait: each check must pass for 1 s after its fault clears |
| `airoa_core not running` | airoa_core, the recording library that ships inside the APK, is not running | reinstall the APK; read `adb logcat -s yubigo` |

airoa_core can also refuse a start. Its reason shows as one or two rows: what is wrong, then what
to do. The operator and assignment rows appear only in production. A reason not in this table is
shown as airoa_core gives it.

| Rows | Meaning |
|---|---|
| `No operator logged in` / `Open this robot's dashboard teleop screen` | no operator has this robot open |
| `Cannot check the operator` / `The backend is unreachable` | check the network and `backend.base_url` |
| `Nothing assigned` / `Assign an episode on the dashboard` | the queue is empty |
| `Every assignment is done` | every queued episode is recorded |
| `The requested episode was not found` | the episode the operator picked is gone from the queue |
| `Cannot fetch the assignment` / `The backend is unreachable` | check the network and `backend.base_url` |
| `The gate is closed` | the rig is not ready (see the reasons above) |
| `The episode has no subtasks` | the assignment has no subtasks; fix it on the dashboard |
| `Cannot start in this state` | airoa_core is in the wrong state for a start |
| `Already recording`, `The recorder is not running` | the recorder is in the wrong state for a start |
| `Recorder error` | the recorder failed; the next row gives its message |

The head camera is not checked when `head_camera` is `false`.

## 8. Calibrating the grippers

The app needs each gripper's fully closed and fully open angle. Without them the jaw shows
`Calibrate` and a start is refused.

1. Press **Y** while not recording. The panel and the `:8080` Rig tab show `Calibrating the grippers`.
2. `Close both grippers fully`, and hold them closed for half a second.
3. `Open both fully and hold them still`. The bars `Left` / `Right` fill. Each side needs about 1 s without moving.
4. When both bars are full, calibration is saved in `calibration.json` and reused at every launch.

If it fails, the panel says `Calibration failed` and why, then `Press Y to retry`:

- `left gripper: no data`: no data from that encoder for about 1 s.
- `left gripper barely moved (0.50 rad)`: the jaw moved less than 0.6 rad (about 34°).
- `left gripper: past half a turn (3.20 rad)`: the reading wrapped past half a turn.

## 9. Assigning the cameras

The app assigns the cameras by itself, so there is no button for it.

- **The head camera**, when it is turned on (`head_camera`), is the camera whose picture size
  differs from the other two, for example a stereo module at 3840×1080 against wrists at 640×480.
- **The wrist cameras** are told apart by motion. Each wrist camera's picture moves with its own
  gripper, so the app compares each picture with how both controllers turn.

A start is refused with `cameras: turn a gripper to match` until the wrists are matched. When
nothing else is wrong, the voice says "Cameras not matched yet. Turn one gripper around for a few
seconds." The app needs this after every launch, and again after both wrist
cameras were unplugged at once (for example a hub replug), because then either camera can come
back on either USB port. To match them, turn one gripper around for a few seconds. Ordinary
handling of the grippers does it too: in recorded episodes it took 3.5 to 7 s of motion. You then
hear three rising notes and "Cameras assigned". If the wrists were swapped, the app swaps them
back before it says so.

To check the match, wave one hand alone within 15 s after that. The voice then says once which
camera moved, for example "Left camera is moving". If it names the other hand, the match is
wrong: replug the hub and match again.

- When only one wrist camera drops and comes back, it returns to its own slot, and the
  cameras stay matched.
- The cameras never move during an episode.
- To pick the cameras by hand instead, use the `:8080` page (Setup tab, Cameras). Each slot has a
  list of `camera 1`, `camera 2`, …, numbered by hub port. Each port gets the next number the
  first time a camera appears on it, and `camera_ports.json` keeps the numbers across replugs and
  launches. The app saves the
  choice as the USB port of each camera (`camera_ports`). A choice made there counts as matched.
  Use this when the head camera has the same picture size as the wrists. To see which camera is
  which, watch the live streams in the Viewer tab.

## 10. The :8080 page

Open `http://<headset IP>:8080` on any phone or laptop on the same network. Hold either trigger
in the headset to see a QR code for it. **Anyone on the network can use the page; there is no
PIN.** Starting and stopping a recording is not on the page; only the headset does that.

Open the page by IP address, or by the headset's own name, `yubi-go-<robot id>.local` (section 16
says when it has one). Through `adb forward`, `localhost` works too. The server refuses (403) any
other name for the headset, such as a router hostname or an `/etc/hosts` alias, because a page on
another site could reach the headset through such a name.

The header shows the recording state (`IDLE`, `REC 12s`, or `WRITE FAILED`), the headset battery
and the headset's name.

The page has three tabs: **Rig**, **Setup** and **Viewer**.

### Rig tab

- **Readiness**: `ready to record`, or the list of reasons a start would be refused.
- **Task**: the task and step, `collected — record another` after a production episode, or
  `no task assigned`.
- **Calibration**: the calibration prompt while it runs, and each side's travel in degrees.
  Calibration is started with Y in the headset; the page has no button for it.
- **Backend** (production only): whether the backend answers, and how many episodes are unbound.
- **Rig**: the rate, resolution and MB/s of every camera, the rate of each encoder, the headset's
  and each controller's tracking, then battery, disk, recording time left, episodes on disk, upload
  state and the headset's address.

### Setup tab

- **Change task**: tap a task to make it the current one. Standalone lists the tasks typed under
  Tasks. Production shows this section only with `task choice: operator`, and then lists the tasks
  queued on the dashboard with how many episodes each has left. A task never runs out: when a
  recording uses its last episode, the rig asks the backend to repeat that episode, so one episode
  per task on the dashboard is enough. Until the copy arrives, about a second after the recording,
  the start is refused with `task: nothing assigned`.
- **Tasks** (standalone only): one card per task, with its name and its subtasks in order. `+ Add
  task` adds a card, and `+ Add subtask` (or Enter in a subtask) adds a step. `✕` removes a subtask,
  or on the card the whole task. The card's "next subtask when both hands hold still for 1 s" box
  turns on section 4.3 for that task. `Save tasks` writes them to `config.json`, and the next
  episode uses them.
  After a save the headset makes each name's audio (about 1 s each). `▶` plays it on the phone,
  and it blinks while the audio is still being made.
- **Cameras**: `camera on the headset` (yes or no, `head_camera`), and a `camera N` list for each
  slot (left wrist, right wrist, head), as section 9 describes. A change is saved at once. It warns
  when fewer cameras are found than needed.
- **Configure**: the settings in `config.json`, which the next episode uses. A list or the folder
  is saved as soon as you change it. The mode, URL and key are saved with `Save`.
  - `video`: `h264` or `jpeg` (section 11).
  - `spoken step names`: on, the headset says each subtask's name at its tone; off, tones only.
  - `new episodes go to`: the folder the next episode is filed under in the Viewer. Blank is the top
    level.
  - `mode`: `standalone` or `production`.
  - Production only: the backend URL and API key, `operator login` and `task choice`. The API key is
    never shown back. Leave it blank to keep the current one, but type it again when the URL
    moves to another host.
- **Metadata**: organization, site, location, robot type, robot id and operator. The section has
  its own `Save`, and the next episode uses the values. In production they come from the backend
  and cannot be edited here.
- **Uploads**: the bucket that finished episodes are sent to (section 12): `endpoint` (with
  `http://` or `https://`), `bucket`, `prefix`, `region` (default `us-east-1`), `access key`,
  `secret key`, and `after upload`: keep the sent episode for `local_retention_hours` (24 h unless
  `storage.yaml` sets it; 0 keeps it until you delete it), or delete it once it is sent. `Save` writes
  `storage.yaml` and keeps the settings the form does not show, such as a `gc:` block. The line
  above the button says within 15 s whether the bucket answers. The keys are never shown back.
  Leave them blank to keep the current ones, but type them again when the endpoint moves to
  another host. `Turn off` removes the file, and episodes then stay on the headset.
- Settings cannot be changed while recording
  (`recording — configuration is latched until the recording ends`).
- **Reset** asks you to type `delete` before it acts, and is refused while recording or while a
  LeRobot export runs.
  **Reset configuration** removes `config.json`, `storage.yaml`, the backend's cached
  `robot_me.json` and `assigned.json`, the gripper calibration, the camera calibrations and
  `camera_ports.json`; the episodes stay. **Delete data** removes every episode and LeRobot export.
- **Licenses**, at the bottom, links to `/licenses`, the licence texts of YUBI Go and everything it
  bundles.

### Viewer tab

- The episode viewer (`/viewer`), under the console's header: **Live** and every recorded episode. It is also its own page at `/viewer`.
- The console's URL follows the tab and the selection (`/#rig`, `/#setup`, `/#viewer/live`, `/#viewer/episode_…`), so a link or a reload opens the same view.
- **Copy to a computer**, at the bottom of the viewer's sidebar, shows a command that copies every finished episode the computer does not
  have yet. It needs only Python 3, resumes an interrupted file, and never deletes anything on the
  headset:

  ```bash
  curl -s http://<headset>:8080/pull.py | python3 - http://<headset>:8080 episodes
  ```

  The command runs a script fetched over plain HTTP, so use it only on a network you trust.

### The episode viewer (`/viewer`)

- The sidebar lists the folders first, by name, then each folder's finished episodes, newest first.
  `↑`/`↓` move between episodes, and the URL (`/viewer#episode_…`) opens one directly.
- Each row has a checkbox for downloading, moving to a folder, exporting or deleting several episodes at once. Shift-click
  a row or its checkbox to set every episode between it and the last one opened or ticked, and
  `select all` ticks every episode. **move** asks for a folder; blank is the top level.
- The main view plays the selected episode from the headset: the three cameras on one timeline, a 3D
  view of the controllers, the gripper jaws and the headset with each hand's and the head's trail,
  and both gripper angles with the subtask boundaries. Both video settings play.
- `space` plays and pauses, `←`/`→` step one frame, and with `shift` one second. A menu sets the
  speed (0.25× to 4×). Click or drag on the gripper plot to jump, or click a subtask's label to play
  that subtask alone. In the 3D view, drag to orbit, shift-drag or right-drag to pan, scroll to
  zoom, and double-click (or **reset view**) to go back; **follow** keeps the hands centred, and
  panning turns it off.
- **overlay** (on by default) draws the rig into the camera pictures. Playback covers only the
  subtasks unless **whole episode** is ticked. The `meta.json` link opens the episode's metadata.
- `download` saves the episode as one `.tar` (`signals.mcap`, then `meta.json`), and `delete` removes
  it after a confirmation. **download** in the selection bar saves every ticked episode as one
  `.tar`, a folder per episode, named after their folder. Neither has `folder.txt`, which only
  files the episode in this viewer.
- **export LeRobot** in the selection bar turns the ticked episodes into a LeRobot dataset, on the
  headset or on a computer. The dialog asks for a dataset name and has two **leave out** boxes,
  both ticked at first. An export on the headset is refused while the rig is armed or recording,
  and it stops if a recording starts. The finished datasets wait under **LeRobot exports** in the
  sidebar.
  [`lerobot-export.md`](lerobot-export.md) describes the dataset and the computer route.
- **Live**, at the top of the sidebar (`/viewer#live`), shows what the headset sees now in the same
  layout: the cameras at up to 30 fps, the 3D view with the last 10 s of trails, and the gripper
  angles. **While recording, the headset sends 10 fps** and serves the live
  view at its lowest CPU priority, so watching never takes time from the recording. The page stops
  both streams whenever its browser tab is hidden.

### For scripts

| Request | What it does |
|---|---|
| `GET /state` | everything the page shows, as JSON |
| `GET /episodes` | the finished episodes |
| `GET /episodes/<episode>/<file>` | one file; honours `Range`, so a download can resume |
| `GET /episodes/<episode>.tar` | the whole episode as one tar |
| `GET /episodes.tar?episodes=<episode>,<episode>&name=<name>` | several episodes as one tar, saved as `<name>.tar` |
| `GET /episodes/<episode>/view` | the viewer's index: frame times, gripper traces and poses, as JSON |
| `GET /episodes/<episode>/view/<camera>.mp4` | an `h264` camera as mp4 (`left`, `right`, `head`) |
| `GET /episodes/<episode>/view/<camera>/<n>.jpg` | frame `n` of a `jpeg` camera |
| `GET /pull.py` | the copy script above |
| `GET /viewer` | the episode viewer |
| `GET /live/<camera>.mjpeg?fps=30` | one camera, every frame up to `fps` (at most 30; 10 while recording; 10 without `fps`) |
| `GET /live/poses` | the 3D view's poses and the gripper angles, as server-sent events at the same rate |
| `GET /meshes/<file>` | the controller and finger meshes the viewer draws |
| `DELETE /episodes/<episode>` | delete an episode |
| `GET /episodes/<episode>/subtasks` | each subtask's `failed` and `lost` flags, for the export dialog (refused while armed or recording) |
| `POST /episodes/<episode>/folder` | file the episode under a folder: `{"folder": "a/b"}` |
| `GET /exports` | the LeRobot exports and their state |
| `POST /exports` | start a LeRobot export: `{"name": …, "episodes": [...], "skip_failed": …, "skip_untracked": …}`. Both `skip_` keys default to `false`. Refused (409) while armed or recording. |
| `GET /exports/<id>.tar` | a finished export as one tar |
| `DELETE /exports/<id>` | delete an export |
| `GET /camera-calibration`, `/camera-calibration/<id>` | the saved camera calibrations |
| `GET /licenses` | the licence texts |
| `GET /rerun_view.py` | a script that opens a LeRobot export in Rerun |
| `POST /config` | change settings: a JSON object with any of the `config.json` keys in section 13. A key left out stays as it is, an unknown key is refused, and the reply is 409 while recording. |
| `POST /storage` | write or remove `storage.yaml` (the Uploads section) |
| `POST /reset/config`, `/reset/data` | **Reset configuration** / **Delete data**; the body must be `{"confirm": "delete"}` |
| `POST /calibrate` | start or abandon calibration (refused while recording) |
| `GET /speech/<id>.wav` | a task or subtask name's audio, for the Tasks section's `▶` |
| `GET /stream?topic=/left_camera/image_raw` | MJPEG stream (`right_camera`, `head_camera` too) |
| `POST /record/start`, `/record/stop` | only through `adb forward` (from the headset itself) |

From a computer without the rig network, forward the ports first:
`adb forward tcp:18080 tcp:8080` and `adb forward tcp:19090 tcp:9090`.

A rosbridge server on `:9090` publishes the recording state (`/record_manager/recording`), the gate
level (`/recording_gate/gate_level`), the task status (`/task_sequence_manager/*`) and `/tf` at
10 Hz, for any rosbridge client. It sends only the topics a client subscribes to.

## 11. Episodes on disk

Episodes are in `/sdcard/Android/data/org.airoa.yubigo/files/episodes/episode_<unix ms>/`.
Each holds these files:

- **`signals.mcap`** (ROS 2, uncompressed, about 24 MB per second with JPEG cameras on both
  wrists and the stereo head camera):
  - the cameras, which follow the `video` setting (`config.json`). A standalone rig defaults to
    `h264`, and a production rig defaults to `jpeg`:
    - `h264`: `/left_camera/video`, `/right_camera/video`, `/head_camera/video`
      (`foxglove_msgs/CompressedVideo`, about 5x smaller)
    - `jpeg`: `/left_camera/image_raw/compressed`, `/right_camera/image_raw/compressed`,
      `/head_camera/image_raw/compressed`
  - `/yubi/gripper/{left,right}/joint_state` and `/joint_states` (jaw angle)
  - `/yubi_go/gripper/{left,right}/raw`, each encoder's raw reading
  - `/tf`, `/tf_static` (controller and headset poses, URDF frames)
  - `/quest/controller/{left,right}/…` and `/quest/hmd/…`: the raw OpenXR poses, velocities and
    tracking flags, and the controller buttons (`joy`)
  - gripper calibration and device ids
  - `/diagnostics` and `/yubi_go/events`: faults and other events during the episode
  - `/rosout`, the app's log lines during the episode
- **`meta.json`** (AIRoA metadata v2.0): the task label, success, the subtask segments, the robot,
  the organization, site, location and operator, the devices with their serial numbers, and the
  software versions.
- **`folder.txt`**, only when the episode is filed in a folder in the viewer: the folder, as `a/b`.

A folder with `meta.json` is a finished episode. Without it, the recording was cut off, and the next
launch deletes it. If airoa_core failed during the episode, `meta.json` says
`"schema_version": "unbuilt"` and keeps the raw fields, so the metadata can be rebuilt later.

To copy episodes to a computer, use the Viewer tab, or:

```bash
adb pull /sdcard/Android/data/org.airoa.yubigo/files/episodes/
```

## 12. Uploads

Uploads are off unless `files/storage.yaml` exists. Without it, episodes stay on the headset, and
the panel shows `No upload` in grey. Fill in **Uploads** on the Setup tab of the :8080 page (section
10) to write the file. To write it by hand instead, push a file of this shape:

```yaml
targets:
  rig:
    endpoint: http://rig:9000      # use https unless the bucket is on the same trusted LAN
    access_key: ...
    secret_key: ...
    bucket: data
    prefix: yubi-go
    path_rule: canonical
delete_after_upload: false
```

- Only the first enabled target is used, and the keys must be written inline.
- The app uploads only between recordings, never during one.
- What was already sent is recorded in `files/db/upload_state.db`, so a restart does not send it
  again.
- Without `storage.yaml`, the app deletes no finished episode: it stays until you delete it. The
  app still deletes an episode that was cancelled or did not close cleanly (section 6), and at
  launch it deletes every folder without `meta.json`.
- With `storage.yaml`, the app deletes each sent episode `local_retention_hours` after its folder
  last changed. The key is 24 when the file does not set it, so set `local_retention_hours: 0` to
  keep sent episodes until you delete them.
- `delete_after_upload: true` deletes each episode as soon as it is stored.
- A finished episode that is not sent yet is kept, however old it is.
- A folder without `meta.json` is an unfinished episode. The next launch deletes it, and so does
  the age sweep, sent or not, unless `local_retention_hours` is 0 or `delete_after_upload` is
  `true`.
- A target's `gc:` block runs airoa_core's own clean-up as well, by the rules that block sets.

## 13. Files in the app's folder

All in `/sdcard/Android/data/org.airoa.yubigo/files/`.

| File | What it is |
|---|---|
| `config.json` | settings. Write it from the Setup tab (section 10). A file pushed with adb belongs to the shell user, so the app can read it only after `chmod 644`; without that the rig falls back to standalone. Use the Setup tab when the file holds an API key. |
| `calibration.json` | gripper calibration |
| `camera_ports.json` | the hub port of each camera number (section 9) |
| `camera-calibration/` | the camera calibrations made in the viewer, one `<left>-<right>.json` per gripper pair |
| `storage.yaml` | upload target (section 12) |
| `assigned.json` | production: the cached assignment queue |
| `robot_me.json` | production: this robot's profile from the backend |
| `episodes/` | recordings |
| `exports/` | LeRobot datasets made in the viewer (section 10) |
| `usb-events.log` | one line per USB device opened or closed, kept across reboots. At 1 MiB it moves to `usb-events.log.1`, which replaces the previous one. |
| `db/` | airoa_core's databases: the offline report queue and the upload state |
| `airoa/` | the airoa_core payload, copied from the APK |
| `tts/` | the speech engine, copied from the APK, and the audio it made for task and subtask names (`tts/cache`) |
| `LICENSES.txt` | the licence texts of YUBI Go and everything it bundles, copied from the APK (`:8080/licenses`) |
| `open-episode` | production: the backend episode being recorded, so the next launch can cancel it after a crash |
| `version_manifest.yaml` | optional, written by you: extra software and hardware rows for `meta.json` |

**Reset configuration** (Setup tab) removes `config.json`, `storage.yaml`, `robot_me.json`,
`assigned.json`, `calibration.json`, the camera calibrations and `camera_ports.json`. It keeps
everything else, including `episodes/`, `exports/`, `db/` and `usb-events.log`. **Delete data** removes everything in
`episodes/` and `exports/`, and keeps the rest.

### config.json keys

| Key | Default | Meaning |
|---|---|---|
| `mode` | — | `standalone` or `production` |
| `tasks` | `[]` | standalone task list (up to 64): a name, or `{"name": …, "subtasks": [ … ]}` with up to 32 steps; `"advance_when_still": true` turns on §4.3 for that task |
| `selected_task` | first of `tasks` | the current standalone task, or with `operator_picks_task` the queued task production records next |
| `operator_picks_task` | `false` | production: the operator picks which queued task to record (Change task). `false` records the dashboard's queue in order. |
| `backend.base_url`, `backend.api_key` | — | production backend |
| `require_operator` | `true` | production: refuse a start until an operator has this robot open in the dashboard's teleop screen. `false` starts without one. |
| `identity.organization`, `site`, `location`, `operator` | empty | written into `meta.json`. All optional: a blank one is filed as `unknown` in a canonical upload path |
| `identity.robot_type`, `robot_id` | `YubiGo`, `yubi-go` | written into `meta.json` and the upload path. Optional too: the defaults fill a new install, and a blank one is filed as `unknown` |
| `camera_ports` | — | the USB port of each camera: `{"left_wrist": "1-1.2", "right_wrist": "1-1.3", "head": "1-1.4"}`. Written when the cameras are assigned (section 9). |
| `camera_roles` | `[0, 1, 2]` | older form, by the order cameras opened; still read when there is no `camera_ports` |
| `head_camera` | `false` | `true` for a rig with a third camera on the headset |
| `keep_faulted_episodes` | `false` | keep recording when a source drops out mid-episode. The older name `keep_faulted_takes` is still read. |
| `video` | `h264` standalone, `jpeg` production | how the cameras are stored in `signals.mcap` (section 11) |
| `folder` | empty | the folder new episodes are filed under in the Viewer. Empty is the top level. |
| `announce_steps` | `true` | say each subtask's name at its tone. `false` plays the tones only. |

## 14. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| The app does not open | A system dialog is blocking the launch, for example the boundary setup or the USB permission prompt. Press the Meta button, clear the dialog, and the app opens by itself. With adb, `adb logcat -d \| grep "Launch is blocked"` names the dialog. |
| The app runs but ignores input | The controllers are asleep. Pick them up and press a button. |
| A wrist shows `Stalled, replug` after a replug | Known stall. The app retries by itself within about 5 s; if it does not recover, replug the hub. |
| Every camera and encoder reconnects every few seconds while a charger is plugged in; the headset says "Power keeps dropping" | The charger cannot hold the hub's load, so it drops and the hub resets with it. A power bank running low does this. Use a stronger charger. |
| Grippers silent after an install | The hub was attached during `adb install`. Replug the hub. |
| OpenXR goes `IDLE` right after launch | The headset is off your head (proximity sensor). |
| No `openxr rendering + poses streaming` line, and `openxr: … — no session yet, retrying in …` repeats | Another app holds the XR session, usually the Quest Browser. Force-stop it. |
| `unauthorized` in `adb devices` | Accept the debugging prompt in the headset (press Meta first if the app is running). |
| Settings ignored, rig is standalone | `config.json` is not readable (`chmod 644`), or `backend.base_url`/`api_key` is missing. |
| The wrists are swapped | Unplug both wrist cameras, plug them back in, and turn one gripper (section 9). |

To read the app's log, connect adb (section 2.2) and run `adb logcat -s yubigo`. Useful lines:

- `airoa_core component startup: … core <commit>`: airoa_core is up.
- `episode episode_…: task "…"`: an episode started.
- `task refused: …`: why a start was refused.
- `[<camera name>] t=…s fps=… … MB/s total_frames=… errs=0 …`: each camera, every 5 s.
- `[…] serial 200 lines/s … B/s (total … lines)`: each gripper encoder, every 5 s.

## 15. Safety

- Follow Meta's health and safety warnings for the headset.
- The app shows passthrough and draws no boundary, so keep the work area clear.
- Route the cables from the headset to the grippers away from your neck.
- Use a rated USB hub and charger.
- Stop and unplug if the headset, the hub or a cable gets hot.
- YUBI Go is research software, provided as is, without warranty (see `LICENSE`).

## 16. Privacy and data

Each episode can contain personal data:

- camera video, which can show people and rooms;
- the operator, organization, site and location in `meta.json`;
- USB serial numbers of the cameras and gripper boards;
- the head and controller poses, motion that can identify a person;
- the app's log as `/rosout`.

LeRobot exports made in the viewer and copies already sent to the bucket hold the same data.
Deleting an episode on the headset does not delete them.

Get consent from the people in view before you record, and avoid recording bystanders. APPI and
GDPR can apply to these recordings.

The app sends nothing to the internet except to the bucket or backend you configure (sections 3
and 12). But the `:8080` page and the `:9090` rosbridge have no login, so anyone on the same
network can watch the cameras and download episodes. By design, the camera streams (`/stream`) can
also be read by any web page open on that network. Use a network you trust.

The headset also announces a name on the LAN, which every device on that network can see. It is
`yubi-go-<robot id>.local`, or `<robot id>.local` when the id already starts with `yubi-go-`. In
production the robot's name on the backend replaces the robot id. A headset whose robot id is
still the default `yubi-go` announces no name, and a changed name takes effect at the next launch.

Without uploads, episodes stay on the headset until you delete them. With an upload target, the
app deletes sent episodes after `local_retention_hours`, which is 24 when `storage.yaml` does not
set it (section 12).
