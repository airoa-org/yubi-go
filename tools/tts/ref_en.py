# /// script
# requires-python = ">=3.10,<3.13"
# dependencies = [
#   "kokoro==0.9.4",
#   "misaki[en]==0.9.4",
#   "soundfile==0.14.0",
#   "numpy",
#   "onnxruntime",
#   "huggingface_hub",
#   "en-core-web-sm @ https://github.com/explosion/spacy-models/releases/download/en_core_web_sm-3.8.0/en_core_web_sm-3.8.0-py3-none-any.whl",
# ]
# ///
"""Writes tts/tests/misaki_en.tsv: the phonemes Kokoro's English pipeline
(misaki 0.9.4 with spaCy en_core_web_sm and the espeak-ng fallback), which
generate.py feeds af_heart, makes of every English phrase and of some task
names.

    uv run tools/tts/ref_en.py           # the test table
    uv run tools/tts/ref_en.py tags      # plus spaCy's tags, to stderr
"""
import csv, os, sys

# The espeak-ng data bundled with espeakng_loader has a broken path on macOS;
# Homebrew's espeak-ng works (as in tools/voice/generate.py).
BREW = "/opt/homebrew/opt/espeak-ng"
if os.path.isdir(BREW):
    import espeakng_loader
    espeakng_loader.get_library_path = lambda: f"{BREW}/lib/libespeak-ng.dylib"
    espeakng_loader.get_data_path = lambda: f"{BREW}/share/espeak-ng-data"

# As generate.py does: en_core_web_sm's own tokenizer crashes on load under
# spacy 3.8, so spaCy falls back to the English defaults.
import spacy

_load = spacy.load
spacy.load = lambda name, **kw: _load(name, exclude=["tokenizer"], **kw)

from kokoro import KPipeline

REPO = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..")
EXTRA = [
    "Wipe the table", "Fold the towel in half", "Put 3 screws in the box",
    "Open the drawer and take out the pen", "Plug the USB cable in", "Close the drawer",
    "Pick up the cup", "Place the cup on the shelf", "Pour water into the glass",
    "Hang the shirt on the hanger", "Wash the plate and dry it", "Peel the banana",
    "Open the can", "Unplug the charger from the outlet", "Turn on the TV with the remote",
    "Stack the red block on the blue block", "Put the violin back in its case",
    "Take one tissue", "Lay out the fork and knife", "Wipe it with a paper towel",
    "Heat it in the microwave", "Take the milk out of the fridge", "Sort the trash",
    "Fold the laundry and put it away", "Return the books to the shelf (left to right)",
    "Say \"done\"", "Cut the tomato in half", "Press and release the button",
    "Hold the mug by the handle", "Wait a moment", "Slice the cucumber thinly",
    "Scoop the rice with the paddle", "A 500 ml bottle", "October 27, 2026", "A 3.5 kg box",
    "Count from 0 to 9", "Item 12345678", "Step 1", "Insert the USB cable",
    "Put on the Quest 3", "Swap A and B", "Grip it and let go", "Turn it with a screwdriver",
    "The drawer at the very back", "Pull the cable out 10 cm", "Tighten the 2nd screw",
    "Record a video of the 1st try", "Put the lid on", "Set the timer to 5 minutes",
    "Use the left gripper", "Don't drop the egg", "It's the robot's turn",
    "Move the box to the left-hand side", "Re-tie the shoelaces",
    "Load the dishwasher", "Water the plants", "Close the window, then lock it",
    "Read the label", "Lead the cable around the chair", "Wind the cable neatly",
    "Present the object to the camera", "Put the iPhone in the drawer",
    "Pay $5.50", "Put 1,000 screws in the box", "The 3rd shelf from the top", "Cut 2.5 cm",
    "Songs from the 1990s", "Move it 0.5 m", "Take 105 steps", "I read the book",
    "Turn on the light at 7:30", "Wipe the table 2 times", "Use 3 of the 12 cups",
    "Place 20 blocks in 4 rows", "Wait 30 seconds, then open it", "Lift the box 15 cm up",
]
pipe = KPipeline(lang_code="a", model=False)
rows = [r["en"] for r in csv.DictReader(open(f"{REPO}/assets/voice/phrases.tsv"), delimiter="\t")]
with open(f"{REPO}/tts/tests/misaki_en.tsv", "w") as f:
    for text in rows + EXTRA:
        ps = " ".join(r.phonemes for r in pipe(text, voice="af_heart"))
        f.write(f"{text}\t{ps}\n")
        if "tags" in sys.argv:
            print(text, "|", " ".join(f"{t.text}/{t.tag}" for t in pipe.g2p(text)[1]), file=sys.stderr)
print("phonemes written", len(rows) + len(EXTRA))
