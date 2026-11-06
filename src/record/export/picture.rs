//! Every camera's picture fitted to the size the dataset's videos share, and
//! the colour stats LeRobot keeps per image feature.

use crate::record::video::Nv12;

/// The wrist cameras' own size, and the size of `observation.image.*` in
/// AIRoA's own UMI training datasets.
pub const WIDTH: usize = 640;
pub const HEIGHT: usize = 480;

/// The head module's frame is a side-by-side stereo pair: its left eye is
/// the left half. A frame this much wider than tall is that pair.
fn left_eye(width: usize, height: usize) -> usize {
    if width >= 2 * height {
        width / 2
    } else {
        width
    }
}

/// The left eye of a stereo pair, centre-cropped to 4:3 and area-averaged
/// down to [`WIDTH`]×[`HEIGHT`]. A frame already that size is returned as is.
pub fn fit(frame: Nv12) -> Nv12 {
    if frame.width == WIDTH && frame.height == HEIGHT {
        return frame;
    }
    let eye = left_eye(frame.width, frame.height);
    // The widest 4:3 window, on even pixels so the chroma grid lines up.
    let (crop_w, crop_h) = if eye * HEIGHT >= frame.height * WIDTH {
        (frame.height * WIDTH / HEIGHT, frame.height)
    } else {
        (eye, eye * HEIGHT / WIDTH)
    };
    let (crop_w, crop_h) = (crop_w & !1, crop_h & !1);
    let x0 = ((eye - crop_w) / 2) & !1;
    let y0 = ((frame.height - crop_h) / 2) & !1;

    let mut data = vec![0u8; WIDTH * HEIGHT * 3 / 2];
    let (y_out, uv_out) = data.split_at_mut(WIDTH * HEIGHT);
    let (y_in, uv_in) = frame.data.split_at(frame.width * frame.height);
    shrink(y_in, frame.width, (x0, y0, crop_w, crop_h), y_out, WIDTH, HEIGHT, 1);
    shrink(
        uv_in,
        frame.width / 2,
        (x0 / 2, y0 / 2, crop_w / 2, crop_h / 2),
        uv_out,
        WIDTH / 2,
        HEIGHT / 2,
        2,
    );
    Nv12 { width: WIDTH, height: HEIGHT, data }
}

/// Each output sample the mean of the input samples its area covers. `step`
/// is 2 for the interleaved chroma plane, whose rows hold Cb,Cr pairs.
fn shrink(
    src: &[u8],
    src_w: usize,
    (x0, y0, w, h): (usize, usize, usize, usize),
    dst: &mut [u8],
    dst_w: usize,
    dst_h: usize,
    step: usize,
) {
    let span = |i: usize, n: usize, from: usize| {
        (i * from / n, ((i + 1) * from / n).max(i * from / n + 1))
    };
    let columns: Vec<(usize, usize)> = (0..dst_w).map(|i| span(i, dst_w, w)).collect();
    // The column sums of one output row's input rows, so the inner loop runs
    // along contiguous bytes.
    let mut sums = vec![0u32; w * step];
    for oy in 0..dst_h {
        let (ya, yb) = span(oy, dst_h, h);
        sums.fill(0);
        for y in ya..yb {
            let row = &src[((y0 + y) * src_w + x0) * step..][..w * step];
            for (sum, &v) in sums.iter_mut().zip(row) {
                *sum += v as u32;
            }
        }
        let out = &mut dst[oy * dst_w * step..][..dst_w * step];
        for (ox, &(xa, xb)) in columns.iter().enumerate() {
            let n = ((yb - ya) * (xb - xa)) as u32;
            for c in 0..step {
                let sum: u32 = (xa..xb).map(|x| sums[x * step + c]).sum();
                out[ox * step + c] = ((sum + n / 2) / n) as u8;
            }
        }
    }
}

/// LeRobot's own image stats read every 4th row and column of a 640x480
/// frame (`compute_stats.py`, `auto_downsample_height_width`: the larger side
/// over 150, whole), so reading every pixel only costs time: 4 ms a frame on
/// the headset.
const STATS_STEP: usize = 4;

/// Adds one frame to `rgb`, converting as a full-range BT.601 decoder would:
/// the encoder is told its input is full range (`video/media_codec.rs`).
pub fn add(rgb: &mut super::table::Rgb, frame: &Nv12) {
    if rgb.frames == 0 {
        rgb.min = [f64::INFINITY; 3];
        rgb.max = [f64::NEG_INFINITY; 3];
    }
    let (w, h) = (frame.width, frame.height);
    let (y, uv) = frame.data.split_at(w * h);
    let mut sum = [0f64; 3];
    let mut squares = [0f64; 3];
    let mut lo = [255u8; 3];
    let mut hi = [0u8; 3];
    let mut samples = 0u64;
    for row in (0..h).step_by(STATS_STEP) {
        for col in (0..w).step_by(STATS_STEP) {
            samples += 1;
            let luma = y[row * w + col] as f32;
            let at = (row / 2) * w + (col / 2) * 2;
            let (cb, cr) = (uv[at] as f32 - 128.0, uv[at + 1] as f32 - 128.0);
            let px = [luma + 1.402 * cr, luma - 0.344_136 * cb - 0.714_136 * cr, luma + 1.772 * cb];
            for c in 0..3 {
                let v = px[c].round().clamp(0.0, 255.0) as u8;
                lo[c] = lo[c].min(v);
                hi[c] = hi[c].max(v);
                sum[c] += v as f64;
                squares[c] += (v as f64) * (v as f64);
            }
        }
    }
    for c in 0..3 {
        rgb.sum[c] += sum[c] / 255.0;
        rgb.squares[c] += squares[c] / (255.0 * 255.0);
        rgb.min[c] = rgb.min[c].min(lo[c] as f64 / 255.0);
        rgb.max[c] = rgb.max[c].max(hi[c] as f64 / 255.0);
    }
    rgb.pixels += samples;
    rgb.frames += 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(width: usize, height: usize, left: u8, right: u8) -> Nv12 {
        let mut data = vec![128u8; width * height * 3 / 2];
        for row in 0..height {
            for col in 0..width {
                data[row * width + col] = if col < width / 2 { left } else { right };
            }
        }
        Nv12 { width, height, data }
    }

    #[test]
    fn a_wrist_frame_passes_through() {
        let frame = flat(WIDTH, HEIGHT, 10, 20);
        let out = fit(Nv12 { width: frame.width, height: frame.height, data: frame.data.clone() });
        assert_eq!(out.data, frame.data);
    }

    #[test]
    fn the_stereo_head_frame_becomes_its_left_eye() {
        let out = fit(flat(3840, 1080, 50, 200));
        assert_eq!((out.width, out.height), (WIDTH, HEIGHT));
        let (y, uv) = out.data.split_at(WIDTH * HEIGHT);
        assert!(y.iter().all(|&v| v == 50), "nothing of the right eye");
        assert!(uv.iter().all(|&v| v == 128));
    }

    #[test]
    fn a_wide_frame_loses_its_sides_and_keeps_its_middle() {
        // 1440x1080 is 4:3 inside 1920x1080: 240 px off each side.
        let mut frame = flat(1920, 1080, 0, 0);
        for row in 0..1080 {
            for col in 240..1680 {
                frame.data[row * 1920 + col] = 99;
            }
        }
        let out = fit(frame);
        assert!(out.data[..WIDTH * HEIGHT].iter().all(|&v| v == 99));
    }

    #[test]
    fn grey_is_grey_in_rgb() {
        let mut rgb = super::super::table::Rgb::default();
        add(&mut rgb, &flat(8, 8, 255, 0));
        assert_eq!(rgb.min, [0.0; 3]);
        assert_eq!(rgb.max, [1.0; 3]);
        assert_eq!(rgb.sum, [2.0; 3]);
        assert_eq!((rgb.pixels, rgb.frames), (4, 1));
    }
}
