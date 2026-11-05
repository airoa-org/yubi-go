//! Every English phrase in assets/voice/phrases.tsv plus some task names,
//! against what Kokoro's English pipeline gave for it (tools/tts/ref_en.py).

use tts::en;

/// Where our output differs from misaki's: (text, ours, why).
const KNOWN: [(&str, &str, &str); 2] = [
    (
        "Insert the USB cable",
        "ɪnsˈɜɹt ðə jˌuˌɛsbˈi kˈAbᵊl",
        "spaCy tags a sentence-initial Insert as a proper noun, which takes the noun's stress",
    ),
    ("I read the book", "ˌI ɹˈid ðə bˈʊk", "spaCy guesses past tense; the tag rules have no tense"),
];

#[test]
fn phonemes_match_misaki() {
    let en = en::En::new().unwrap();
    let mut wrong = Vec::new();
    let cases = include_str!("misaki_en.tsv");
    for line in cases.lines() {
        let (text, want) = line.split_once('\t').unwrap();
        let got = en.phonemes(text);
        let want = KNOWN.iter().find(|(t, _, _)| *t == text).map_or(want, |(_, ours, _)| ours);
        if got != want {
            wrong.push(format!("{text}\n  misaki {want}\n  ours   {got}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "{} of {} differ:\n{}",
        wrong.len(),
        cases.lines().count(),
        wrong.join("\n")
    );
}
