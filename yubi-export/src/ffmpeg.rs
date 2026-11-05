//! ffmpeg behind [`Video`], in place of the headset's hardware codecs: each
//! row's chosen frame decoded, fitted to the dataset's size by the headset's
//! own code, and encoded by x264 with every frame a key frame.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::thread::JoinHandle;

use anyhow::{bail, Context};
use yubi_go::record::export::picture::{self, HEIGHT, WIDTH};
use yubi_go::record::export::table::Rgb;
use yubi_go::record::export::Video;
use yubi_go::record::mp4;
use yubi_go::record::playback::{Codec, Index};
use yubi_go::record::video::{nv12_from_jpeg, Nv12};

/// The YUBI setting of AIRoA's internal converter (`gop: 1`, `crf: 20`), which the
/// headset's 7.4 Mbps was sized against.
const X264: [&str; 8] = ["-c:v", "libx264", "-crf", "20", "-g", "1", "-x264-params", "aud=1"];

/// Full-range BT.601, which is what a JPEG holds and what the headset's
/// encoder labels its stream (`video/android.rs`, `COLOR_RANGE_FULL`).
const COLOUR: [&str; 8] = [
    "-color_range",
    "pc",
    "-colorspace",
    "smpte170m",
    "-color_primaries",
    "smpte170m",
    "-color_trc",
    "smpte170m",
];

pub struct Ffmpeg;

fn ffmpeg() -> Command {
    let mut c = Command::new("ffmpeg");
    c.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
    c
}

fn spawn(mut c: Command) -> anyhow::Result<Child> {
    c.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    c.spawn().context("cannot run ffmpeg; is it installed and on PATH?")
}

/// The child's whole stdout, read on its own thread so neither pipe fills.
fn drain(child: &mut Child) -> JoinHandle<std::io::Result<Vec<u8>>> {
    let mut stdout = child.stdout.take().unwrap();
    std::thread::spawn(move || {
        let mut out = Vec::new();
        stdout.read_to_end(&mut out).map(|_| out)
    })
}

struct Out {
    child: Child,
    stdin: ChildStdin,
    stream: JoinHandle<std::io::Result<Vec<u8>>>,
    rgb: Rgb,
    fed: usize,
}

impl Out {
    fn new() -> anyhow::Result<Out> {
        let size = format!("{WIDTH}x{HEIGHT}");
        let mut c = ffmpeg();
        c.args(["-f", "rawvideo", "-pix_fmt", "nv12", "-s", &size, "-r", "30"])
            .args(COLOUR)
            .args(["-i", "-"])
            .args(X264)
            .args(COLOUR)
            .args(["-f", "h264", "-"]);
        let mut child = spawn(c)?;
        let stdin = child.stdin.take().unwrap();
        let stream = drain(&mut child);
        Ok(Out { child, stdin, stream, rgb: Rgb::default(), fed: 0 })
    }

    fn push(&mut self, frame: &Nv12) -> anyhow::Result<()> {
        picture::add(&mut self.rgb, frame);
        self.stdin.write_all(&frame.data).context("ffmpeg stopped taking frames")?;
        self.fed += 1;
        Ok(())
    }

    fn finish(self, out: &Path) -> anyhow::Result<Rgb> {
        let Out { mut child, stdin, stream, rgb, fed } = self;
        drop(stdin);
        let stream = stream.join().expect("the reader does not panic")?;
        if !child.wait()?.success() {
            bail!("ffmpeg failed to encode {}", out.display());
        }
        let units = access_units(&stream);
        if units.len() != fed {
            bail!("x264 gave back {} of {fed} frames", units.len());
        }
        if !units.iter().all(|u| mp4::is_key(u)) {
            bail!("x264 made a frame that is not a key frame");
        }
        let file = mp4::file(&units, 30).context("x264's first frame carries no SPS and PPS")?;
        std::fs::write(out, file)?;
        Ok(rgb)
    }
}

/// An Annex B stream cut into frames at its access unit delimiters, which
/// `aud=1` has x264 put before every frame.
fn access_units(stream: &[u8]) -> Vec<Vec<u8>> {
    let mut units: Vec<Vec<u8>> = Vec::new();
    for nal in mp4::nal_units(stream) {
        if nal[0] & 0x1f == 9 || units.is_empty() {
            units.push(Vec::new());
        }
        let unit = units.last_mut().unwrap();
        unit.extend_from_slice(&[0, 0, 0, 1]);
        unit.extend_from_slice(nal);
    }
    units
}

impl Video for Ffmpeg {
    fn write(
        &self,
        index: &Index,
        slot: usize,
        picks: &[usize],
        out: &Path,
    ) -> anyhow::Result<Rgb> {
        let camera = index.cameras[slot].as_ref().context("no camera")?;
        let mut encoded = Out::new()?;
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
/// feeds each row the picture it picked. ffmpeg hands back one picture per
/// frame in order, so the `k`th picture is frame `start + k`.
fn decode(index: &Index, slot: usize, picks: &[usize], out: &mut Out) -> anyhow::Result<()> {
    let frames = &index.cameras[slot].as_ref().unwrap().frames;
    let (first, last) = (picks[0], *picks.last().unwrap());
    let start = frames[..=first].iter().rposition(|f| f.key).unwrap_or(0);
    let units: Vec<Vec<u8>> = (start..=last)
        .map(|i| index.bytes(slot, i).expect("an indexed frame exists"))
        .collect::<std::io::Result<_>>()?;
    let sets = mp4::parameter_sets(&units[0]).context("the key frame carries no SPS and PPS")?;
    let (w, h) = mp4::dimensions(&sets.0).context("the SPS gives no picture size")?;
    let (w, h) = (w as usize, h as usize);

    // Both ranges pinned, so the samples pass through as the headset's decoder
    // hands them over. Left to itself, Ubuntu 24.04's ffmpeg 6.1 squeezed the
    // full-range stream into limited range (black at 11, not 0), where
    // Homebrew's 8.1 did not (tools/export-parity.py, 2026-11-01).
    let mut c = ffmpeg();
    c.args(["-f", "h264", "-i", "-", "-fps_mode", "passthrough"])
        .args(["-vf", "scale=in_range=full:out_range=full", "-f", "rawvideo"])
        .args(["-pix_fmt", "nv12", "-"]);
    let mut child = spawn(c)?;
    let mut stdin = child.stdin.take().unwrap();
    let feeder = std::thread::spawn(move || units.iter().try_for_each(|u| stdin.write_all(u)));
    let mut stdout = child.stdout.take().unwrap();

    let mut row = 0;
    let mut frame = start;
    loop {
        let mut data = vec![0u8; w * h * 3 / 2];
        match stdout.read_exact(&mut data) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        if row < picks.len() && picks[row] <= frame {
            let fitted = picture::fit(Nv12 { width: w, height: h, data });
            while row < picks.len() && picks[row] <= frame {
                out.push(&fitted)?;
                row += 1;
            }
        }
        frame += 1;
    }
    feeder.join().expect("the feeder does not panic").context("ffmpeg stopped taking frames")?;
    if !child.wait()?.success() {
        bail!("ffmpeg failed to decode camera {slot}");
    }
    if frame != last + 1 {
        bail!("ffmpeg decoded {} of {} frames", frame - start, last + 1 - start);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stream_splits_at_each_delimiter() {
        let s = [
            0, 0, 0, 1, 9, 0xf0, 0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x65, 2, 0, 0, 0, 1, 9, 0xf0, 0, 0,
            1, 0x65, 3,
        ];
        let units = access_units(&s);
        assert_eq!(units.len(), 2);
        assert_eq!(units[1], [0, 0, 0, 1, 9, 0xf0, 0, 0, 0, 1, 0x65, 3]);
    }
}
