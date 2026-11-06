//! `tts <speech-dir> <libonnxruntime> <out-dir> <text>...`: speaks each text
//! into `<out-dir>/<n>.wav` and says how long every stage took.

use std::path::Path;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, runtime, out, texts @ ..] = args.as_slice() else {
        eprintln!("usage: tts <speech-dir> <libonnxruntime> <out-dir> <text>...");
        std::process::exit(2);
    };
    let threads = std::env::var("TTS_THREADS").ok().and_then(|t| t.parse().ok()).unwrap_or(2);
    let t = Instant::now();
    let mut speaker = tts::Speaker::load(Path::new(dir), Path::new(runtime), threads).unwrap();
    println!("load {:6.0} ms ({threads} threads)", t.elapsed().as_secs_f64() * 1e3);
    for (i, text) in texts.iter().enumerate() {
        let t = Instant::now();
        match speaker.speak(text) {
            Ok(audio) => {
                std::fs::write(Path::new(out).join(format!("{i}.wav")), tts::kokoro::wav(&audio))
                    .unwrap();
                println!(
                    "{i}: {:6.0} ms  audio {:4.2} s  {text}",
                    t.elapsed().as_secs_f64() * 1e3,
                    audio.len() as f64 / tts::kokoro::RATE as f64
                );
            }
            Err(e) => println!("{i}: {e}  {text}"),
        }
    }
}
