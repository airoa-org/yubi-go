//! Kokoro-82M through ONNX Runtime: phonemes in, 24 kHz mono out, shaped the
//! way `tools/voice/generate.py` shapes the embedded clips.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use ort::environment::Environment;
use ort::session::Session;
use ort::value::Tensor;

pub const RATE: u32 = 24_000;
/// A voice is 510 style rows of 256, one per phrase length in phonemes.
const STYLE: usize = 256;
const ROWS: usize = 510;
// generate.py's shaping, so a spoken name sits level with the clips around it.
const PEAK: f32 = 0.9;
const PAD_S: f32 = 0.03;
const FLOOR: f32 = 0.01;

pub struct Kokoro {
    session: Session,
    voices: HashMap<String, Vec<f32>>,
    vocab: HashMap<char, i64>,
}

impl Kokoro {
    /// `runtime` is libonnxruntime itself, `model` the ONNX export, and each
    /// voice that export's raw float32 file, named by its file stem.
    pub fn load(
        runtime: &Path,
        model: &Path,
        voices: &[&Path],
        threads: usize,
    ) -> Result<Kokoro, String> {
        // The runtime library loads once per process; a model dropped while
        // idle and loaded again reuses it.
        static ENV: OnceLock<Environment> = OnceLock::new();
        if ENV.get().is_none() {
            let env = ort::init_from(runtime)
                .map_err(|e| e.to_string())?
                .build()
                .map_err(|e| e.to_string())?;
            let _ = ENV.set(env);
        }
        let env = ENV.get().unwrap();
        let session = (|| -> ort::Result<Session> {
            Session::builder(env)?.with_intra_threads(threads)?.commit_from_file(model)
        })()
        .map_err(|e| e.to_string())?;
        let mut loaded = HashMap::new();
        for voice in voices {
            let bytes = std::fs::read(voice).map_err(|e| format!("{voice:?}: {e}"))?;
            if bytes.len() != ROWS * STYLE * 4 {
                return Err(format!("{voice:?} is {} bytes, not a Kokoro voice", bytes.len()));
            }
            let style = bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b));
            let name = voice.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            loaded.insert(name, style.collect());
        }
        Ok(Kokoro {
            session,
            voices: loaded,
            vocab: crate::tables::VOCAB.iter().copied().collect(),
        })
    }

    pub fn speak(&mut self, phonemes: &str, voice: &str) -> Result<Vec<f32>, String> {
        let voice = self.voices.get(voice).ok_or_else(|| format!("no voice {voice}"))?;
        let ids: Vec<i64> = phonemes.chars().filter_map(|p| self.vocab.get(&p).copied()).collect();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // KPipeline indexes the voice by the phoneme string's length, pads
        // with id 0 at each end, and stops at 510 tokens.
        let row = phonemes.chars().count().clamp(1, ROWS) - 1;
        let style = voice[row * STYLE..(row + 1) * STYLE].to_vec();
        let mut input = vec![0];
        input.extend(ids.iter().take(ROWS - 2));
        input.push(0);
        let n = input.len();
        let run = || -> ort::Result<Vec<f32>> {
            let outputs = self.session.run(ort::inputs![
                "input_ids" => Tensor::from_array(([1, n], input))?,
                "style" => Tensor::from_array(([1, STYLE], style))?,
                "speed" => Tensor::from_array(([1], vec![1.0f32]))?,
            ])?;
            Ok(outputs[0].try_extract_tensor::<f32>()?.1.to_vec())
        };
        Ok(shape(run().map_err(|e| e.to_string())?))
    }
}

fn shape(mut audio: Vec<f32>) -> Vec<f32> {
    let max = audio.iter().fold(0f32, |m, s| m.max(s.abs()));
    if max == 0.0 {
        return audio;
    }
    let loud: Vec<usize> =
        audio.iter().enumerate().filter(|(_, s)| s.abs() > FLOOR * max).map(|(i, _)| i).collect();
    let pad = (PAD_S * RATE as f32) as usize;
    let (first, last) = (loud[0], loud[loud.len() - 1]);
    audio = audio[first.saturating_sub(pad)..(last + pad).min(audio.len())].to_vec();
    let fade = pad.min(audio.len() / 2);
    let len = audio.len();
    for i in 0..fade {
        let ramp = i as f32 / (fade - 1).max(1) as f32;
        audio[i] *= ramp;
        audio[len - 1 - i] *= ramp;
    }
    let max = audio.iter().fold(0f32, |m, s| m.max(s.abs()));
    audio.iter().map(|s| s * PEAK / max).collect()
}

/// 16-bit PCM, as the embedded clips are.
pub fn wav(audio: &[f32]) -> Vec<u8> {
    let data = (audio.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&RATE.to_le_bytes());
    out.extend_from_slice(&(RATE * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data.to_le_bytes());
    for s in audio {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    out
}
