//! The headset's hardware H.264 codecs through the NDK's MediaCodec: an
//! [`Encoder`] fed NV12 frames and a [`Decoder`] that hands them back. NDK
//! `extern` declarations rather than a binding crate: about fifteen stable
//! functions from API 21–29.

use std::ffi::{c_char, c_void, CStr, CString};

use super::Nv12;

pub(crate) enum Codec {}
pub(crate) enum Format {}

#[repr(C)]
#[derive(Default)]
pub(crate) struct BufferInfo {
    pub offset: i32,
    pub size: i32,
    pub pts_us: i64,
    pub flags: u32,
}

pub(crate) const AMEDIA_OK: i32 = 0;
const CONFIGURE_FLAG_ENCODE: u32 = 1;
const BUFFER_FLAG_KEY_FRAME: u32 = 1;
const BUFFER_FLAG_CODEC_CONFIG: u32 = 2;
const BUFFER_FLAG_END_OF_STREAM: u32 = 4;
/// `COLOR_FormatYUV420SemiPlanar`: NV12, which Qualcomm encoders take without
/// converting.
const COLOR_FORMAT_NV12: i32 = 21;
/// `COLOR_FormatYUV420Planar`: I420, which some decoders hand back instead.
const COLOR_FORMAT_I420: i32 = 19;
/// `COLOR_FormatYUV420Flexible`, which the Quest's `c2.qti.avc.decoder` reports
/// for the NV12 it was asked for (2026-10-28).
const COLOR_FORMAT_FLEXIBLE: i32 = 0x7F42_0888;
/// Qualcomm's NV12 with its rows and planes padded, which its decoders hand
/// back for a flexible request.
const COLOR_FORMAT_QCOM_NV12: i32 = 0x7FA3_0C04;
/// `COLOR_RANGE_FULL`, `COLOR_STANDARD_BT601_NTSC` and
/// `COLOR_TRANSFER_SDR_VIDEO`: JPEG's YCbCr spans 0–255 in BT.601, and an
/// unlabelled stream is played as limited range, which blows out the whites.
const COLOR_RANGE_FULL: i32 = 1;
const COLOR_STANDARD_BT601: i32 = 4;
const COLOR_TRANSFER_SDR: i32 = 3;
/// A key frame each second, so a reader can seek to within one second and the
/// 2 s pre-roll always holds one.
const KEY_FRAME_INTERVAL_S: i32 = 1;
/// How long `submit` waits for a free input buffer: two wrist frames. The
/// wait stalls only this camera's thread.
const INPUT_WAIT_US: i64 = 20_000;

#[link(name = "mediandk")]
extern "C" {
    pub(crate) fn AMediaCodec_createEncoderByType(mime: *const c_char) -> *mut Codec;
    pub(crate) fn AMediaCodec_createDecoderByType(mime: *const c_char) -> *mut Codec;
    pub(crate) fn AMediaCodec_configure(
        codec: *mut Codec,
        format: *const Format,
        surface: *mut c_void,
        crypto: *mut c_void,
        flags: u32,
    ) -> i32;
    pub(crate) fn AMediaCodec_start(codec: *mut Codec) -> i32;
    pub(crate) fn AMediaCodec_stop(codec: *mut Codec) -> i32;
    pub(crate) fn AMediaCodec_delete(codec: *mut Codec) -> i32;
    pub(crate) fn AMediaCodec_getName(codec: *mut Codec, name: *mut *mut c_char) -> i32;
    pub(crate) fn AMediaCodec_releaseName(codec: *mut Codec, name: *mut c_char);
    pub(crate) fn AMediaCodec_getInputFormat(codec: *mut Codec) -> *mut Format;
    pub(crate) fn AMediaCodec_getOutputFormat(codec: *mut Codec) -> *mut Format;
    pub(crate) fn AMediaCodec_dequeueInputBuffer(codec: *mut Codec, timeout_us: i64) -> isize;
    pub(crate) fn AMediaCodec_getInputBuffer(
        codec: *mut Codec,
        idx: usize,
        size: *mut usize,
    ) -> *mut u8;
    pub(crate) fn AMediaCodec_queueInputBuffer(
        codec: *mut Codec,
        idx: usize,
        offset: i64,
        size: usize,
        time_us: u64,
        flags: u32,
    ) -> i32;
    pub(crate) fn AMediaCodec_dequeueOutputBuffer(
        codec: *mut Codec,
        info: *mut BufferInfo,
        timeout_us: i64,
    ) -> isize;
    pub(crate) fn AMediaCodec_getOutputBuffer(
        codec: *mut Codec,
        idx: usize,
        size: *mut usize,
    ) -> *mut u8;
    pub(crate) fn AMediaCodec_releaseOutputBuffer(
        codec: *mut Codec,
        idx: usize,
        render: bool,
    ) -> i32;
    pub(crate) fn AMediaCodec_setParameters(codec: *mut Codec, params: *const Format) -> i32;

    pub(crate) fn AMediaFormat_new() -> *mut Format;
    pub(crate) fn AMediaFormat_delete(format: *mut Format) -> i32;
    pub(crate) fn AMediaFormat_setString(
        format: *mut Format,
        key: *const c_char,
        value: *const c_char,
    );
    pub(crate) fn AMediaFormat_setInt32(format: *mut Format, key: *const c_char, value: i32);
    pub(crate) fn AMediaFormat_getInt32(
        format: *const Format,
        key: *const c_char,
        out: *mut i32,
    ) -> bool;
}

pub(crate) fn key(name: &str) -> CString {
    CString::new(name).expect("format keys are static ASCII")
}

/// A hardware H.264 encoder fed NV12 frames, owned by one thread.
pub(crate) struct Encoder {
    codec: *mut Codec,
    pub(super) name: String,
    pub(super) width: usize,
    pub(super) height: usize,
    pub(super) fps: f32,
    pub(super) bitrate: i32,
    /// The input layout the encoder asked for, which may pad rows and planes.
    stride: usize,
    slice_height: usize,
    /// The SPS and PPS, from the encoder's codec-config buffer.
    config: Vec<u8>,
}

impl Encoder {
    pub(super) fn new(width: usize, height: usize, fps: f32) -> anyhow::Result<Encoder> {
        Encoder::open(width, height, fps, super::bitrate(width, height, fps), KEY_FRAME_INTERVAL_S)
    }

    /// `key_interval_s` 0 makes every frame a key frame.
    pub(crate) fn open(
        width: usize,
        height: usize,
        fps: f32,
        bitrate: i32,
        key_interval_s: i32,
    ) -> anyhow::Result<Encoder> {
        let mime = key("video/avc");
        // SAFETY: each pointer is checked before use, and the format and a
        // codec that failed to start are deleted on every path out.
        unsafe {
            let codec = AMediaCodec_createEncoderByType(mime.as_ptr());
            if codec.is_null() {
                anyhow::bail!("no video/avc encoder");
            }
            let format = AMediaFormat_new();
            let set = |k: &str, v: i32| AMediaFormat_setInt32(format, key(k).as_ptr(), v);
            AMediaFormat_setString(format, key("mime").as_ptr(), mime.as_ptr());
            set("width", width as i32);
            set("height", height as i32);
            set("color-format", COLOR_FORMAT_NV12);
            set("bitrate", bitrate);
            set("frame-rate", fps.round() as i32);
            set("i-frame-interval", key_interval_s);
            // `CompressedVideo` refuses B-frames, which need later frames to
            // decode an earlier one.
            set("max-bframes", 0);
            set("color-range", COLOR_RANGE_FULL);
            set("color-standard", COLOR_STANDARD_BT601);
            set("color-transfer", COLOR_TRANSFER_SDR);
            let status = AMediaCodec_configure(
                codec,
                format,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                CONFIGURE_FLAG_ENCODE,
            );
            AMediaFormat_delete(format);
            if status != AMEDIA_OK || AMediaCodec_start(codec) != AMEDIA_OK {
                AMediaCodec_delete(codec);
                anyhow::bail!("configure or start {width}x{height} failed ({status})");
            }
            let mut raw: *mut c_char = std::ptr::null_mut();
            let mut name = String::new();
            if AMediaCodec_getName(codec, &mut raw) == AMEDIA_OK && !raw.is_null() {
                name = CStr::from_ptr(raw).to_string_lossy().into_owned();
                AMediaCodec_releaseName(codec, raw);
            }
            let (mut stride, mut slice_height) = (width as i32, height as i32);
            let input = AMediaCodec_getInputFormat(codec);
            if !input.is_null() {
                AMediaFormat_getInt32(input, key("stride").as_ptr(), &mut stride);
                AMediaFormat_getInt32(input, key("slice-height").as_ptr(), &mut slice_height);
                AMediaFormat_delete(input);
            }
            let (stride, slice_height) =
                ((stride as usize).max(width), (slice_height as usize).max(height));
            log::info!(
                "encoder {name}: {width}x{height} @{fps:.1} Hz, {} kbps, stride {stride}, slice height {slice_height}",
                bitrate / 1000
            );
            Ok(Encoder {
                codec,
                name,
                width,
                height,
                fps,
                bitrate,
                stride,
                slice_height,
                config: Vec::new(),
            })
        }
    }

    pub(super) fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// `false` when no input buffer came free within [`INPUT_WAIT_US`], so
    /// the frame was dropped.
    pub(crate) fn submit(&mut self, frame: &Nv12, pts_us: u64) -> anyhow::Result<bool> {
        let (w, h) = (self.width, self.height);
        let needed = self.stride * self.slice_height + self.stride * (h / 2);
        // SAFETY: `idx` is the codec's and goes back to it on every path.
        unsafe {
            let idx = AMediaCodec_dequeueInputBuffer(self.codec, INPUT_WAIT_US);
            if idx < 0 {
                return Ok(false);
            }
            let idx = idx as usize;
            let mut capacity = 0usize;
            let buffer = AMediaCodec_getInputBuffer(self.codec, idx, &mut capacity);
            if buffer.is_null() || capacity < needed {
                AMediaCodec_queueInputBuffer(self.codec, idx, 0, 0, pts_us, 0);
                anyhow::bail!("input buffer holds {capacity} bytes, a frame needs {needed}");
            }
            let dst = std::slice::from_raw_parts_mut(buffer, needed);
            let (y, uv) = frame.data.split_at(w * h);
            for row in 0..h {
                dst[row * self.stride..][..w].copy_from_slice(&y[row * w..][..w]);
            }
            let chroma = &mut dst[self.stride * self.slice_height..];
            for row in 0..h / 2 {
                chroma[row * self.stride..][..w].copy_from_slice(&uv[row * w..][..w]);
            }
            let status = AMediaCodec_queueInputBuffer(self.codec, idx, 0, needed, pts_us, 0);
            if status != AMEDIA_OK {
                anyhow::bail!("queueInputBuffer: {status}");
            }
        }
        Ok(true)
    }

    /// Hand every finished frame to `sink` as (pts, Annex B bytes, key frame),
    /// key frames with the SPS and PPS in front.
    pub(crate) fn drain(&mut self, mut sink: impl FnMut(u64, Vec<u8>, bool)) {
        loop {
            let mut info = BufferInfo::default();
            // SAFETY: an index >= 0 is an output buffer released below.
            let idx = unsafe { AMediaCodec_dequeueOutputBuffer(self.codec, &mut info, 0) };
            // -2 and -3 are the format and buffers changing, which need nothing
            // here; -1 is nothing ready.
            if idx == -2 || idx == -3 {
                continue;
            }
            if idx < 0 {
                return;
            }
            // SAFETY: the buffer is valid until released, below.
            unsafe {
                let mut capacity = 0usize;
                let buffer = AMediaCodec_getOutputBuffer(self.codec, idx as usize, &mut capacity);
                if !buffer.is_null() && info.size > 0 {
                    let data = std::slice::from_raw_parts(
                        buffer.add(info.offset as usize),
                        info.size as usize,
                    );
                    if info.flags & BUFFER_FLAG_CODEC_CONFIG != 0 {
                        self.config = data.to_vec();
                    } else if info.flags & BUFFER_FLAG_KEY_FRAME != 0 {
                        sink(
                            info.pts_us as u64,
                            super::with_parameter_sets(&self.config, data),
                            true,
                        );
                    } else {
                        sink(info.pts_us as u64, data.to_vec(), false);
                    }
                }
                AMediaCodec_releaseOutputBuffer(self.codec, idx as usize, false);
            }
        }
    }

    pub(super) fn request_key_frame(&self) {
        // SAFETY: a fresh format, deleted right after the call.
        unsafe {
            let params = AMediaFormat_new();
            AMediaFormat_setInt32(params, key("request-sync").as_ptr(), 0);
            let status = AMediaCodec_setParameters(self.codec, params);
            AMediaFormat_delete(params);
            if status != AMEDIA_OK {
                log::warn!("encoder {}: request-sync refused ({status})", self.name);
            }
        }
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        // SAFETY: the codec is ours and deleted once.
        unsafe {
            AMediaCodec_stop(self.codec);
            AMediaCodec_delete(self.codec);
        }
    }
}

/// A hardware H.264 decoder handing back NV12 pictures, owned by one thread.
pub(crate) struct Decoder {
    codec: *mut Codec,
    /// The output layout, from the decoder's latest output format.
    layout: Option<Layout>,
}

#[derive(Clone, Copy)]
struct Layout {
    color: i32,
    stride: usize,
    slice_height: usize,
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

pub(crate) enum Decoded {
    Picture(u64, Nv12),
    /// Nothing came out within the wait.
    Pending,
    /// The end-of-stream input has come through.
    Ended,
}

impl Decoder {
    pub(crate) fn open(width: usize, height: usize) -> anyhow::Result<Decoder> {
        let mime = key("video/avc");
        // SAFETY: as in `Encoder::open`.
        unsafe {
            let codec = AMediaCodec_createDecoderByType(mime.as_ptr());
            if codec.is_null() {
                anyhow::bail!("no video/avc decoder");
            }
            let format = AMediaFormat_new();
            AMediaFormat_setString(format, key("mime").as_ptr(), mime.as_ptr());
            AMediaFormat_setInt32(format, key("width").as_ptr(), width as i32);
            AMediaFormat_setInt32(format, key("height").as_ptr(), height as i32);
            AMediaFormat_setInt32(format, key("color-format").as_ptr(), COLOR_FORMAT_NV12);
            let status =
                AMediaCodec_configure(codec, format, std::ptr::null_mut(), std::ptr::null_mut(), 0);
            AMediaFormat_delete(format);
            if status != AMEDIA_OK || AMediaCodec_start(codec) != AMEDIA_OK {
                AMediaCodec_delete(codec);
                anyhow::bail!("decoder configure or start {width}x{height} failed ({status})");
            }
            Ok(Decoder { codec, layout: None })
        }
    }

    /// `false` when no input buffer came free within [`INPUT_WAIT_US`].
    /// `end` marks the last access unit, after which [`Decoder::next`] runs
    /// out with [`Decoded::Ended`].
    pub(crate) fn submit(&mut self, unit: &[u8], pts_us: u64, end: bool) -> anyhow::Result<bool> {
        // SAFETY: `idx` is the codec's and goes back to it on every path.
        unsafe {
            let idx = AMediaCodec_dequeueInputBuffer(self.codec, INPUT_WAIT_US);
            if idx < 0 {
                return Ok(false);
            }
            let idx = idx as usize;
            let mut capacity = 0usize;
            let buffer = AMediaCodec_getInputBuffer(self.codec, idx, &mut capacity);
            let flags = if end { BUFFER_FLAG_END_OF_STREAM } else { 0 };
            if buffer.is_null() || capacity < unit.len() {
                AMediaCodec_queueInputBuffer(self.codec, idx, 0, 0, pts_us, flags);
                anyhow::bail!("input buffer holds {capacity} bytes, a frame needs {}", unit.len());
            }
            std::slice::from_raw_parts_mut(buffer, unit.len()).copy_from_slice(unit);
            let status =
                AMediaCodec_queueInputBuffer(self.codec, idx, 0, unit.len(), pts_us, flags);
            if status != AMEDIA_OK {
                anyhow::bail!("queueInputBuffer: {status}");
            }
        }
        Ok(true)
    }

    pub(crate) fn next(&mut self, wait_us: i64) -> anyhow::Result<Decoded> {
        loop {
            let mut info = BufferInfo::default();
            // SAFETY: an index >= 0 is an output buffer released below.
            let idx = unsafe { AMediaCodec_dequeueOutputBuffer(self.codec, &mut info, wait_us) };
            if idx == -2 {
                self.layout = Some(self.layout()?);
                continue;
            }
            if idx == -3 {
                continue;
            }
            if idx < 0 {
                return Ok(Decoded::Pending);
            }
            // SAFETY: the buffer is valid until released, below.
            let picture = unsafe {
                let mut capacity = 0usize;
                let buffer = AMediaCodec_getOutputBuffer(self.codec, idx as usize, &mut capacity);
                let picture = match self.layout {
                    Some(layout) if !buffer.is_null() && info.size > 0 => {
                        let data = std::slice::from_raw_parts(
                            buffer.add(info.offset as usize),
                            info.size as usize,
                        );
                        Some(tight(layout, data))
                    }
                    _ => None,
                };
                AMediaCodec_releaseOutputBuffer(self.codec, idx as usize, false);
                picture
            };
            if let Some(picture) = picture {
                return Ok(Decoded::Picture(info.pts_us as u64, picture?));
            }
            if info.flags & BUFFER_FLAG_END_OF_STREAM != 0 {
                return Ok(Decoded::Ended);
            }
        }
    }

    fn layout(&self) -> anyhow::Result<Layout> {
        // SAFETY: the format is the codec's copy, deleted here.
        unsafe {
            let format = AMediaCodec_getOutputFormat(self.codec);
            if format.is_null() {
                anyhow::bail!("decoder gave no output format");
            }
            let get = |k: &str| {
                let mut v = -1;
                AMediaFormat_getInt32(format, key(k).as_ptr(), &mut v).then_some(v)
            };
            let (width, height) = (get("width"), get("height"));
            let color = get("color-format").unwrap_or(-1);
            let stride = get("stride").or(width).unwrap_or(0) as usize;
            let slice_height = get("slice-height").or(height).unwrap_or(0) as usize;
            let left = get("crop-left").unwrap_or(0) as usize;
            let top = get("crop-top").unwrap_or(0) as usize;
            let right = get("crop-right").map(|r| r as usize + 1);
            let bottom = get("crop-bottom").map(|b| b as usize + 1);
            AMediaFormat_delete(format);
            let width = right.unwrap_or(width.unwrap_or(0) as usize) - left;
            let height = bottom.unwrap_or(height.unwrap_or(0) as usize) - top;
            let known = [
                COLOR_FORMAT_NV12,
                COLOR_FORMAT_FLEXIBLE,
                COLOR_FORMAT_QCOM_NV12,
                COLOR_FORMAT_I420,
            ];
            if !known.contains(&color) {
                anyhow::bail!("decoder output colour format {color:#x} is not one read here");
            }
            log::info!(
                "decoder output: {width}x{height} colour {color:#x}, \
                 stride {stride}, slice height {slice_height}"
            );
            Ok(Layout { color, stride, slice_height, left, top, width, height })
        }
    }
}

/// A decoded picture without its padding, as tight NV12.
fn tight(l: Layout, data: &[u8]) -> anyhow::Result<Nv12> {
    let (w, h) = (l.width & !1, l.height & !1);
    let luma = l.stride * l.slice_height;
    let (half, plane) = (l.stride / 2, (l.stride / 2) * (l.slice_height / 2));
    // One past the last byte read: the bottom chroma row of the picture.
    let last = l.top / 2 + h / 2 - 1;
    let end = match l.color {
        COLOR_FORMAT_I420 => luma + plane + last * half + l.left / 2 + w / 2,
        _ => luma + last * l.stride + l.left + w,
    };
    if w == 0 || h == 0 || data.len() < end {
        anyhow::bail!("decoded buffer holds {} bytes, its {w}x{h} picture needs {end}", data.len());
    }
    let mut out = vec![0u8; w * h * 3 / 2];
    let (y, uv) = out.split_at_mut(w * h);
    for row in 0..h {
        let at = (l.top + row) * l.stride + l.left;
        y[row * w..][..w].copy_from_slice(&data[at..at + w]);
    }
    for row in 0..h / 2 {
        let line = &mut uv[row * w..][..w];
        let r = l.top / 2 + row;
        match l.color {
            COLOR_FORMAT_I420 => {
                for col in 0..w / 2 {
                    let at = luma + r * half + l.left / 2 + col;
                    line[col * 2] = data[at];
                    line[col * 2 + 1] = data[at + plane];
                }
            }
            _ => {
                let at = luma + r * l.stride + l.left;
                line.copy_from_slice(&data[at..at + w]);
            }
        }
    }
    Ok(Nv12 { width: w, height: h, data: out })
}

impl Drop for Decoder {
    fn drop(&mut self) {
        // SAFETY: the codec is ours and deleted once.
        unsafe {
            AMediaCodec_stop(self.codec);
            AMediaCodec_delete(self.codec);
        }
    }
}
