use std::fs;
use std::io;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use crate::misc::capture::Capture;
use crate::misc::clock::{boottime_ns, realtime_ns};
use crate::misc::config::Config;
use crate::misc::pose::Located;
use crate::record::mcap::SignalsMcap;

/// The file `finish` writes last, so its presence is what says a take
/// finished: the uploader, the panel and the startup sweep all ask this.
pub const META: &str = "meta.json";

/// Did this take close? A directory without [`META`] is one the process died
/// in the middle of.
pub fn is_complete(dir: &Path) -> bool {
    dir.join(META).exists()
}

pub const SIGNALS: &str = "signals.mcap";

/// The folder a take is filed under, as `a/b`, beside its data. Takes stay
/// flat on disk, because the uploader, its state database and core's GC all
/// read `episodes/` one level deep; a file inside the take travels with it
/// to the bucket and through `pull.py`. No file is the root.
pub const FOLDER: &str = "folder.txt";

/// A folder as typed, made canonical: `/`-separated names, each trimmed,
/// empty ones dropped. `.` and `..` and control characters are refused, so a
/// folder never reads as a path out of anything.
pub fn clean_folder(raw: &str) -> Result<String, String> {
    let parts: Vec<&str> = raw.split('/').map(str::trim).filter(|p| !p.is_empty()).collect();
    if parts.len() > 8 {
        return Err(format!("{raw:?} is more than 8 folders deep"));
    }
    for part in &parts {
        if *part == "." || *part == ".." || part.chars().any(char::is_control) {
            return Err(format!("{part:?} cannot name a folder"));
        }
        if part.chars().count() > 64 {
            return Err(format!("{part:?} is over 64 characters"));
        }
    }
    Ok(parts.join("/"))
}

pub fn folder_of(dir: &Path) -> String {
    fs::read_to_string(dir.join(FOLDER))
        .ok()
        .and_then(|f| clean_folder(&f).ok())
        .unwrap_or_default()
}

/// Files the take under `folder`; the root removes the file.
pub fn set_folder(dir: &Path, folder: &str) -> io::Result<()> {
    if folder.is_empty() {
        return match fs::remove_file(dir.join(FOLDER)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    fs::write(dir.join(FOLDER), folder)
}

#[derive(Clone, Copy)]
struct Anchor {
    boottime_ns: u64,
    realtime_ns: u64,
}

impl Anchor {
    fn now() -> Anchor {
        Anchor { boottime_ns: boottime_ns(), realtime_ns: realtime_ns() }
    }
}

fn binding(config: &Config) -> bool {
    config.assignment.as_ref().is_some_and(|a| !a.spent)
}

pub(crate) fn uuid_v4() -> io::Result<String> {
    let mut b = [0u8; 16];
    fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b))?;
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex = |r: std::ops::Range<usize>| -> String {
        b[r].iter().map(|byte| format!("{byte:02x}")).collect()
    };
    Ok(format!("{}-{}-{}-{}-{}", hex(0..4), hex(4..6), hex(6..8), hex(8..10), hex(10..16)))
}

struct Attempt {
    step: usize,
    /// `None` is the take's own start or end, known only once it closes.
    start_ns: Option<u64>,
    end_ns: Option<u64>,
    success: bool,
}

pub struct Recorder {
    pub name: String,
    dir: PathBuf,
    config_dir: String,
    signals: Option<SignalsMcap>,
    started: Anchor,
    failed: bool,
    uuid: String,
    config: Config,
    calibration: crate::misc::calib::Calibration,
    /// The tries already closed, in order. A retried step appears once per try.
    attempts: Vec<Attempt>,
    /// The step the hands are on.
    step: usize,
    /// When the running try began, `None` for the take's own start. `None`
    /// outside means no try is running: the take is paused after a failure.
    open: Option<Option<u64>>,
    /// The running try's first motion, when a hold started it. Its start moves
    /// there, so the operator frozen to hear the step's name is left out. A
    /// double-click's step starts as the jaw opens, as the operator chose.
    onset: Option<crate::record::still::Onset>,
    usable_start_ns: Option<u64>,
    usable_end_ns: Option<u64>,
    first_ns: Option<u64>,
    head_stereo: bool,
    /// Whether each camera's video has reached a key frame. Frames before it
    /// cannot be decoded, so they are left out.
    keyed: [bool; 3],
}

impl Recorder {
    #[cfg(test)]
    pub fn start(
        base: &str,
        calibration: crate::misc::calib::Calibration,
        expecting_video: [bool; 3],
        gesture: Option<crate::record::click::Gesture>,
    ) -> io::Result<Recorder> {
        Self::start_with_config(base, calibration, expecting_video, gesture, Config::load(base))
    }

    /// `base` is the files dir — where config.json and assigned.json live.
    /// Takes go one level below it, and only this function knows that.
    pub(crate) fn start_with_config(
        base: &str,
        calibration: crate::misc::calib::Calibration,
        expecting_video: [bool; 3],
        gesture: Option<crate::record::click::Gesture>,
        config: Config,
    ) -> io::Result<Recorder> {
        let started = Anchor::now();
        let uuid = uuid_v4()?;
        let name = format!("episode_{}", started.realtime_ns / 1_000_000);
        let dir = Path::new(&takes(base)).join(&name);
        fs::create_dir_all(&dir)?;
        let mut signals = SignalsMcap::create(&dir.join(SIGNALS), expecting_video)?;
        signals.clock(started.boottime_ns, started.realtime_ns)?;
        signals.calibration(started.realtime_ns, &calibration)?;
        set_folder(&dir, &config.folder)?;
        log::info!("episode {name}: task {:?}", config.task());
        Ok(Recorder {
            name,
            dir,
            config_dir: base.to_owned(),
            signals: Some(signals),
            started,
            failed: false,
            uuid,
            config,
            calibration,
            attempts: Vec::new(),
            step: 0,
            open: Some(None),
            onset: None,
            usable_start_ns: gesture.map(|g| g.task_start_ns),
            usable_end_ns: None,
            first_ns: None,
            head_stereo: false,
            keyed: [false; 3],
        })
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub(crate) fn set_head_stereo(&mut self, stereo: bool) {
        self.head_stereo = stereo;
    }

    pub(crate) fn set_uuid(&mut self, uuid: String) {
        self.uuid = uuid;
    }

    fn check(&mut self, result: io::Result<()>) {
        if let Err(e) = result {
            log::error!("episode {} write failed — stopping (disk full?): {e}", self.name);
            self.signals = None;
            self.failed = true;
        }
    }

    /// A fault opening (`open`) or closing mid-take, as an event: level ERROR
    /// for one that cancels or stops the take, WARN otherwise, OK on close.
    pub fn fault(
        &mut self,
        at_ns: u64,
        source: &'static str,
        fault: crate::record::gate::Fault,
        open: bool,
    ) {
        use crate::misc::status;
        let level = match (open, fault.cancels_the_take() || fault.stops_the_take()) {
            (false, _) => status::OK,
            (true, true) => status::ERROR,
            (true, false) => status::WARN,
        };
        let event = status::Status {
            level,
            name: format!("fault/{source}"),
            message: fault.text().to_string(),
            ..status::Status::default()
        }
        .with("fault", format!("{fault:?}"))
        .with("state", if open { "open" } else { "closed" });
        self.status(at_ns, true, &[event]);
    }

    /// Messages the writer fell behind on and lost, as one event at the end,
    /// because `meta.json` has no field for them and the take still closes.
    pub fn dropped(&mut self, at_ns: u64, count: u64) {
        use crate::misc::status;
        let event = status::Status {
            level: status::WARN,
            name: "writer/dropped".into(),
            message: format!("{count} messages dropped (disk too slow)"),
            ..status::Status::default()
        }
        .with("dropped", count);
        self.status(at_ns, true, &[event]);
    }

    pub fn status(&mut self, at_ns: u64, event: bool, statuses: &[crate::misc::status::Status]) {
        let at_ns = self.wall_ns(at_ns);
        if let Some(s) = &mut self.signals {
            let r = s.status(at_ns, event, statuses);
            self.check(r);
        }
    }

    pub fn joy(&mut self, at_ns: u64, side: usize, axes: &[f32], buttons: &[i32]) {
        let at_ns = self.wall_ns(at_ns);
        if let Some(s) = &mut self.signals {
            let r = s.joy(at_ns, side, axes, buttons);
            self.check(r);
        }
    }

    pub fn input_changes(&mut self, at_ns: u64, side: usize, changes: &[crate::hub::InputChange]) {
        let at_ns = self.wall_ns(at_ns);
        let changes: Vec<_> = changes
            .iter()
            .map(|c| crate::hub::InputChange { at_ns: self.wall_ns(c.at_ns), ..*c })
            .collect();
        if let Some(s) = &mut self.signals {
            let r = s.input_changes(at_ns, side, &changes);
            self.check(r);
        }
    }

    pub fn log(&mut self, line: &crate::misc::journal::Line) {
        let at_ns = self.wall_ns(line.at_ns);
        if let Some(s) = &mut self.signals {
            let r = s.log(at_ns, line);
            self.check(r);
        }
    }

    /// Where the take's last boundary lies, so a new one never runs before it.
    fn floor(&self) -> u64 {
        match self.open {
            Some(Some(begins)) => Some(begins),
            Some(None) => None,
            None => self.attempts.last().and_then(|a| a.end_ns),
        }
        .or(self.usable_start_ns)
        .unwrap_or(self.started.boottime_ns)
    }

    fn open_at(&mut self, at_ns: u64, held: bool) {
        self.open = Some(Some(at_ns));
        self.onset = held.then(|| crate::record::still::Onset::new(at_ns));
    }

    fn close(&mut self, end_ns: Option<u64>, success: bool) -> bool {
        let Some(start_ns) = self.open.take() else {
            log::warn!("episode {}: no step is running", self.name);
            return false;
        };
        let ends = end_ns.or(self.usable_end_ns);
        let moved = self
            .onset
            .take()
            .and_then(|o| o.moved_ns())
            .filter(|&at| ends.is_none_or(|end| at < end));
        let start_ns = moved.or(start_ns);
        self.attempts.push(Attempt { step: self.step, start_ns, end_ns, success });
        true
    }

    pub fn advance(&mut self, gesture: crate::record::click::Gesture) {
        let ends = gesture.task_end_ns.max(self.floor());
        if self.close(Some(ends), true) {
            self.step += 1;
            self.open_at(gesture.task_start_ns.max(ends), gesture.held);
        }
    }

    /// The running try failed at `at_ns`. Nothing is a segment until `resume`,
    /// so the operator's reset is left out of the data.
    pub fn fail(&mut self, at_ns: u64) {
        let ends = at_ns.max(self.floor());
        self.close(Some(ends), false);
    }

    /// Try the same step again.
    pub fn resume(&mut self, gesture: crate::record::click::Gesture) {
        if self.open.is_some() {
            log::warn!("episode {}: resume while a step is running", self.name);
            return;
        }
        self.open_at(gesture.task_start_ns.max(self.floor()), gesture.held);
    }

    fn saw(&mut self, at_ns: u64) {
        self.first_ns = Some(self.first_ns.map_or(at_ns, |first| first.min(at_ns)));
    }

    pub fn frame(&mut self, at_ns: u64, capture: Capture, slot: usize, jpeg: &[u8]) {
        self.saw(capture.at_ns.unwrap_or(at_ns));
        let (at_ns, capture) = self.stamped(at_ns, capture);
        if let Some(s) = &mut self.signals {
            let r = s.frame(at_ns, capture, slot, jpeg);
            self.check(r);
        }
    }

    pub fn video(
        &mut self,
        at_ns: u64,
        capture: Capture,
        slot: usize,
        data: &[u8],
        key_frame: bool,
    ) {
        let Some(keyed) = self.keyed.get_mut(slot) else { return };
        *keyed |= key_frame;
        if !*keyed {
            return;
        }
        self.saw(capture.at_ns.unwrap_or(at_ns));
        let (at_ns, capture) = self.stamped(at_ns, capture);
        if let Some(s) = &mut self.signals {
            let r = s.video(at_ns, capture, slot, data);
            self.check(r);
        }
    }

    fn stamped(&self, at_ns: u64, capture: Capture) -> (u64, Capture) {
        let mut uvc = capture.uvc;
        if let Some(u) = &mut uvc {
            u.first.reaped_ns = self.wall_ns(u.first.reaped_ns);
            u.last.reaped_ns = self.wall_ns(u.last.reaped_ns);
        }
        let capture = Capture { at_ns: capture.at_ns.map(|at| self.wall_ns(at)), uvc, ..capture };
        (self.wall_ns(at_ns), capture)
    }

    pub fn pose(&mut self, at_ns: u64, capture: Capture, part: usize, located: Located) {
        if part < 2 {
            if let Some(onset) = &mut self.onset {
                onset.hand(located.velocity, at_ns);
            }
        }
        self.saw(capture.at_ns.unwrap_or(at_ns));
        let (at_ns, capture) = self.stamped(at_ns, capture);
        if let Some(s) = &mut self.signals {
            let r = s.pose(at_ns, capture, part, located);
            self.check(r);
        }
    }

    pub fn gripper(
        &mut self,
        at_ns: u64,
        seq: Option<u64>,
        side: usize,
        raw: f64,
        calib: Option<f64>,
    ) {
        if let Some(onset) = &mut self.onset {
            onset.jaw(side, calib, at_ns);
        }
        self.saw(at_ns);
        let at_ns = self.wall_ns(at_ns);
        if let Some(s) = &mut self.signals {
            let r = s.gripper(at_ns, seq, side, raw, calib);
            self.check(r);
        }
    }

    pub fn interrupted(mut self) -> bool {
        log::warn!("episode {} was interrupted — discarding it", self.name);
        self.failed = true;
        self.finish(false, None)
    }

    pub fn finish(mut self, success: bool, gesture: Option<crate::record::click::Gesture>) -> bool {
        if let Some(g) = gesture {
            self.usable_end_ns = Some(g.task_end_ns.max(self.floor()));
        }
        if self.open.is_some() {
            self.close(None, success);
        }

        let (mut violations, backwards) = match self.signals.take() {
            Some(s) => {
                let backwards = s.backwards();
                (s.close().unwrap_or_else(|e| vec![format!("flush failed: {e}")]), backwards)
            }
            None => (vec!["writes failed mid-episode".to_string()], Vec::new()),
        };
        // A sample out of order leaves every other message whole, so a rig
        // that keeps faulted takes keeps this one as it keeps the others.
        if self.config.keep_faulted_episodes && violations.iter().all(|v| backwards.contains(v)) {
            for v in &violations {
                log::error!("episode {} kept although {v} (keep_faulted_episodes)", self.name);
            }
            violations.clear();
        }
        let ended = Anchor::now();

        // meta.json is what says this take finished, so it goes down only once
        // the take has earned it. A crash between writing it and deleting a bad
        // take would otherwise leave a half-written mcap looking uploadable.
        // Written beside and renamed in, so a force-stop mid-write never
        // leaves a half `meta.json` that reads as complete.
        if violations.is_empty() && !self.failed {
            let meta = self.meta_json(ended, success);
            match crate::misc::save::replace(&self.dir.join(META), meta.as_bytes()) {
                Ok(()) => {
                    let bound = binding(&self.config);
                    // Standalone has no assignments, so nothing there is unbound.
                    if self.config.production() {
                        crate::record::backend::episode::mark_bound(bound);
                    }
                    if let Some(a) = self.config.assignment.as_ref().filter(|_| bound) {
                        crate::record::backend::episode::consumed(
                            self.config_dir.clone(),
                            a.id.clone(),
                        );
                    }
                    return true;
                }
                // The mcap closed and is on disk, so the take is whole and
                // only its marker is missing: keep it for someone to pull.
                // Without `meta.json` it is never uploaded, and the next
                // launch's sweep deletes it.
                Err(e) => {
                    log::error!(
                        "episode {}: meta.json not written: {e} — the take is kept unmarked at {}; \
                         copy it off before the app starts again",
                        self.name,
                        self.dir.display()
                    );
                    return false;
                }
            }
        }
        // A take whose signals did not flush is not data. Say why while the
        // evidence is still here, then take it off the disk — nothing
        // downstream can use it and a half-written mcap is worse than none.
        for v in &violations {
            log::error!("episode {} failed its close check — {v}", self.name);
        }
        match fs::remove_dir_all(&self.dir) {
            Ok(()) => log::warn!("episode {} deleted: it did not close cleanly", self.name),
            Err(e) => {
                log::error!("episode {} did not close cleanly and will not delete: {e}", self.name)
            }
        }
        false
    }

    fn devices_json(&self) -> Vec<serde_json::Value> {
        let entry = |side: usize| {
            let s = self.calibration.side(side)?;
            let hand = if side == 0 { "left" } else { "right" };
            Some(serde_json::json!({
                "role": format!("gripper_{hand}"),
                "type": "as5601_encoder",
                "id": s.device_id,
            }))
        };
        // Only on a rig that has one, and naming the stereo module as such: its
        // frames are two eyes side by side, which a consumer must split.
        let head = self.config.head_camera.then(|| {
            serde_json::json!({
                "role": "head_camera",
                "type": if self.head_stereo { "uvc_mjpeg_stereo" } else { "uvc_mjpeg" },
                "id": crate::record::mcap::CAM_NAMES[crate::hub::SLOT_HEAD],
            })
        });
        (0..2).filter_map(entry).chain(head).collect()
    }

    fn wall(&self, boottime_ns: u64) -> f64 {
        self.wall_ns(boottime_ns) as f64 / 1e9
    }

    fn wall_ns(&self, boottime_ns: u64) -> u64 {
        let base = self.started.realtime_ns as i128 - self.started.boottime_ns as i128;
        (base + boottime_ns as i128).max(0) as u64
    }

    fn usable(&self, ended: Anchor) -> (f64, f64) {
        let end = self.usable_end_ns.unwrap_or(ended.boottime_ns).min(ended.boottime_ns);
        let start = self
            .usable_start_ns
            .unwrap_or_else(|| self.data_start_ns())
            .max(self.data_start_ns())
            .min(end);
        let ended_wall = ended.realtime_ns as f64 / 1e9;
        let end_wall = match self.usable_end_ns {
            Some(_) if end < ended.boottime_ns => self.wall(end),
            _ => ended_wall,
        };
        (self.wall(start), end_wall)
    }

    fn data_start_ns(&self) -> u64 {
        self.first_ns.unwrap_or(self.started.boottime_ns).min(self.started.boottime_ns)
    }

    fn segments_json(&self, ended: Anchor) -> (Vec<String>, Vec<serde_json::Value>) {
        let mut names = self.config.subtask_names();
        let reached = self.attempts.iter().map(|a| a.step + 1).max().unwrap_or(1);
        // A take with no subtasks of its own is labelled with the task.
        let named = names.len() >= reached;
        if named {
            names.truncate(reached);
        } else {
            names = vec![self.config.task().to_string()];
        }
        let (t0, t1) = self.usable(ended);
        let segments = self
            .attempts
            .iter()
            .map(|a| {
                serde_json::json!({
                    "start_time": a.start_ns.map_or(t0, |ns| self.wall(ns)),
                    "end_time": a.end_ns.map_or(t1, |ns| self.wall(ns)),
                    "label_idx": if named { a.step } else { 0 },
                    "success": a.success,
                })
            })
            .collect();
        (names, segments)
    }

    /// Everything one episode's meta.json is made of, under the names
    /// `airoa_core`'s `MetadataBuilder` takes. Nothing else: the document is
    /// the v2.0 schema's, with no keys of our own bolted onto it.
    fn meta_args(&self, ended: Anchor, success: bool) -> serde_json::Value {
        let (t0, t1) = (self.wall(self.data_start_ns()), ended.realtime_ns as f64 / 1e9);
        let id = &self.config.identity;
        let (labels, segments) = self.segments_json(ended);

        let (programs, discovered) = crate::record::system_info::discovered(&self.config_dir);
        let mut devices = self.devices_json();
        devices.extend(discovered);

        serde_json::json!({
            "uuid": self.uuid,
            "robot": {"uri": null, "type": id.robot_type, "id": id.robot_id, "checksum": null},
            "files": [{"type": "mcap", "name": SIGNALS, "checksum": null}],
            "environment": {"type": "real_world", "site": id.site, "location": id.location},
            "runner": {"type": "operator", "organization": id.organization, "name": id.operator},
            "devices": devices,
            "programs": programs,
            "episode": {"start_time": t0, "end_time": t1, "success": success,
                        "label": self.config.task()},
            "labels": labels,
            "segments": segments,
        })
    }

    fn meta_json(&self, ended: Anchor, success: bool) -> String {
        let args = self.meta_args(ended, success);
        match airoa_sdk::build_metadata(args.clone()) {
            Ok(built) => built,
            Err(e) => {
                // The gate hard-stops while airoa_core is down (`Fault::CoreDown`
                // refuses the start), so reaching here means it broke mid-take.
                // Losing the recording is worse than an unbuilt meta.json: the
                // episode still closes, and these are the very fields the
                // builder wanted, so it can be rebuilt off the headset.
                log::error!("episode {}: meta.json left unbuilt — {e}", self.name);
                serde_json::to_string_pretty(&serde_json::json!({
                    "schema_version": "unbuilt",
                    "unbuilt_reason": e.to_string(),
                    "metadata_inputs": args,
                }))
                .unwrap_or_default()
            }
        }
    }
}

/// Takes live one level below the files dir, so a scan of "everything under
/// the mount" — the GC's — never meets the payload or the config.
pub const TAKES: &str = "episodes";

pub fn takes(base: &str) -> String {
    format!("{base}/{TAKES}")
}

pub fn list(base: &str) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(takes(base))
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| n.starts_with("episode_"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names.reverse();
    names
}

/// Delete takes the process died in the middle of. `finish` writes meta.json
/// last, so a directory without one was never closed and holds a truncated
/// mcap; a take that closed badly deleted itself already.
pub fn sweep_incomplete(base: &str) -> usize {
    let mut swept = 0;
    for name in list(base) {
        let dir = Path::new(&takes(base)).join(&name);
        if is_complete(&dir) {
            continue;
        }
        match fs::remove_dir_all(&dir) {
            Ok(()) => {
                log::warn!("deleted incomplete episode {name} — it has no meta.json");
                swept += 1;
            }
            Err(e) => log::error!("cannot delete incomplete episode {name}: {e}"),
        }
    }
    if swept > 0 {
        log::warn!("{swept} incomplete episode(s) deleted at startup");
    }
    swept
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
