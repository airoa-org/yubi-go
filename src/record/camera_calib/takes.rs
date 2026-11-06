//! What a calibration reads from a take: its frame times.

/// A frame's time comes from the camera's own clock when its stamps follow
/// that clock to within this, as they did for every frame of 2026-10-28..30: USB
/// arrival scatters the stamps ±2 ms around it.
const PTS_GATE_NS: f64 = 4e6;

/// Each frame's time on the take's origin, keyed by its stamp: when its first
/// packet arrived, regressed on the camera's clock, which drops the ±2 ms
/// arrival jitter. The first packet, because a take from a pre-release build
/// is stamped at its last one, 8 ms later for a wrist and 16 ms for the head
/// (2026-10-28 against 2026-10-31), and both must give one delay. Empty when the
/// arrivals do not follow the clock, as after the camera reopened mid-take;
/// the stamps are used then.
pub(crate) fn frame_times(frames: &[(u64, u32, u64)], origin: u64) -> Vec<(u64, f64)> {
    let n = frames.len();
    if n < 10 {
        return Vec::new();
    }
    let mut x = vec![0.0; n];
    for i in 1..n {
        x[i] = x[i - 1] + frames[i].1.wrapping_sub(frames[i - 1].1) as f64;
    }
    let y: Vec<f64> = frames.iter().map(|f| f.2.wrapping_sub(frames[0].2) as i64 as f64).collect();
    let mut keep = vec![true; n];
    let (mut a, mut b) = (0.0, 0.0);
    for _ in 0..3 {
        let (mut s, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for i in (0..n).filter(|&i| keep[i]) {
            s += 1.0;
            sx += x[i];
            sy += y[i];
            sxx += x[i] * x[i];
            sxy += x[i] * y[i];
        }
        let det = s * sxx - sx * sx;
        if det.abs() < 1e-9 {
            return Vec::new();
        }
        b = (s * sxy - sx * sy) / det;
        a = (sy - b * sx) / s;
        let r: Vec<f64> = (0..n).map(|i| y[i] - a - b * x[i]).collect();
        let mid = median((0..n).filter(|&i| keep[i]).map(|i| r[i]).collect());
        keep = r.iter().map(|v| (v - mid).abs() < PTS_GATE_NS).collect();
    }
    if keep.iter().filter(|k| **k).count() * 10 < n * 9 {
        return Vec::new();
    }
    let base = frames[0].2.wrapping_sub(origin) as i64 as f64;
    frames.iter().zip(&x).map(|(f, &xi)| (f.0, base + a + b * xi)).collect()
}

fn median(mut v: Vec<f64>) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}
