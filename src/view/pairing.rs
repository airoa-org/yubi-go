//! Matches the wrist cameras to the grippers without asking anyone. A wrist
//! camera's picture changes when its own gripper turns, so each camera follows
//! the spin of its own controller more than the other one's. The head camera
//! is the one whose picture size differs from the two wrists'.

use std::collections::VecDeque;

/// Every tick since the last reset counts, up to 60 s at the panel's 10 Hz.
/// Pooled from the start of four takes from 2026-10-25, the score never went
/// below 0 and passed 0.7 within 3.5 to 7 s of a hand moving.
const HISTORY: usize = 600;
/// The score that decides, either way.
const DECIDE: f32 = 0.7;
/// Ticks with a fresh picture from both wrists and a pose from both hands.
const MIN_SAMPLES: usize = 30;
/// Spin spread (rad/s) a hand needs to count, because a hand at rest
/// correlates with noise. The moving hands in those takes ran 0.24 to 1.24.
const MIN_SPIN: f32 = 0.2;
/// The score holds past [`DECIDE`] this many ticks (1 s) first.
const HOLD: u32 = 10;
/// Ticks of one hand turning alone before a [`Wave`] names a camera: 1 s, as [`HOLD`].
const WAVE: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Verdict {
    /// Each wrist slot shows its own gripper.
    Matched,
    /// The left slot shows the right gripper, and the other way round.
    Crossed,
}

/// Picture change and controller spin per tick, left then right.
#[derive(Default)]
pub struct Wrists {
    samples: VecDeque<[f32; 4]>,
    held: (Option<Verdict>, u32),
}

impl Wrists {
    /// One tick: how much each wrist slot's picture changed, and how fast each
    /// controller turned. The verdict comes once it has held for [`HOLD`] ticks.
    pub fn tick(&mut self, change: [Option<f32>; 2], spin: [Option<f32>; 2]) -> Option<Verdict> {
        let sample = [change[0], change[1], spin[0], spin[1]].map(|v| v.unwrap_or(f32::NAN));
        if self.samples.len() == HISTORY {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        let now = match score(&self.samples) {
            Some(s) if s >= DECIDE => Some(Verdict::Matched),
            Some(s) if s <= -DECIDE => Some(Verdict::Crossed),
            _ => None,
        };
        self.held = match (now, self.held) {
            (Some(v), (Some(was), n)) if v == was => (now, n + 1),
            (Some(_), _) => (now, 1),
            (None, _) => (None, 0),
        };
        now.filter(|_| self.held.1 >= HOLD)
    }
}

/// For each hand that moved: how much more its own slot's picture follows it
/// than the other slot's. Positive is matched, negative crossed.
fn score(samples: &VecDeque<[f32; 4]>) -> Option<f32> {
    let full: Vec<[f32; 4]> =
        samples.iter().copied().filter(|s| s.iter().all(|v| v.is_finite())).collect();
    if full.len() < MIN_SAMPLES {
        return None;
    }
    let col = |i: usize| -> Vec<f32> { full.iter().map(|s| s[i]).collect() };
    let pictures = [col(0), col(1)];
    let c = |a: &[f32], b: &[f32]| corr(a, b).unwrap_or(0.0);
    let moved: Vec<f32> = (0..2)
        .map(|hand| (hand, col(2 + hand)))
        .filter(|(_, spin)| spread(spin) >= MIN_SPIN)
        .map(|(hand, spin)| c(&pictures[hand], &spin) - c(&pictures[1 - hand], &spin))
        .collect();
    (!moved.is_empty()).then(|| moved.iter().sum())
}

/// After a match, which wrist slot's picture moves more while one hand turns
/// alone. It reads fresh pictures, so a wrong match names the other side.
#[derive(Default)]
pub struct Wave {
    hand: Option<usize>,
    change: [f32; 2],
    ticks: usize,
}

impl Wave {
    /// One tick, as [`Wrists::tick`]. Ticks with both hands still are skipped,
    /// because a turning hand slows at each reversal. Both hands turning starts over.
    pub fn tick(&mut self, change: [Option<f32>; 2], spin: [Option<f32>; 2]) -> Option<usize> {
        let alone = match spin.map(|s| s.is_some_and(|s| s >= MIN_SPIN)) {
            [true, false] => 0,
            [false, true] => 1,
            [true, true] => {
                *self = Wave::default();
                return None;
            }
            [false, false] => return None,
        };
        if self.hand != Some(alone) {
            *self = Wave { hand: Some(alone), ..Wave::default() };
        }
        let [Some(left), Some(right)] = change else { return None };
        self.change[0] += left;
        self.change[1] += right;
        self.ticks += 1;
        (self.ticks >= WAVE).then(|| usize::from(self.change[1] > self.change[0]))
    }
}

fn spread(v: &[f32]) -> f32 {
    let mean = v.iter().sum::<f32>() / v.len() as f32;
    (v.iter().map(|x| (x - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt()
}

fn corr(a: &[f32], b: &[f32]) -> Option<f32> {
    let (sa, sb) = (spread(a), spread(b));
    if sa == 0.0 || sb == 0.0 {
        return None;
    }
    let (ma, mb) = (a.iter().sum::<f32>() / a.len() as f32, b.iter().sum::<f32>() / b.len() as f32);
    let cov = a.iter().zip(b).map(|(x, y)| (x - ma) * (y - mb)).sum::<f32>() / a.len() as f32;
    Some(cov / (sa * sb))
}

/// Every 16th pixel's luma of a panel thumbnail, enough to see a picture move.
pub fn grey(rgb: &[u8]) -> Vec<u8> {
    rgb.as_chunks::<3>()
        .0
        .iter()
        .step_by(16)
        .map(|p| (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32) as u8)
        .collect()
}

/// Mean luma change between two `grey` pictures of one camera.
pub fn change(a: &[u8], b: &[u8]) -> Option<f32> {
    (a.len() == b.len() && !a.is_empty())
        .then(|| a.iter().zip(b).map(|(x, y)| x.abs_diff(*y) as f32).sum::<f32>() / a.len() as f32)
}

/// How fast a pose turned (rad/s) between two `[x, y, z, qx, qy, qz, qw]`.
pub fn spin(a: [f64; 7], b: [f64; 7], secs: f64) -> Option<f32> {
    let dot: f64 = (3..7).map(|i| a[i] * b[i]).sum();
    (secs > 0.0 && dot.is_finite()).then(|| (2.0 * dot.abs().min(1.0).acos() / secs) as f32)
}

/// The rank of the camera whose picture size differs from the other two, or
/// `None` while any size is unknown or no single one stands out.
pub fn head_rank(sizes: [Option<(u32, u32)>; 3]) -> Option<usize> {
    let s = sizes.map(|s| s.filter(|&(w, h)| w > 0 && h > 0));
    let [a, b, c] = [s[0]?, s[1]?, s[2]?];
    match (a == b, b == c, a == c) {
        (true, false, false) => Some(2),
        (false, true, false) => Some(0),
        (false, false, true) => Some(1),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each hand turns in its own bursts, as when the operator handles things.
    fn hands(tick: usize) -> [f32; 2] {
        let wave = |period: usize| ((tick % period) as f32 / period as f32 * 6.3).sin().max(0.0);
        [wave(23), wave(37)]
    }

    fn run(cross: bool, still: Option<usize>) -> Option<(usize, Verdict)> {
        let mut w = Wrists::default();
        (0..300).find_map(|t| {
            let mut spin = hands(t);
            if let Some(hand) = still {
                spin[hand] = 0.0;
            }
            let [l, r] = spin.map(|s| s * 5.0 + 1.0);
            let change = if cross { [r, l] } else { [l, r] };
            w.tick(change.map(Some), spin.map(Some)).map(|v| (t, v))
        })
    }

    #[test]
    fn both_hands_moving_decide_as_soon_as_enough_is_seen() {
        // The first score comes on the tick that fills MIN_SAMPLES and counts as one.
        let soon = MIN_SAMPLES + HOLD as usize - 2;
        assert_eq!(run(false, None), Some((soon, Verdict::Matched)));
        assert_eq!(run(true, None), Some((soon, Verdict::Crossed)));
    }

    #[test]
    fn one_hand_moving_is_enough() {
        assert_eq!(run(false, Some(0)).map(|(_, v)| v), Some(Verdict::Matched));
        assert_eq!(run(true, Some(1)).map(|(_, v)| v), Some(Verdict::Crossed));
    }

    #[test]
    fn hands_at_rest_decide_nothing() {
        let mut w = Wrists::default();
        let found = (0..300).find_map(|t| {
            let noise = (t * 7919 % 13) as f32 / 13.0;
            w.tick(
                [Some(noise), Some(1.0 - noise)],
                [Some(0.01 * (1.0 - noise)), Some(0.01 * noise)],
            )
        });
        assert_eq!(found, None, "the spin is too small to judge");
    }

    #[test]
    fn missing_samples_decide_nothing() {
        let mut w = Wrists::default();
        assert_eq!((0..300).find_map(|_| w.tick([None, Some(1.0)], [Some(1.0), Some(1.0)])), None);
    }

    #[test]
    fn a_wave_names_the_slot_whose_picture_moves() {
        let named = |change: [f32; 2], spin: [f32; 2]| {
            let mut w = Wave::default();
            (0..30).find_map(|_| w.tick(change.map(Some), spin.map(Some)))
        };
        assert_eq!(named([5.0, 1.0], [1.0, 0.0]), Some(0));
        assert_eq!(named([1.0, 5.0], [1.0, 0.0]), Some(1), "a crossed match names the other side");
        assert_eq!(named([5.0, 1.0], [1.0, 1.0]), None, "both hands turning");
        assert_eq!(named([5.0, 1.0], [0.0, 0.0]), None, "hands at rest");
    }

    #[test]
    fn a_pause_in_the_wave_keeps_what_it_saw() {
        let mut w = Wave::default();
        let ticks = (0..2 * WAVE).map(|t| [1.0, 0.0f32].map(|s| s * (t % 2) as f32));
        let named: Vec<_> =
            ticks.map(|spin| w.tick([Some(5.0), Some(1.0)], spin.map(Some))).collect();
        assert_eq!(named.iter().flatten().next(), Some(&0));
    }

    #[test]
    fn change_is_the_mean_luma_step_between_pictures() {
        let (black, white) = (grey(&[0; 48 * 3]), grey(&[255; 48 * 3]));
        assert_eq!(black.len(), 3, "every 16th pixel");
        assert_eq!(change(&black, &white), Some(255.0));
        assert_eq!(change(&white, &white), Some(0.0));
        assert_eq!(change(&white, &white[..2]), None, "pictures of different sizes");
    }

    #[test]
    fn spin_is_the_angle_turned_per_second() {
        let half = std::f64::consts::FRAC_PI_4;
        let a = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let b = [0.0, 0.0, 0.0, 0.0, 0.0, half.sin(), half.cos()];
        let s = spin(a, b, 0.5).unwrap();
        assert!((s - std::f32::consts::PI).abs() < 1e-4, "90° in 0.5 s, got {s}");
    }

    #[test]
    fn the_head_is_the_odd_size_out() {
        let (wrist, stereo) = (Some((640, 480)), Some((1280, 480)));
        assert_eq!(head_rank([wrist, stereo, wrist]), Some(1));
        assert_eq!(head_rank([stereo, wrist, wrist]), Some(0));
        assert_eq!(head_rank([wrist, wrist, wrist]), None, "three alike");
        assert_eq!(head_rank([wrist, stereo, None]), None, "one unknown");
        assert_eq!(head_rank([wrist, stereo, Some((0, 0))]), None, "not open yet");
    }
}
