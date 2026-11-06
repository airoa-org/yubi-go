"""Frutiger-Aero takes on the ignition gesture: glossy, airy, watery; the rising fifth kept."""
from logo import *


def lowpass(x, hz, order=2):
    b, a = signal.butter(order, hz / (R / 2))
    return signal.lfilter(b, a, x)


def chorus(make, f, dur, cents=(-6, 0, 6)):
    return sum(make(semis(f, c / 100), dur) for c in cents) / len(cents)


def glass(f, dur):
    """Soft glassy mallet: harmonic FM with a small strike, chorused, top rolled off."""
    one = lambda ff, d: fm(ff, d, ratio=1.0, index=0.9, idx_decay=0.04, decay=0.55, attack=0.006)
    return lowpass(chorus(one, f, dur), 2600)


def bubble(f_end, dur=0.14):
    """A water 'bloop': a sine sweeping up an octave and a bit, gone in a moment."""
    t = t_(dur)
    f = f_end / 2.3 * 2.3 ** (t / dur) ** 0.6
    x = np.sin(2 * np.pi * np.cumsum(f) / R)
    return x * env(dur, attack=0.004, decay=dur / 2.5)


def airpad(notes, dur, attack=0.35):
    """Breathy chorused sines with a slow shimmer, no saw edge."""
    t = t_(dur)
    x = np.zeros_like(t)
    for f in notes:
        for c in (-7, 0, 7):
            ff = semis(f, c / 100) * (1 + 0.002 * np.sin(2 * np.pi * 0.4 * t + rng.uniform(0, 6.3)))
            ph = 2 * np.pi * np.cumsum(ff) / R
            x += np.sin(ph) + 0.15 * np.sin(2 * ph)
    x /= 3 * len(notes)
    e = np.minimum(t / attack, 1) * np.exp(-np.maximum(t - attack, 0) / (dur * 0.45))
    e[-int(0.15 * R):] *= np.linspace(1, 0, int(0.15 * R))
    return lowpass(x * e, 2200)


def wind(dur, lo=700, hi=2600):
    n = int(dur * R)
    b, a = signal.butter(2, [lo / (R / 2), hi / (R / 2)], "band")
    return signal.lfilter(b, a, rng.standard_normal(n)) * np.sin(np.linspace(0, np.pi, n)) ** 2


def maj9(root):
    """Open maj9 voiced from the octave below, so nothing rings above ~1 kHz."""
    b = root / 2
    return [b, semis(b, 7), semis(b, 11), semis(b, 14), semis(b, 16)]


def aero_reverb(x):
    return reverb(x, size=1.6, mix=0.32)


def bubbles(root):
    buf = np.zeros(int(2.4 * R))
    for k, at in enumerate((0.0, 0.08, 0.15)):
        place(buf, 0.35 * bubble(semis(root, -5 + 4 * k)), at)
    s = 0.3
    place(buf, 0.75 * glass(root, 1.0), s)
    place(buf, 0.75 * glass(semis(root, 7), 1.4), s + 0.11)
    place(buf, 0.5 * airpad(maj9(root), 1.9, attack=0.12), s)
    return aero_reverb(buf)


def sky(root):
    buf = np.zeros(int(2.6 * R))
    place(buf, 0.2 * wind(1.1), 0.0)
    place(buf, 0.55 * airpad(maj9(root), 2.4, attack=0.6), 0.0)
    s = 0.55
    place(buf, 0.7 * glass(root, 1.0), s)
    place(buf, 0.7 * glass(semis(root, 7), 1.5), s + 0.13)
    return aero_reverb(buf)


def glassy(root):
    """The fifth grows into a rolled maj9 arpeggio on glass, over a pad."""
    buf = np.zeros(int(2.4 * R))
    place(buf, 0.18 * riser(0.3, f0=500, f1=2500), 0.0)
    s = 0.3
    for k, n in enumerate((0, 7, 11, 14)):
        place(buf, 0.6 * glass(semis(root, n - 12 if n > 7 else n), 1.3), s + 0.07 * k)
    place(buf, 0.45 * airpad(maj9(root), 1.8, attack=0.08), s)
    return aero_reverb(buf)


def splash(root):
    """One droplet, then the chord blooms in reversed into the strike."""
    buf = np.zeros(int(2.4 * R))
    place(buf, 0.4 * bubble(semis(root, 7), 0.12), 0.0)
    rise = 0.45
    swell = sum(glass(f, rise) for f in maj9(root))[::-1] * 0.22
    place(buf, swell, 0.1)
    s = 0.1 + rise
    place(buf, 0.75 * glass(root, 1.0), s)
    place(buf, 0.75 * glass(semis(root, 7), 1.4), s + 0.1)
    place(buf, 0.4 * airpad(maj9(root), 1.6, attack=0.05), s)
    return aero_reverb(buf)


def bloom(root):
    """The pad bends up a fourth into place while the air swells, then the glass fifth."""
    buf = np.zeros(int(2.4 * R))
    dur = 1.9
    t = t_(dur)
    bend = 2 ** ((-5 / 12) * np.exp(-t / 0.12))
    x = np.zeros_like(t)
    for f in maj9(root):
        for c in (-7, 7):
            x += np.sin(2 * np.pi * np.cumsum(semis(f, c / 100) * bend) / R)
    e = np.minimum(t / 0.25, 1) * np.exp(-t / 0.9)
    e[-int(0.15 * R):] *= np.linspace(1, 0, int(0.15 * R))
    place(buf, 0.5 * lowpass(x * e / 10, 2000), 0.0)
    place(buf, 0.15 * wind(0.5), 0.0)
    s = 0.3
    place(buf, 0.75 * glass(root, 1.0), s)
    place(buf, 0.75 * glass(semis(root, 7), 1.4), s + 0.11)
    return aero_reverb(buf)


if __name__ == "__main__":
  for key, root in (("E", E5), ("B", semis(E5, -5))):
    for name, make in (("bubbles", bubbles), ("sky", sky), ("glass", glassy), ("splash", splash), ("bloom", bloom)):
        finish(f"aero-{name}-{key}", make(root))
