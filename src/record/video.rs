//! The `h264` video setting: each camera's JPEGs are decoded and re-encoded
//! by the headset's hardware H.264 encoder, one thread per camera, and go
//! into `signals.mcap` as `foxglove_msgs/CompressedVideo`.
//!
//! The decode runs on the encoder's thread, never on the USB reap thread,
//! because a late URB resubmit makes the camera fade its own frames toward
//! grey, and no error counter reports it.

use std::sync::Arc;

use crate::misc::capture::Capture;

/// One camera frame for the encoder.
pub struct Job {
    pub at_ns: u64,
    pub capture: Capture,
    pub jpeg: Arc<[u8]>,
    /// The camera's measured rate, which the bitrate is sized for.
    pub hz: f32,
}

/// How long a stop waits for the encoders to hand over their last frames.
/// The head camera's last frame came ~270 ms before the stop in the episodes
/// of 2026-11-02, because its decode and encode run behind capture.
pub const FLUSH_WAIT: std::time::Duration = std::time::Duration::from_secs(1);

pub enum Push {
    Queued,
    /// The camera's encoder is behind, so the frame is dropped.
    Busy,
    /// This device gave the camera no encoder, so the caller records the JPEG.
    Unavailable,
}

/// A decoded frame in the layout the encoder takes: a full-size Y plane, then
/// a half-size plane of interleaved Cb,Cr.
pub struct Nv12 {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
}

/// Decoded to YCbCr rather than RGB, because a JPEG is YCbCr inside and so is
/// the encoder's input. Chroma is averaged over each 2x2 block, because
/// dropping three of four samples aliases on coloured edges.
pub fn nv12_from_jpeg(jpeg: &[u8]) -> anyhow::Result<Nv12> {
    use zune_jpeg::zune_core::bytestream::ZCursor;
    use zune_jpeg::zune_core::colorspace::ColorSpace;
    use zune_jpeg::zune_core::options::DecoderOptions;

    let options = DecoderOptions::default().jpeg_set_out_colorspace(ColorSpace::YCbCr);
    let mut decoder = zune_jpeg::JpegDecoder::new_with_options(ZCursor::new(jpeg), options);
    let ycc = decoder.decode().map_err(|e| anyhow::anyhow!("jpeg decode: {e:?}"))?;
    let info = decoder.info().ok_or_else(|| anyhow::anyhow!("jpeg carries no size"))?;
    let (w, h) = (info.width as usize, info.height as usize);
    if ycc.len() != w * h * 3 || w % 2 != 0 || h % 2 != 0 {
        anyhow::bail!("jpeg decoded to {} bytes for {w}x{h}", ycc.len());
    }
    let mut data = vec![0u8; w * h * 3 / 2];
    let (y, uv) = data.split_at_mut(w * h);
    for (y, px) in y.iter_mut().zip(ycc.as_chunks::<3>().0) {
        *y = px[0];
    }
    for row in 0..h / 2 {
        for col in 0..w / 2 {
            for k in 1..3 {
                let at = |r: usize, c: usize| ycc[(r * w + c) * 3 + k] as u16;
                let (r, c) = (row * 2, col * 2);
                let sum = at(r, c) + at(r, c + 1) + at(r + 1, c) + at(r + 1, c + 1);
                uv[row * w + col * 2 + k - 1] = ((sum + 2) / 4) as u8;
            }
        }
    }
    Ok(Nv12 { width: w, height: h, data })
}

/// Bits per second for a camera: 0.10 bits per pixel, the usual working point
/// for H.264 on detailed real-world video. The floor and ceiling only guard
/// against a nonsense rate.
pub fn bitrate(width: usize, height: usize, fps: f32) -> i32 {
    (width as f64 * height as f64 * fps as f64 * 0.10).clamp(2e6, 60e6) as i32
}

/// `CompressedVideo` needs the SPS and PPS in every key frame message, so a
/// reader can start at any key frame. The encoder hands them over once, in
/// its codec-config buffer, so they are put in front of each key frame that
/// does not carry them already.
pub fn with_parameter_sets(config: &[u8], frame: &[u8]) -> Vec<u8> {
    const SPS: u8 = 7;
    if first_nal_type(frame) == Some(SPS) {
        return frame.to_vec();
    }
    [config, frame].concat()
}

fn first_nal_type(annex_b: &[u8]) -> Option<u8> {
    let body = annex_b.strip_prefix(&[0, 0, 0, 1]).or_else(|| annex_b.strip_prefix(&[0, 0, 1]))?;
    body.first().map(|b| b & 0x1f)
}

/// The encoders of the three camera slots, each started on its camera's
/// first frame and kept across takes, because a MediaCodec start costs tens to
/// hundreds of milliseconds.
#[derive(Default)]
pub struct Encoders {
    #[cfg(target_os = "android")]
    slots: std::sync::Mutex<[Option<android::Slot>; 3]>,
}

#[cfg(not(target_os = "android"))]
impl Encoders {
    pub fn push(&self, _slot: usize, _job: Job) -> Push {
        Push::Unavailable
    }

    pub fn request_key_frames(&self) {}

    pub fn flush(&self) -> Vec<std::sync::mpsc::Receiver<()>> {
        Vec::new()
    }
}

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub(crate) mod media_codec;
#[cfg(target_os = "android")]
pub(crate) use media_codec::{Decoded, Decoder, Encoder};

#[cfg(test)]
mod tests {
    use super::*;

    /// Red left half, blue right half, saved at quality 100 without chroma
    /// subsampling, so each 2x2 block keeps its half's colour. Pillow puts
    /// red at YCbCr (87, 101, 208) and blue at (58, 208, 115).
    #[test]
    fn a_jpeg_packs_to_nv12_with_each_half_keeping_its_colour() {
        let frame =
            nv12_from_jpeg(include_bytes!("../../assets/test/two_halves_16x8.jpg")).unwrap();
        assert_eq!((frame.width, frame.height, frame.data.len()), (16, 8, 16 * 8 * 3 / 2));
        let near = |got: u8, want: u8| (got as i32 - want as i32).abs() <= 3;
        let (y, uv) = frame.data.split_at(16 * 8);
        for row in 0..8 {
            assert!(near(y[row * 16 + 3], 87) && near(y[row * 16 + 12], 58), "luma row {row}");
        }
        for row in 0..4 {
            let pair = |col: usize| (uv[row * 16 + col * 2], uv[row * 16 + col * 2 + 1]);
            let (red, blue) = (pair(1), pair(6));
            assert!(near(red.0, 101) && near(red.1, 208), "red chroma {red:?}");
            assert!(near(blue.0, 208) && near(blue.1, 115), "blue chroma {blue:?}");
        }
    }

    #[test]
    fn a_key_frame_gets_the_parameter_sets_once() {
        let config = [0, 0, 0, 1, 0x67, 0xaa, 0, 0, 0, 1, 0x68, 0xbb];
        let idr = [0, 0, 0, 1, 0x65, 0xcc];
        assert_eq!(with_parameter_sets(&config, &idr), [&config[..], &idr[..]].concat());
        let already = [&config[..], &idr[..]].concat();
        assert_eq!(with_parameter_sets(&config, &already), already);
        assert_eq!(first_nal_type(&[0, 0, 1, 0x41]), Some(1));
    }

    #[test]
    fn the_bitrate_follows_the_pixel_rate() {
        assert_eq!(bitrate(640, 480, 100.0), 3_072_000);
        assert_eq!(bitrate(64, 64, 1.0), 2_000_000);
        assert_eq!(bitrate(8000, 8000, 120.0), 60_000_000);
    }
}
