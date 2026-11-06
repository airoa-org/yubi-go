//! The headset's hardware codecs behind [`super::Video`]: each row's chosen
//! frame decoded, fitted to the dataset's size, and encoded again with every
//! frame a key frame.

use std::path::Path;
use std::time::{Duration, Instant};

use super::picture::{self, HEIGHT, WIDTH};
use super::signals::FPS;
use super::table::Rgb;
use crate::record::playback::{Codec, Index};
use crate::record::video::{nv12_from_jpeg, Decoded, Decoder, Encoder, Nv12};

/// Every frame a key frame, as AIRoA's internal converter writes YUBI
/// datasets (`gop: 1`): a trainer seeks to any frame without decoding its neighbours,
/// which it measured as 8.5x faster.
const KEY_INTERVAL_S: i32 = 0;

/// 1.5x what x264 spends at `crf 20` all-intra, the internal converter's
/// setting: 4.9 Mbps on a real wrist take (20.4 kB a
/// frame). A hardware encoder needs more bits for the same picture, and how
/// many more is not yet measured on the headset's.
const BITRATE: i32 = 7_400_000;

/// How long the encoder may take to hand back its last frames once fed.
const FLUSH: Duration = Duration::from_secs(5);

/// What the decoder is waiting on between polls.
const POLL_US: i64 = 10_000;

pub struct Transcoder {
    /// Whether to give up: a take has started and needs the codecs and cores.
    pub stop: fn() -> bool,
}

struct Out {
    stop: fn() -> bool,
    encoder: Encoder,
    rgb: Rgb,
    units: Vec<Vec<u8>>,
    fed: usize,
}

impl Out {
    fn new(stop: fn() -> bool) -> anyhow::Result<Out> {
        let encoder = Encoder::open(WIDTH, HEIGHT, FPS as f32, BITRATE, KEY_INTERVAL_S)?;
        Ok(Out { stop, encoder, rgb: Rgb::default(), units: Vec::new(), fed: 0 })
    }

    fn collect(&mut self) -> anyhow::Result<()> {
        let mut inter = false;
        let units = &mut self.units;
        self.encoder.drain(|_, data, key| {
            inter |= !key;
            units.push(data);
        });
        if inter {
            anyhow::bail!("the encoder made a frame that is not a key frame");
        }
        Ok(())
    }

    /// The next row's picture, at its place on the 30 Hz grid.
    fn push(&mut self, frame: &Nv12) -> anyhow::Result<()> {
        if (self.stop)() {
            anyhow::bail!("recording started, so the export stopped");
        }
        picture::add(&mut self.rgb, frame);
        let pts_us = self.fed as u64 * 1_000_000 / FPS;
        while !self.encoder.submit(frame, pts_us)? {
            self.collect()?;
        }
        self.fed += 1;
        self.collect()
    }

    fn finish(mut self, out: &Path) -> anyhow::Result<Rgb> {
        let since = Instant::now();
        while self.units.len() < self.fed && since.elapsed() < FLUSH {
            std::thread::sleep(Duration::from_millis(5));
            self.collect()?;
        }
        if self.units.len() != self.fed {
            anyhow::bail!("the encoder gave back {} of {} frames", self.units.len(), self.fed);
        }
        let mp4 = crate::record::mp4::file(&self.units, FPS)
            .ok_or_else(|| anyhow::anyhow!("the encoder's first frame carries no SPS and PPS"))?;
        std::fs::write(out, mp4)?;
        Ok(self.rgb)
    }
}

impl super::Video for Transcoder {
    fn write(
        &self,
        index: &Index,
        slot: usize,
        picks: &[usize],
        out: &Path,
    ) -> anyhow::Result<Rgb> {
        let camera = index.cameras[slot].as_ref().ok_or_else(|| anyhow::anyhow!("no camera"))?;
        let mut encoded = Out::new(self.stop)?;
        match camera.codec {
            Codec::Jpeg => {
                let mut shown: Option<(usize, Nv12)> = None;
                for &pick in picks {
                    if shown.as_ref().is_none_or(|(i, _)| *i != pick) {
                        let jpeg = index.bytes(slot, pick).expect("a picked frame exists")?;
                        shown = Some((pick, picture::fit(nv12_from_jpeg(&jpeg)?)));
                    }
                    encoded.push(&shown.as_ref().unwrap().1)?;
                }
            }
            Codec::H264 => decode(index, slot, picks, &mut encoded)?,
        }
        encoded.finish(out)
    }
}

/// Decodes from the key frame before the first pick through the last, and
/// feeds each row the picture it picked. A frame the decoder drops is stood
/// in for by the next one it gives back.
fn decode(index: &Index, slot: usize, picks: &[usize], out: &mut Out) -> anyhow::Result<()> {
    let frames = &index.cameras[slot].as_ref().unwrap().frames;
    let (first, last) = (picks[0], *picks.last().unwrap());
    let start = frames[..=first].iter().rposition(|f| f.key).unwrap_or(0);
    let unit = |i: usize| index.bytes(slot, i).expect("an indexed frame exists");
    let sets = crate::record::mp4::parameter_sets(&unit(start)?)
        .ok_or_else(|| anyhow::anyhow!("the key frame carries no SPS and PPS"))?;
    let (w, h) = crate::record::mp4::dimensions(&sets.0)
        .ok_or_else(|| anyhow::anyhow!("the SPS gives no picture size"))?;
    let mut decoder = Decoder::open(w as usize, h as usize)?;

    let mut row = 0;
    // The pts is the frame's number, which comes back with its picture.
    let mut place = |pts: u64, frame: Nv12, out: &mut Out| -> anyhow::Result<()> {
        if row < picks.len() && picks[row] <= pts as usize {
            let fitted = picture::fit(frame);
            while row < picks.len() && picks[row] <= pts as usize {
                out.push(&fitted)?;
                row += 1;
            }
        }
        Ok(())
    };
    let mut ended = false;
    for i in start..=last {
        let data = unit(i)?;
        loop {
            let queued = decoder.submit(&data, i as u64, i == last)?;
            loop {
                match decoder.next(0)? {
                    Decoded::Picture(pts, frame) => place(pts, frame, out)?,
                    Decoded::Pending => break,
                    Decoded::Ended => ended = true,
                }
            }
            if queued {
                break;
            }
        }
    }
    let since = Instant::now();
    while !ended {
        match decoder.next(POLL_US)? {
            Decoded::Picture(pts, frame) => place(pts, frame, out)?,
            Decoded::Ended => ended = true,
            Decoded::Pending if since.elapsed() > FLUSH => {
                anyhow::bail!("the decoder stopped short of the end of the stream")
            }
            Decoded::Pending => {}
        }
    }
    if row < picks.len() {
        anyhow::bail!("the decoder gave back pictures for {row} of {} rows", picks.len());
    }
    Ok(())
}
