# Startup sound

Generates `assets/sound/boot.wav`, the chime the app plays at launch. It was made for YUBI Go
and is covered by the repository's Apache-2.0 licence.

```bash
cd tools/sound
uv run --no-project --with numpy --with scipy python aero.py   # writes aero-*.wav here
uv run --no-project --with numpy --with scipy python -c "
from scipy.io import wavfile; import numpy as np
r, x = wavfile.read('aero-splash-B-quest.wav'); x = x.astype(np.float64)
n = int(0.15 * r); x[-n:] *= np.linspace(1, 0, n)
wavfile.write('../../assets/sound/boot.wav', r, np.round(x).astype(np.int16))"
```

The 150 ms fade-out is there because the render stops mid-reverb, which clicks. `logo.py` holds
the shared instruments; `aero.py` renders the variants, and `splash-B-quest` is the one used.
