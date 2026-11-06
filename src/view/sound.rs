use crate::hub::Feedback;

pub const RATE: u32 = 48_000;

const PEAK: f32 = 0.075;
const HARSH_GAIN: f32 = 0.4;

/// Startup sound, 48 kHz mono at -1 dBFS. Its loudest half second is 0.16 RMS, so
/// this puts it ~2 dB above the speech (`voice::GAIN`): it has to reach someone not
/// wearing the headset. Untested by ear on the headset.
const BOOT: &[u8] = include_bytes!("../../assets/sound/boot.wav");
const BOOT_GAIN: f32 = 0.54;

const ATTACK_MS: f32 = 5.0;
const RELEASE_MS: f32 = 20.0;

struct Segment {
    hz: Option<f32>,
    ms: u32,
    harsh: bool,
}

const fn note(hz: f32, ms: u32) -> Segment {
    Segment { hz: Some(hz), ms, harsh: false }
}

const fn rest(ms: u32) -> Segment {
    Segment { hz: None, ms, harsh: false }
}

const fn harsh(hz: f32, ms: u32) -> Segment {
    Segment { hz: Some(hz), ms, harsh: true }
}

fn segments(what: Feedback) -> Vec<Segment> {
    match what {
        Feedback::Started => vec![note(660.0, 90), note(990.0, 110)],
        Feedback::Stopped => vec![note(990.0, 90), note(660.0, 110)],
        Feedback::Advanced => vec![note(820.0, 70)],
        // Three notes, so it is not taken for the two-note start.
        Feedback::Assigned => vec![note(784.0, 70), note(988.0, 70), note(1319.0, 120)],
        Feedback::MarkedFailed | Feedback::EndedFailed => {
            vec![note(330.0, 70), rest(60), note(330.0, 70)]
        }
        Feedback::Refused => vec![Segment { hz: Some(220.0), ms: 420, harsh: true }],
        Feedback::Fault => {
            vec![harsh(1320.0, 90), rest(60), harsh(1320.0, 90), rest(60), harsh(1320.0, 90)]
        }
        Feedback::Cancelled => {
            let mut pips = segments(Feedback::Fault);
            pips.extend([rest(160), harsh(440.0, 160), harsh(220.0, 280)]);
            pips
        }
        Feedback::RunningOut => {
            vec![note(880.0, 180), rest(70), note(587.0, 180), rest(70), note(440.0, 180)]
        }
        // Spoken only: `voice::spoken`.
        Feedback::TimeWarning => Vec::new(),
    }
}

#[cfg_attr(not(any(target_os = "android", test)), allow(dead_code))]
fn padded(mut pcm: Vec<i16>, burst: usize) -> Vec<i16> {
    let bursts = pcm.len().div_ceil(burst) + 1;
    pcm.resize(bursts * burst, 0);
    pcm
}

pub fn cue(what: Feedback) -> Vec<i16> {
    let mut pcm = Vec::new();
    for segment in segments(what) {
        let samples = (segment.ms * RATE / 1000) as usize;
        let Some(hz) = segment.hz else {
            pcm.resize(pcm.len() + samples, 0);
            continue;
        };
        let attack = (ATTACK_MS * RATE as f32 / 1000.0).min(samples as f32 / 2.0);
        let release = (RELEASE_MS * RATE as f32 / 1000.0).min(samples as f32 / 2.0);
        for i in 0..samples {
            let t = i as f32 / RATE as f32;
            let phase = (t * hz).fract();
            let wave = match segment.harsh {
                true if phase < 0.5 => 1.0,
                true => -1.0,
                false => (phase * std::f32::consts::TAU).sin(),
            };
            let fade = (samples - 1 - i) as f32 / release;
            let envelope = (i as f32 / attack).min(fade).clamp(0.0, 1.0);
            let gain = if segment.harsh { HARSH_GAIN } else { 1.0 };
            pcm.push((wave * envelope * gain * PEAK * i16::MAX as f32) as i16);
        }
    }
    pcm
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
fn boot() -> Vec<i16> {
    crate::view::voice::samples(BOOT).into_iter().map(|s| (s as f32 * BOOT_GAIN) as i16).collect()
}

#[cfg_attr(not(target_os = "android"), allow(dead_code))]
#[derive(Debug)]
pub enum Cue {
    Boot,
    Tone(Feedback),
    /// Speech, after a tone when `lead` is set, and a made name after it.
    Speech {
        lead: Option<Feedback>,
        ids: Vec<&'static str>,
        name: Vec<i16>,
    },
    /// A made name (`misc::speech`) after a tone, already at the stream's rate.
    Name {
        lead: Feedback,
        pcm: Vec<i16>,
    },
}

#[cfg(target_os = "android")]
pub fn play(what: Feedback) {
    player::send(Cue::Tone(what))
}

#[cfg(target_os = "android")]
pub fn play_boot() {
    player::send(Cue::Boot)
}

#[cfg(target_os = "android")]
pub fn say(lead: Option<Feedback>, ids: Vec<&'static str>) {
    player::send(Cue::Speech { lead, ids, name: Vec::new() })
}

/// Clips, then a made name (`misc::speech`) at the stream's rate.
#[cfg(target_os = "android")]
pub fn say_then(ids: Vec<&'static str>, name: Vec<i16>) {
    player::send(Cue::Speech { lead: None, ids, name })
}

#[cfg(target_os = "android")]
pub fn say_name(lead: Feedback, pcm: Vec<i16>) {
    player::send(Cue::Name { lead, pcm })
}

#[cfg(target_os = "android")]
pub fn warm() {
    player::sender();
}

#[cfg(target_os = "android")]
mod player {
    use super::{boot, cue, padded, Cue, RATE};
    use log::{info, warn};
    use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
    use std::sync::OnceLock;

    /// Silence between a tone and the speech after it, so the two read as two.
    const LEAD_GAP_MS: u32 = 150;

    enum Builder {}
    enum Stream {}

    struct Owned(*mut Stream);
    unsafe impl Send for Owned {}

    const AAUDIO_OK: i32 = 0;
    const AAUDIO_FORMAT_PCM_I16: i32 = 1;
    const AAUDIO_PERFORMANCE_MODE_NONE: i32 = 10;

    #[link(name = "aaudio")]
    extern "C" {
        fn AAudio_createStreamBuilder(builder: *mut *mut Builder) -> i32;
        fn AAudioStreamBuilder_setSampleRate(builder: *mut Builder, rate: i32);
        fn AAudioStreamBuilder_setChannelCount(builder: *mut Builder, count: i32);
        fn AAudioStreamBuilder_setFormat(builder: *mut Builder, format: i32);
        fn AAudioStreamBuilder_setPerformanceMode(builder: *mut Builder, mode: i32);
        fn AAudioStreamBuilder_openStream(builder: *mut Builder, stream: *mut *mut Stream) -> i32;
        fn AAudioStreamBuilder_delete(builder: *mut Builder) -> i32;
        fn AAudioStream_requestStart(stream: *mut Stream) -> i32;
        fn AAudioStream_write(
            stream: *mut Stream,
            buffer: *const i16,
            frames: i32,
            timeout_ns: i64,
        ) -> i32;
        fn AAudioStream_getSampleRate(stream: *mut Stream) -> i32;
        fn AAudioStream_getFramesPerBurst(stream: *mut Stream) -> i32;
        fn AAudioStream_getBufferSizeInFrames(stream: *mut Stream) -> i32;
        fn AAudioStream_getXRunCount(stream: *mut Stream) -> i32;
    }

    fn open() -> Option<Owned> {
        let mut builder = std::ptr::null_mut();
        let mut stream = std::ptr::null_mut();
        unsafe {
            if AAudio_createStreamBuilder(&mut builder) != AAUDIO_OK {
                warn!("audio: no stream builder");
                return None;
            }
            AAudioStreamBuilder_setSampleRate(builder, RATE as i32);
            AAudioStreamBuilder_setChannelCount(builder, 1);
            AAudioStreamBuilder_setFormat(builder, AAUDIO_FORMAT_PCM_I16);
            AAudioStreamBuilder_setPerformanceMode(builder, AAUDIO_PERFORMANCE_MODE_NONE);
            let opened = AAudioStreamBuilder_openStream(builder, &mut stream);
            AAudioStreamBuilder_delete(builder);
            if opened != AAUDIO_OK {
                warn!("audio: openStream failed ({opened}) — tones are off this session");
                return None;
            }
            let started = AAudioStream_requestStart(stream);
            if started != AAUDIO_OK {
                warn!("audio: requestStart failed ({started}) — tones are off this session");
                return None;
            }
            info!(
                "audio: AAudio open at {} Hz (asked {RATE}), {} frames/burst, {} frame buffer",
                AAudioStream_getSampleRate(stream),
                AAudioStream_getFramesPerBurst(stream),
                AAudioStream_getBufferSizeInFrames(stream),
            );
        }
        Some(Owned(stream))
    }

    pub fn send(what: Cue) {
        if let Some(sender) = sender() {
            if let Err(TrySendError::Full(what)) = sender.try_send(what) {
                info!("audio: {what:?} dropped, a cue is still playing");
            }
        }
    }

    pub fn sender() -> Option<&'static SyncSender<Cue>> {
        static PLAYER: OnceLock<Option<SyncSender<Cue>>> = OnceLock::new();
        PLAYER
            .get_or_init(|| {
                let stream = open()?;
                let (tx, rx) = sync_channel::<Cue>(1);
                std::thread::spawn(move || {
                    let owned = stream;
                    let stream = owned.0;
                    let burst = unsafe { AAudioStream_getFramesPerBurst(stream) }.max(1) as usize;
                    let mut xruns = 0;
                    for what in rx {
                        let pcm = match what {
                            Cue::Boot => boot(),
                            Cue::Tone(what) => cue(what),
                            Cue::Speech { lead, ids, name } => {
                                let mut pcm = lead.map(cue).unwrap_or_default();
                                if !pcm.is_empty() {
                                    pcm.resize(pcm.len() + (LEAD_GAP_MS * RATE / 1000) as usize, 0);
                                }
                                pcm.extend(crate::view::voice::pcm(&ids));
                                pcm.extend(name);
                                pcm
                            }
                            Cue::Name { lead, pcm: name } => {
                                let mut pcm = cue(lead);
                                pcm.resize(pcm.len() + (LEAD_GAP_MS * RATE / 1000) as usize, 0);
                                pcm.extend(name);
                                pcm
                            }
                        };
                        let pcm = padded(pcm, burst);
                        let frames = pcm.len() as i32;
                        // A blocking write returns early at its timeout, so it
                        // must outlast the cue: speech runs up to ~4 s.
                        let timeout_ns =
                            (pcm.len() as i64 * 1_000_000_000 / RATE as i64) + 1_000_000_000;
                        let queued = unsafe { AAudioStream_getBufferSizeInFrames(stream) }.max(0);
                        crate::hub::sounding_until(
                            crate::misc::clock::boottime_ns()
                                + (pcm.len() as u64 + queued as u64) * 1_000_000_000 / RATE as u64,
                        );
                        let written =
                            unsafe { AAudioStream_write(stream, pcm.as_ptr(), frames, timeout_ns) };
                        if written < 0 {
                            warn!("audio: write failed ({written})");
                        }
                        let seen = unsafe { AAudioStream_getXRunCount(stream) };
                        if seen > xruns {
                            info!("audio: {} underruns (was {xruns})", seen);
                            xruns = seen;
                        }
                    }
                });
                Some(tx)
            })
            .as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `EndedFailed` is left out: it shares `MarkedFailed`'s tone, and the
    // voice tells them apart.
    const ALL: [Feedback; 9] = [
        Feedback::Started,
        Feedback::Assigned,
        Feedback::Stopped,
        Feedback::Advanced,
        Feedback::MarkedFailed,
        Feedback::Refused,
        Feedback::Fault,
        Feedback::Cancelled,
        Feedback::RunningOut,
    ];

    #[test]
    fn a_cue_fills_whole_bursts_and_ends_in_a_silent_one() {
        for burst in [4800, 3360, 192, 1] {
            for what in ALL {
                let bare = cue(what);
                let out = padded(bare.clone(), burst);
                assert_eq!(out.len() % burst, 0, "{what:?} at {burst}: partial burst");
                assert!(out.len() > bare.len(), "{what:?} at {burst}: no tail at all");
                assert!(
                    out[bare.len()..].iter().all(|&s| s == 0),
                    "{what:?} at {burst}: the tail must be silence, not more tone"
                );
                assert!(
                    out.len() - bare.len() >= burst,
                    "{what:?} at {burst}: a whole burst of silence, so a repeat is inaudible"
                );
            }
        }
    }

    #[test]
    fn every_cue_starts_and_ends_at_silence() {
        for what in ALL {
            let pcm = cue(what);
            assert!(!pcm.is_empty(), "{what:?} is silent");
            assert_eq!(pcm[0], 0, "{what:?} starts with a step");
            assert_eq!(pcm[pcm.len() - 1], 0, "{what:?} ends with a step");
        }
    }

    #[test]
    fn every_cue_is_audible_and_none_clips() {
        for what in ALL {
            let peak = cue(what).into_iter().map(i16::abs).max().unwrap();
            let want = (PEAK * i16::MAX as f32) as i16;
            let floor = (want as f32 * HARSH_GAIN / 2.0) as i16;
            assert!(peak > floor, "{what:?} never gets loud: {peak}");
            assert!(peak <= want, "{what:?} exceeds its own ceiling: {peak} > {want}");
        }
    }

    #[test]
    fn the_boot_sound_is_at_the_stream_rate_and_starts_and_ends_at_silence() {
        assert_eq!(u16::from_le_bytes([BOOT[22], BOOT[23]]), 1, "boot.wav must be mono");
        assert_eq!(u32::from_le_bytes(BOOT[24..28].try_into().unwrap()), RATE);
        let pcm = boot();
        assert!(pcm.len() > RATE as usize, "boot.wav is missing or truncated");
        assert_eq!((pcm[0], pcm[pcm.len() - 1]), (0, 0));
    }

    #[test]
    fn started_rises_and_stopped_falls() {
        let pitch = |pcm: &[i16]| {
            let crossings =
                |half: &[i16]| half.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
            let mid = pcm.len() / 2;
            (crossings(&pcm[..mid]), crossings(&pcm[mid..]))
        };
        let (first, second) = pitch(&cue(Feedback::Started));
        assert!(second > first, "started must rise: {first} then {second}");
        let (first, second) = pitch(&cue(Feedback::Stopped));
        assert!(second < first, "stopped must fall: {first} then {second}");
    }

    #[test]
    fn cancelled_opens_with_the_fault_cue_and_then_says_more() {
        let (fault, cancelled) = (cue(Feedback::Fault), cue(Feedback::Cancelled));
        assert_eq!(cancelled[..fault.len()], fault[..], "the pips must be the same pips");
        assert!(cancelled.len() > fault.len(), "and the take being gone is the rest of it");
    }

    #[test]
    fn running_out_shares_no_pitch_with_a_boundary_cue() {
        let pitches = |what| {
            segments(what).into_iter().filter_map(|s| s.hz).map(|hz| hz as u32).collect::<Vec<_>>()
        };
        let running_out = pitches(Feedback::RunningOut);
        for what in [Feedback::Started, Feedback::Stopped, Feedback::Advanced] {
            for hz in pitches(what) {
                assert!(!running_out.contains(&hz), "{what:?} shares {hz} Hz with running-out");
            }
        }
        assert!(running_out.windows(2).all(|p| p[0] > p[1]), "it falls the whole way");
        assert!(segments(Feedback::RunningOut).iter().all(|s| !s.harsh));
    }

    #[test]
    fn running_out_is_slower_than_a_gesture_and_shorter_than_cancelled() {
        let ms = |what| segments(what).into_iter().map(|s| s.ms).sum::<u32>();
        for what in [Feedback::Started, Feedback::Stopped, Feedback::Advanced, Feedback::Refused] {
            assert!(ms(Feedback::RunningOut) > ms(what), "must outlast {what:?}");
        }
        assert!(ms(Feedback::RunningOut) < ms(Feedback::Cancelled));
    }

    #[test]
    fn every_cue_is_distinct() {
        for (i, a) in ALL.iter().enumerate() {
            for b in &ALL[i + 1..] {
                assert_ne!(cue(*a), cue(*b), "{a:?} and {b:?} sound identical");
            }
        }
    }

    #[test]
    fn audition() {
        let Ok(dir) = std::env::var("SOUND_DIR") else {
            return;
        };
        std::fs::create_dir_all(&dir).unwrap();
        for what in ALL {
            let pcm = cue(what);
            let bytes = (pcm.len() * 2) as u32;
            let mut wav = Vec::new();
            wav.extend(b"RIFF");
            wav.extend((36 + bytes).to_le_bytes());
            wav.extend(b"WAVEfmt ");
            wav.extend(16u32.to_le_bytes());
            wav.extend(1u16.to_le_bytes());
            wav.extend(1u16.to_le_bytes());
            wav.extend(RATE.to_le_bytes());
            wav.extend((RATE * 2).to_le_bytes());
            wav.extend(2u16.to_le_bytes());
            wav.extend(16u16.to_le_bytes());
            wav.extend(b"data");
            wav.extend(bytes.to_le_bytes());
            wav.extend(pcm.iter().flat_map(|s| s.to_le_bytes()));
            std::fs::write(format!("{dir}/{what:?}.wav").to_lowercase(), wav).unwrap();
        }
    }

    #[test]
    fn the_bad_cues_are_harsh_where_the_rest_are_clean() {
        let at_the_rails = |pcm: Vec<i16>| {
            let peak = pcm.iter().copied().map(i16::abs).max().unwrap() as f32;
            pcm.iter().filter(|s| s.unsigned_abs() as f32 > peak * 0.9).count() as f32
                / pcm.len() as f32
        };
        let refused = cue(Feedback::Refused);
        for what in [Feedback::Started, Feedback::Stopped, Feedback::MarkedFailed, Feedback::Fault]
        {
            assert!(refused.len() > cue(what).len(), "refused must outlast {what:?}");
        }
        let cancelled = cue(Feedback::Cancelled);
        for what in ALL.into_iter().filter(|w| *w != Feedback::Cancelled) {
            assert!(cancelled.len() > cue(what).len(), "cancelled must outlast {what:?}");
        }
        assert!(at_the_rails(cue(Feedback::Cancelled)) > 0.5, "cancelled should be square");
        assert!(at_the_rails(refused) > 0.8, "refused should be square");
        assert!(at_the_rails(cue(Feedback::Fault)) > 0.5, "fault should be square");
        assert!(at_the_rails(cue(Feedback::Started)) < 0.4, "started should be a sine");
    }
}
