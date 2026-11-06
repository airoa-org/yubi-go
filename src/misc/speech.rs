//! Spoken task and subtask names. They are typed on the page, so no clip can
//! be recorded ahead for them: `tts` makes each one on the headset when the
//! task list changes, and a take only plays the file.

use std::collections::{BTreeMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use log::{info, warn};

use crate::hub::Hub;
use crate::misc::config::Config;

/// Under the files dir: the engine as the APK bundles it, and what it made.
const ENGINE: &str = "tts";
const MADE: &str = "tts/cache";
/// 1.0–1.5 s a name on the Quest 3S at 2 threads, against 0.85–1.2 s at 4,
/// so the other cores stay with capture. Measured with the 8-bit model and a
/// Kokoro voice the APK no longer ships; `af_heart`, the one it ships, has
/// not been timed.
const THREADS: usize = 2;
/// The model holds ~250 MB while loaded (peak RSS on the Quest 3S), so it is
/// dropped once nothing has been asked of it for this long.
const IDLE: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Pending,
    Ready,
    Failed(String),
}

static STATES: Mutex<BTreeMap<String, State>> = Mutex::new(BTreeMap::new());

/// Every name the rig may say, and where its audio stands.
pub fn states() -> BTreeMap<String, State> {
    STATES.lock().unwrap().clone()
}

/// The file a name is kept in, by a hash of the text and the engine.
pub fn id(text: &str) -> String {
    // FNV-1a 64: stable across builds, which `DefaultHasher` is not.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in tts::VERSION.bytes().chain([0]).chain(text.bytes()) {
        hash = (hash ^ byte as u64).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn made(files: &str, id: &str) -> PathBuf {
    Path::new(files).join(MADE).join(format!("{id}.wav"))
}

/// A made name's WAV by its [`id`], or nothing while it is still being made.
pub fn wav(files: &str, id: &str) -> Option<Vec<u8>> {
    if id.len() != 16 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    std::fs::read(made(files, id)).ok()
}

/// A name ready to say, as 24 kHz samples.
pub fn samples(files: &str, text: &str) -> Option<Vec<i16>> {
    let wav = std::fs::read(made(files, &id(text))).ok()?;
    Some(wav.get(44..)?.as_chunks::<2>().0.iter().map(|&b| i16::from_le_bytes(b)).collect())
}

/// Every name a take can say: each task and subtask, and the assignment's.
pub fn wanted(config: &Config) -> Vec<String> {
    let mut out = Vec::new();
    for task in &config.tasks {
        out.extend(std::iter::once(&task.name).chain(&task.subtasks).cloned());
    }
    if let Some(a) = &config.assignment {
        out.push(a.task.clone());
        out.extend(a.subtasks.iter().map(|s| s.name.clone()));
    }
    let mut seen = HashSet::new();
    out.retain(|t| !t.trim().is_empty() && seen.insert(t.clone()));
    out
}

/// Copy the engine the APK carries into the files dir, unless it is already
/// this one. `FILES` lists it, as `tools/tts/fetch.sh` laid it out.
pub fn unpack<R: Read>(files: &Path, open: impl Fn(&str) -> Option<R>) -> std::io::Result<bool> {
    let Some(mut list) = open("FILES") else { return Ok(false) };
    let mut names = String::new();
    list.read_to_string(&mut names)?;
    let names: Vec<&str> = names.lines().filter(|l| !l.is_empty()).collect();
    crate::misc::core::unpack_bundle(&files.join(ENGINE), &names, "STAMP", open)
}

/// Make every wanted name that has no audio yet, one at a time, never while
/// a take records. `prepare` runs first on the same thread: the unpack, which
/// is ~190 MB on a fresh install.
pub fn start(
    hub: &'static Hub,
    files: &'static str,
    runtime: &'static str,
    prepare: impl FnOnce() + Send + 'static,
) {
    std::thread::spawn(move || {
        // Below the capture threads; ONNX Runtime's workers inherit it.
        #[cfg(target_os = "android")]
        unsafe {
            libc::setpriority(libc::PRIO_PROCESS, libc::gettid() as libc::id_t, 10);
        }
        prepare();
        let mut speaker: Option<tts::Speaker> = None;
        let mut used = Instant::now();
        let mut kept = Vec::new();
        loop {
            let wanted = wanted(&Config::load(files));
            if wanted != kept {
                forget_unwanted(files, &wanted);
                kept = wanted.clone();
            }
            let next = {
                let mut states = STATES.lock().unwrap();
                states.retain(|text, _| wanted.contains(text));
                for text in &wanted {
                    states.entry(text.clone()).or_insert_with(|| {
                        match made(files, &id(text)).exists() {
                            true => State::Ready,
                            false => State::Pending,
                        }
                    });
                }
                states.iter().find(|(_, s)| **s == State::Pending).map(|(t, _)| t.clone())
            };
            match next {
                Some(text) if !hub.recording() => {
                    let made_it = make(&mut speaker, files, runtime, &text);
                    used = Instant::now();
                    STATES.lock().unwrap().insert(text, made_it);
                    continue;
                }
                _ => {}
            }
            if speaker.is_some() && used.elapsed() > IDLE {
                speaker = None;
            }
            std::thread::sleep(POLL);
        }
    });
}

fn make(speaker: &mut Option<tts::Speaker>, files: &str, runtime: &str, text: &str) -> State {
    let engine = Path::new(files).join(ENGINE);
    let loaded = match speaker {
        Some(loaded) => loaded,
        None => match tts::Speaker::load(&engine, Path::new(runtime), THREADS) {
            Ok(loaded) => speaker.insert(loaded),
            Err(e) => {
                warn!("speech: engine unavailable: {e}");
                return State::Failed(format!("speech engine unavailable: {e}"));
            }
        },
    };
    let started = Instant::now();
    let audio = match loaded.speak(text) {
        Ok(audio) => audio,
        Err(e) => return State::Failed(e),
    };
    let to = made(files, &id(text));
    let written = std::fs::create_dir_all(to.parent().unwrap())
        .and_then(|_| std::fs::write(to.with_extension("tmp"), tts::kokoro::wav(&audio)))
        .and_then(|_| std::fs::rename(to.with_extension("tmp"), &to));
    match written {
        Ok(()) => {
            info!("speech: {text:?} made in {} ms", started.elapsed().as_millis());
            State::Ready
        }
        Err(e) => State::Failed(format!("cannot keep the audio: {e}")),
    }
}

/// Audio for names no longer on the list goes, so the cache stays the list.
fn forget_unwanted(files: &str, wanted: &[String]) {
    let keep: HashSet<String> = wanted.iter().map(|t| format!("{}.wav", id(t))).collect();
    let Ok(entries) = std::fs::read_dir(Path::new(files).join(MADE)) else { return };
    for entry in entries.flatten() {
        if !keep.contains(&*entry.file_name().to_string_lossy()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
