//! Speech on the headset in the voice the embedded clips use: Kokoro-82M
//! with `af_heart`.
//!
//! `en` turns text into phonemes. It ports misaki 0.9.4's English G2P,
//! including spaCy's tokenizer rules, so neither spaCy nor espeak-ng (GPL)
//! runs. It matches misaki on 183 of 185 phrases (`tests/en.rs`), and the
//! two misses are heteronyms. `kokoro` runs the ONNX export through ONNX
//! Runtime, loaded with `dlopen`. A word outside the lexicon keeps only its
//! ASCII letters, so a name typed only in Japanese is not spoken.

pub mod en;
pub mod kokoro;
mod tables;

use std::path::Path;

/// Changes whenever the same text would come out differently, so audio
/// cached under an older one is made again.
pub const VERSION: &str = "kokoro-v1.0-q8 misaki-0.9.4 1";

/// What `tools/tts/fetch.sh` lays out, relative to the speech directory.
pub const MODEL: &str = "model.onnx";
const VOICE: &str = "af_heart";

pub struct Speaker {
    en: en::En,
    kokoro: kokoro::Kokoro,
}

impl Speaker {
    pub fn load(dir: &Path, runtime: &Path, threads: usize) -> Result<Speaker, String> {
        let voice = dir.join(format!("{VOICE}.bin"));
        Ok(Speaker {
            en: en::En::new()?,
            kokoro: kokoro::Kokoro::load(runtime, &dir.join(MODEL), &[&voice], threads)?,
        })
    }

    /// 24 kHz mono, shaped like the embedded clips.
    pub fn speak(&mut self, text: &str) -> Result<Vec<f32>, String> {
        self.kokoro.speak(&self.en.phonemes(text), VOICE)
    }
}
