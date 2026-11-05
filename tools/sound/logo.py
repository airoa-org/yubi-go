"""Instruments for the startup sound (`aero.py` uses them), and four sketches built on the
recording-start motif: 660 Hz -> 990 Hz (a rising fifth)."""
import numpy as np
from scipy import signal
from scipy.io import wavfile

R = 48_000
rng = np.random.default_rng(7)
E5, B5 = 660.0, 990.0  # the take-start cue, exactly


def t_(dur):
    return np.arange(int(dur * R)) / R


def semis(f, n):
    return f * 2 ** (n / 12)


def place(buf, x, at):
    i = int(at * R)
    end = min(len(buf), i + len(x))
    buf[i:end] += x[: end - i]


def env(dur, attack=0.004, decay=1.0):
    t = t_(dur)
    e = np.minimum(t / attack, 1) * np.exp(-t / decay)
    tail = int(0.02 * R)
    e[-tail:] *= np.linspace(1, 0, tail)
    return e


def fm(f, dur, ratio=1.0, index=3.0, idx_decay=0.08, decay=0.6, attack=0.003):
    """Two-operator FM; a fast-falling index gives the bright strike, then a clean tone."""
    t = t_(dur)
    i = index * np.exp(-t / idx_decay)
    return np.sin(2 * np.pi * f * t + i * np.sin(2 * np.pi * f * ratio * t)) * env(dur, attack, decay)


def bell(f, dur):
    """Warm and in tune: harmonic FM (ratio 1) with a soft strike, no high partials left ringing."""
    x = fm(f, dur, ratio=1.0, index=1.2, idx_decay=0.06, decay=0.5, attack=0.01)
    b, a = signal.butter(2, 2200 / (R / 2))
    return signal.lfilter(b, a, x)


def marimba(f, dur):
    return fm(f, dur, ratio=4.0, index=1.6, idx_decay=0.02, decay=0.25)


def pluck(f, dur):
    return fm(f, dur, ratio=1.0, index=4.0, idx_decay=0.05, decay=0.35)


def pad(freqs, dur, attack=0.25):
    t = t_(dur)
    x = np.zeros_like(t)
    for f in freqs:
        for d in (-0.08, 0.0, 0.08):  # semitone-cents detune for width
            ff = semis(f, d)
            x += signal.sawtooth(2 * np.pi * ff * t + rng.uniform(0, 6.28))
    b, a = signal.butter(2, 1600 / (R / 2))
    x = signal.lfilter(b, a, x) / (3 * len(freqs))
    e = np.minimum(t / attack, 1) * np.exp(-np.maximum(t - attack, 0) / (dur * 0.5))
    e[-int(0.1 * R):] *= np.linspace(1, 0, int(0.1 * R))
    return x * e


def riser(dur, f0=300, f1=6000):
    """Noise swept up through a band-pass, block by block."""
    n = int(dur * R)
    noise = rng.standard_normal(n)
    out = np.zeros(n)
    blk = 512
    zi = None
    for k in range(0, n, blk):
        frac = k / n
        fc = f0 * (f1 / f0) ** frac
        b, a = signal.butter(2, [fc * 0.8 / (R / 2), min(fc * 1.25, R / 2 - 1) / (R / 2)], "band")
        if zi is None:
            zi = signal.lfilter_zi(b, a) * 0
        seg, zi = signal.lfilter(b, a, noise[k:k + blk], zi=zi)
        out[k:k + blk] = seg
    return out * np.linspace(0, 1, n) ** 2


def reverb(x, size=0.8, mix=0.18):
    t = t_(size)
    ir = rng.standard_normal(len(t)) * np.exp(-t / (size / 5))
    b, a = signal.butter(2, 1800 / (R / 2))
    ir = signal.lfilter(b, a, ir)
    wet = signal.fftconvolve(x, ir)[: len(x)]
    wet *= np.max(np.abs(x)) / (np.max(np.abs(wet)) + 1e-9)
    return (1 - mix) * x + mix * wet


def delay(x, time, feedback=0.45, taps=4):
    out = x.copy()
    for k in range(1, taps + 1):
        d = int(time * k * R)
        out[d:] += x[: len(x) - d] * feedback ** k
    return out


def finish(name, x):
    x = x / np.max(np.abs(x)) * 0.89  # -1 dBFS
    wavfile.write(f"{name}.wav", R, (x * 32767).astype(np.int16))
    # What the Quest's small speakers keep: little below ~400 Hz.
    b, a = signal.butter(4, 400 / (R / 2), "high")
    q = signal.lfilter(b, a, x)
    q = q / np.max(np.abs(q)) * 0.89
    wavfile.write(f"{name}-quest.wav", R, (q * 32767).astype(np.int16))
    print(f"{name}: {len(x) / R:.2f} s")


# 1. Stacked fifths: the motif climbs itself (E-B, B-F#, E-B an octave up), then blooms.
def stacked():
    buf = np.zeros(int(2.0 * R))
    steps = [(E5 / 2, B5 / 2), (B5 / 2, semis(B5 / 2, 7)), (E5, B5)]
    at = 0.0
    for lo, hi in steps:
        place(buf, 0.6 * pluck(lo, 0.5), at)
        place(buf, 0.6 * pluck(hi, 0.5), at + 0.09)
        at += 0.2
    chord = [E5 / 2, semis(E5 / 2, 7), E5, semis(E5, 4), B5]  # E major, open
    for k, f in enumerate(chord):
        place(buf, 0.35 * bell(f, 1.3), at + 0.012 * k)
    place(buf, 0.5 * pad(chord, 1.4, attack=0.05), at)
    return reverb(buf, mix=0.3)


# 2. Ignition: a short riser, then the motif struck on bells over a chord.
def ignition():
    buf = np.zeros(int(2.0 * R))
    rise = 0.55
    place(buf, 0.25 * riser(rise), 0.0)
    place(buf, 0.8 * bell(E5, 1.0), rise)
    place(buf, 0.8 * bell(B5, 1.3), rise + 0.11)
    place(buf, 0.45 * pad([E5 / 2, semis(E5 / 2, 7), semis(E5, 4)], 1.4, attack=0.03), rise)
    return reverb(buf, mix=0.3)


# 3. Echo: the motif, exactly as the take cue, thrown into a tempo delay and answered by a fifth above.
def echo():
    buf = np.zeros(int(2.0 * R))
    beat = 0.16
    place(buf, 0.7 * marimba(E5, 0.4), 0.0)
    place(buf, 0.7 * marimba(B5, 0.4), beat * 0.6)
    buf = delay(buf, beat * 2, feedback=0.4, taps=3)
    tail = np.zeros_like(buf)
    land = beat * 4
    place(tail, 0.8 * bell(E5, 1.4), land)
    place(tail, 0.4 * pad([E5 / 2, E5, semis(E5, 4)], 1.4, attack=0.02), land)
    return reverb(buf + tail, mix=0.25)


# 4. Robot hello: the fifth as a glide, vocoder-ish formant sweep, landing on a bright pluck chord.
def glide():
    buf = np.zeros(int(2.0 * R))
    dur = 0.45
    t = t_(dur)
    f = E5 + (B5 - E5) / (1 + np.exp(-(t - dur * 0.55) * 40))  # step-like portamento E -> B
    ph = 2 * np.pi * np.cumsum(f) / R
    x = signal.square(ph, duty=0.3) * env(dur, 0.01, 0.6)
    b, a = signal.butter(2, [800 / (R / 2), 3200 / (R / 2)], "band")
    place(buf, 0.5 * signal.lfilter(b, a, x), 0.0)
    for k, ff in enumerate([E5 / 2, E5, semis(E5, 4), B5]):
        place(buf, 0.4 * pluck(ff, 1.4), dur + 0.02 + 0.03 * k)
    return reverb(buf, mix=0.3)


if __name__ == "__main__":
  for name, make in [("1-stacked", stacked), ("2-ignition", ignition), ("3-echo", echo), ("4-glide", glide)]:
    finish(name, make())
