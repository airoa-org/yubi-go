//! The thread each camera's encoder runs on, behind [`super::Encoders`]: it
//! decodes the camera's JPEGs on a small pool, feeds the frames to its
//! [`Encoder`], and hands the encoded frames to the hub.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::media_codec::Encoder;
use super::{Encoders, Job, Nv12, Push, FLUSH_WAIT};
use crate::misc::capture::Capture;

pub(super) struct Slot {
    jobs: SyncSender<Msg>,
    refused: Arc<AtomicBool>,
}

enum Msg {
    Frame(Job),
    KeyFrame,
    /// Answered once every frame sent before it has left the encoder.
    Flush(SyncSender<()>),
}

/// About a third of a second of a wrist camera.
const QUEUE: usize = 32;

/// How long the thread waits for a frame before it collects finished output
/// anyway, so a take's last frames leave the encoder without a next input.
const DRAIN_WAIT: Duration = Duration::from_millis(20);

/// Frames reach the encoder only while the rig is armed or recording, so a
/// pause this long means a new run of frames, which must open on a key frame.
/// 200 ms is 20 wrist frames: a camera never pauses that long mid-run.
const GAP_NS: u64 = 200_000_000;

/// A frame that waited longer than this in the queue is skipped undecoded.
/// Queue length adds latency, not throughput: the 3840x1080 head camera
/// decodes at ~16 fps, so a full queue held it 2 s behind, and a take's stop
/// cut its last 2 s. 200 ms is under the wrists'
/// 32 frames at 100 fps, so a wrist that keeps up never skips one.
const STALE_NS: u64 = 200_000_000;

const STATUS_PERIOD: Duration = Duration::from_secs(1);

impl Encoders {
    pub fn push(&self, slot: usize, job: Job) -> Push {
        let mut slots = self.slots.lock().unwrap();
        let Some(entry) = slots.get_mut(slot) else { return Push::Unavailable };
        let s = entry.get_or_insert_with(|| spawn(slot));
        if s.refused.load(Ordering::Relaxed) {
            return Push::Unavailable;
        }
        match s.jobs.try_send(Msg::Frame(job)) {
            Ok(()) => Push::Queued,
            Err(TrySendError::Full(_)) => Push::Busy,
            Err(TrySendError::Disconnected(_)) => Push::Unavailable,
        }
    }

    /// A take is starting: every running encoder opens its next frame as a
    /// key frame, so the take's video starts without waiting a GOP.
    pub fn request_key_frames(&self) {
        for s in self.slots.lock().unwrap().iter().flatten() {
            let _ = s.jobs.try_send(Msg::KeyFrame);
        }
    }

    /// A take is stopping: one answer per running encoder, sent when every
    /// frame pushed before now has come out of it. The send blocks, so it
    /// happens outside the lock that `push` takes on the camera's thread.
    pub fn flush(&self) -> Vec<Receiver<()>> {
        let senders: Vec<_> =
            self.slots.lock().unwrap().iter().flatten().map(|s| s.jobs.clone()).collect();
        senders
            .into_iter()
            .filter_map(|jobs| {
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                jobs.send(Msg::Flush(tx)).ok().map(|_| rx)
            })
            .collect()
    }
}

fn spawn(slot: usize) -> Slot {
    let (jobs, rx) = std::sync::mpsc::sync_channel(QUEUE);
    let refused = Arc::new(AtomicBool::new(false));
    let flag = refused.clone();
    let name = crate::record::mcap::CAM_NAMES[slot];
    let spawned = std::thread::Builder::new()
        .name(format!("encode-{name}"))
        .spawn(move || run(slot, rx, &flag));
    if let Err(e) = spawned {
        log::error!("{name} camera: no encoder thread ({e}) — recording its JPEGs");
        refused.store(true, Ordering::Relaxed);
    }
    Slot { jobs, refused }
}

#[derive(Default)]
struct Counts {
    frames_in: u64,
    frames_out: u64,
    decode_failures: u64,
    encoder_busy: u64,
    stale: u64,
}

/// One camera's decoded frames in flight, oldest first, each with the
/// channel its decoder answers on.
type InFlight = VecDeque<(Job, Receiver<anyhow::Result<Nv12>>)>;

fn run(slot: usize, rx: Receiver<Msg>, refused: &AtomicBool) {
    let name = crate::record::mcap::CAM_NAMES[slot];
    let decode = spawn_decoders(name);
    let mut in_flight: InFlight = VecDeque::new();
    let mut state = State {
        slot,
        name,
        hub: crate::app::hub(),
        encoder: None,
        pending: VecDeque::new(),
        last_at: None,
        counts: Counts::default(),
    };
    let mut status_at = Instant::now();
    loop {
        match rx.recv_timeout(DRAIN_WAIT) {
            Ok(Msg::KeyFrame) => {
                if let Some(e) = &state.encoder {
                    e.request_key_frame();
                }
            }
            Ok(Msg::Frame(job)) => {
                state.counts.frames_in += 1;
                if crate::misc::clock::boottime_ns().saturating_sub(job.at_ns) > STALE_NS {
                    state.counts.stale += 1;
                } else {
                    if in_flight.len() == DECODERS {
                        let (job, done) = in_flight.pop_front().unwrap();
                        let decoded =
                            done.recv().unwrap_or_else(|_| Err(anyhow::anyhow!("decoder gone")));
                        if !state.encode(job, decoded, refused) {
                            return;
                        }
                    }
                    let (answer, done) = std::sync::mpsc::sync_channel(1);
                    if decode.send((job.jpeg.clone(), answer)).is_err() {
                        return;
                    }
                    in_flight.push_back((job, done));
                }
            }
            Ok(Msg::Flush(flushed)) => {
                while let Some((job, done)) = in_flight.pop_front() {
                    let decoded =
                        done.recv().unwrap_or_else(|_| Err(anyhow::anyhow!("decoder gone")));
                    if !state.encode(job, decoded, refused) {
                        return;
                    }
                }
                // A frame the codec drops never comes out, so the wait is bounded.
                let until = Instant::now() + FLUSH_WAIT;
                state.drain();
                while !state.pending.is_empty() && Instant::now() < until {
                    std::thread::sleep(Duration::from_millis(2));
                    state.drain();
                }
                let _ = flushed.send(());
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        while let Some(decoded) = in_flight.front().and_then(|(_, done)| done.try_recv().ok()) {
            let (job, _) = in_flight.pop_front().unwrap();
            if !state.encode(job, decoded, refused) {
                return;
            }
        }
        state.drain();
        if status_at.elapsed() >= STATUS_PERIOD {
            status_at = Instant::now();
            state.report();
        }
    }
}

/// Decode threads per camera, which share its frames and answer in any
/// order. One thread decodes the 3840x1080 head camera at ~16 fps
/// on the headset, so its 30 fps needs two; the third is headroom.
const DECODERS: usize = 3;

type DecodeJob = (Arc<[u8]>, SyncSender<anyhow::Result<Nv12>>);

/// The pool's threads exit when the returned sender is dropped.
fn spawn_decoders(name: &str) -> SyncSender<DecodeJob> {
    let (tx, rx) = std::sync::mpsc::sync_channel::<DecodeJob>(DECODERS);
    let rx = Arc::new(std::sync::Mutex::new(rx));
    for i in 0..DECODERS {
        let rx = rx.clone();
        let spawned =
            std::thread::Builder::new().name(format!("decode-{name}-{i}")).spawn(move || loop {
                let Ok((jpeg, answer)) = rx.lock().unwrap().recv() else { return };
                let _ = answer.send(super::nv12_from_jpeg(&jpeg));
            });
        if let Err(e) = spawned {
            log::warn!("{name} camera: decode thread {i} not started ({e})");
        }
    }
    tx
}

struct State {
    slot: usize,
    name: &'static str,
    hub: &'static Arc<crate::hub::Hub>,
    encoder: Option<Encoder>,
    // Each submitted frame's stamps, matched to its output by timestamp. The
    // encoder runs without B-frames, so output comes in input order.
    pending: VecDeque<(u64, u64, Capture)>,
    last_at: Option<u64>,
    counts: Counts,
}

impl State {
    /// `false` when the device has no encoder for this camera, which then
    /// records its JPEGs.
    fn encode(&mut self, job: Job, decoded: anyhow::Result<Nv12>, refused: &AtomicBool) -> bool {
        let name = self.name;
        let frame = match decoded {
            Ok(frame) => frame,
            Err(e) => {
                self.counts.decode_failures += 1;
                log::debug!("{name} camera: frame not encoded — {e}");
                return true;
            }
        };
        if self.encoder.as_ref().is_none_or(|e| e.size() != (frame.width, frame.height)) {
            // A camera at its rate before the first measurement is
            // assumed at 30 fps, the common UVC default.
            let fps = if job.hz > 1.0 { job.hz } else { 30.0 };
            // The old codec goes first: the device has few to give.
            drop(self.encoder.take());
            self.pending.clear();
            match Encoder::new(frame.width, frame.height, fps) {
                Ok(e) => self.encoder = Some(e),
                Err(e) => {
                    log::error!("{name} camera: no H.264 encoder ({e}) — recording its JPEGs");
                    refused.store(true, Ordering::Relaxed);
                    return false;
                }
            }
        }
        let Some(e) = &mut self.encoder else { return true };
        if self.last_at.is_none_or(|t| job.at_ns.saturating_sub(t) > GAP_NS) {
            e.request_key_frame();
        }
        self.last_at = Some(job.at_ns);
        let pts_us = job.at_ns / 1000;
        match e.submit(&frame, pts_us) {
            Ok(true) => self.pending.push_back((pts_us, job.at_ns, job.capture)),
            Ok(false) => self.counts.encoder_busy += 1,
            Err(err) => log::warn!("{name} camera: encoder refused a frame — {err}"),
        }
        true
    }

    fn drain(&mut self) {
        let Some(e) = &mut self.encoder else { return };
        let (pending, counts, hub, slot) =
            (&mut self.pending, &mut self.counts, self.hub, self.slot);
        e.drain(|pts_us, data, key_frame| {
            while pending.front().is_some_and(|&(pts, _, _)| pts < pts_us) {
                pending.pop_front();
            }
            let Some((_, at_ns, capture)) =
                pending.front().filter(|&&(pts, _, _)| pts == pts_us).copied()
            else {
                return;
            };
            pending.pop_front();
            counts.frames_out += 1;
            hub.record_video(slot, at_ns, capture, data, key_frame);
        });
    }

    fn report(&self) {
        let Some(e) = &self.encoder else { return };
        let counts = &self.counts;
        self.hub.set_status(
            crate::misc::status::Status::new(format!("video/{}", self.name))
                .with("encoder", &e.name)
                .with("size", format!("{}x{}", e.width, e.height))
                .with("fps", format!("{:.1}", e.fps))
                .with("bitrate_bps", e.bitrate)
                .with("frames_in", counts.frames_in)
                .with("frames_out", counts.frames_out)
                .with("decode_failures", counts.decode_failures)
                .with("encoder_busy", counts.encoder_busy)
                .with("stale", counts.stale),
        );
    }
}
