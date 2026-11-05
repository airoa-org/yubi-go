//! Tokens, as spaCy's English defaults cut them, and the tags and subtokens
//! misaki reads off them.
//!
//! spaCy is MIT-licensed; its licence text is tools/licenses/spacy.txt.

use super::{currency_units, Tk, Word, PUNCTS, PUNCT_TAGS};

const PREFIXES: &str = "([{\"“‘'$£€¿¡«<";
const SUFFIXES: &str = ".,!?:;)]}\"”’'…»>";
const CONTRACTIONS: [&str; 7] = ["n't", "'s", "'re", "'ve", "'ll", "'m", "'d"];

pub(super) fn tokenize(text: &str) -> Vec<Tk> {
    let mut out = Vec::new();
    let chunks: Vec<&str> = text.split_whitespace().collect();
    for chunk in chunks {
        let mut rest = chunk;
        let mut head = Vec::new();
        while let Some(c) = rest.chars().next().filter(|c| PREFIXES.contains(*c)) {
            if rest.chars().count() == 1 {
                break;
            }
            head.push(c.to_string());
            rest = &rest[c.len_utf8()..];
        }
        let mut tail = Vec::new();
        loop {
            if let Some(c) = rest.chars().last().filter(|c| SUFFIXES.contains(*c)) {
                if rest.chars().count() > 1 {
                    tail.push(c.to_string());
                    rest = &rest[..rest.len() - c.len_utf8()];
                    continue;
                }
            }
            let lower = rest.to_lowercase().replace('’', "'");
            if let Some(s) =
                CONTRACTIONS.iter().find(|s| lower.ends_with(**s) && lower.len() > s.len())
            {
                let cut = rest.len()
                    - rest.chars().rev().take(s.chars().count()).map(char::len_utf8).sum::<usize>();
                tail.push(rest[cut..].to_string());
                rest = &rest[..cut];
                continue;
            }
            break;
        }
        let mut pieces = head;
        if !rest.is_empty() {
            pieces.push(rest.to_string());
        }
        pieces.extend(tail.into_iter().rev());
        let n = pieces.len();
        for (i, p) in pieces.into_iter().enumerate() {
            out.push(Tk { text: p, whitespace: i + 1 == n, is_head: true, ..Tk::default() });
        }
    }
    if let Some(last) = out.last_mut() {
        last.whitespace = false;
    }
    out
}

/// Part-of-speech tags from word order, standing in for spaCy's tagger.
/// Clause-initial words before an object are imperative verbs, because task
/// names are instructions; a word between a determiner and a noun is an
/// adjective; everything else open-class is a noun.
pub(super) fn tag(mut tokens: Vec<Tk>) -> Vec<Tk> {
    const DT: [&str; 14] = [
        "the", "a", "an", "this", "that", "these", "those", "no", "each", "every", "both", "all",
        "some", "another",
    ];
    const PRPS: [&str; 7] = ["my", "your", "his", "her", "its", "our", "their"];
    const PRP: [&str; 11] =
        ["i", "you", "he", "she", "it", "we", "they", "me", "him", "us", "them"];
    const IN: [&str; 31] = [
        "of", "in", "on", "at", "from", "with", "without", "by", "into", "onto", "under", "over",
        "for", "about", "after", "before", "through", "across", "between", "behind", "beside",
        "near", "inside", "outside", "above", "below", "toward", "towards", "around", "along",
        "until",
    ];
    const PARTICLES: [&str; 9] = ["up", "down", "out", "off", "in", "on", "away", "back", "over"];
    const CC: [&str; 4] = ["and", "or", "but", "nor"];
    const RB: [&str; 12] = [
        "not", "n't", "too", "very", "then", "also", "again", "still", "almost", "now", "just",
        "yet",
    ];
    const MD: [&str; 9] = ["can", "could", "will", "would", "should", "must", "may", "might", "ca"];
    let lower: Vec<String> = tokens.iter().map(|t| t.text.to_lowercase()).collect();
    let is_word = |i: usize| {
        tokens.get(i).is_some_and(|t| t.text.chars().next().is_some_and(char::is_alphanumeric))
    };
    let n = tokens.len();
    // Followed by what an imperative verb takes: an object, a particle, or a
    // number ("Put 3 screws ...").
    let takes_object = |i: usize| match lower.get(i + 1).map(String::as_str) {
        None => i == 0,
        Some(nx) => {
            [&DT[..], &PRPS, &PRP, &PARTICLES, &CC].iter().any(|set| set.contains(&nx))
                || nx.starts_with(|c: char| c.is_ascii_digit())
        }
    };
    let mut tags: Vec<String> = Vec::with_capacity(n);
    for i in 0..n {
        let t = &tokens[i].text;
        let w = lower[i].as_str();
        let prev = if i > 0 { tags[i - 1].as_str() } else { "" };
        let next = lower.get(i + 1).map(String::as_str);
        let clause_start = i == 0
            || [".", ",", ":", "CC"].contains(&prev)
            || (prev == "RB" && lower[i - 1] == "then");
        let first = t.chars().next().unwrap_or(' ');
        let tag = if !first.is_alphanumeric() {
            match t.as_str() {
                "." | "!" | "?" | "…" => ".",
                "," => ",",
                ":" | ";" | "-" | "–" | "—" => ":",
                "(" | "[" | "{" => "-LRB-",
                ")" | "]" | "}" => "-RRB-",
                "“" => "``",
                "”" => "''",
                "\"" => match i == 0 || tokens[i - 1].whitespace {
                    true => "``",
                    false => "''",
                },
                "$" | "£" | "€" => "$",
                _ => "NFP",
            }
        } else if first.is_ascii_digit() {
            "CD"
        } else if w == "to" {
            "TO"
        } else if DT.contains(&w) && !(w == "a" && t == "A" && i > 0) {
            "DT"
        } else if PRPS.contains(&w) {
            "PRP$"
        } else if PRP.contains(&w) {
            "PRP"
        } else if CC.contains(&w) {
            "CC"
        } else if MD.contains(&w) {
            "MD"
        } else if PARTICLES.contains(&w) && prev.starts_with("VB")
            || (PARTICLES.contains(&w) && (next.is_none() || !is_word(i + 1)))
        {
            "RP"
        } else if IN.contains(&w) {
            "IN"
        } else if RB.contains(&w) || (w.ends_with("ly") && w.len() > 4) {
            "RB"
        } else if prev == "TO" || prev == "MD" || (clause_start && takes_object(i)) {
            "VB"
        } else if w.ends_with("ing") && w.len() > 4 {
            "VBG"
        } else if w.ends_with("ed") && w.len() > 3 {
            "VBN"
        } else if i > 0
            && !clause_start
            && first.is_uppercase()
            && t.chars().skip(1).any(char::is_lowercase)
        {
            "NNP"
        } else if ["DT", "PRP$", "JJ"].contains(&prev)
            && is_word(i + 1)
            && !next.is_some_and(|nx| IN.contains(&nx) || CC.contains(&nx) || nx == "to")
        {
            "JJ"
        } else {
            "NN"
        };
        tags.push(tag.to_string());
    }
    for (tk, tag) in tokens.iter_mut().zip(tags) {
        tk.tag = tag;
    }
    tokens
}

/// misaki's subtokenize regex, alternative by alternative:
/// `^['‘’]+|\p{Lu}(?=\p{Lu}\p{Ll})|(?:^-)?(?:\d?[,.]?\d)+|[-_]+|['‘’]{2,}|`
/// `\p{L}*?(?:['‘’]\p{L})*?\p{Ll}(?=\p{Lu})|\p{L}+(?:['‘’]\p{L})*|`
/// `[^-_\p{L}'‘’\d]|['‘’]+$`
pub(super) fn subtokenize(word: &str) -> Vec<String> {
    let c: Vec<char> = word.chars().collect();
    let n = c.len();
    let apos = |x: char| "'‘’".contains(x);
    let digit = |i: usize| i < n && c[i].is_ascii_digit();
    let sep = |i: usize| i < n && (c[i] == ',' || c[i] == '.');
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let end = (|| {
            if i == 0 && apos(c[0]) {
                return Some((0..n).find(|&j| !apos(c[j])).unwrap_or(n));
            }
            if i + 2 < n
                && c[i].is_uppercase()
                && c[i + 1].is_uppercase()
                && c[i + 2].is_lowercase()
            {
                return Some(i + 1);
            }
            let start = if i == 0 && c[0] == '-' && digit(1) { 1 } else { i };
            let mut j = start;
            loop {
                if digit(j) && sep(j + 1) && digit(j + 2) {
                    j += 3;
                } else if (digit(j) || sep(j)) && digit(j + 1) {
                    j += 2;
                } else if digit(j) {
                    j += 1;
                } else {
                    break;
                }
            }
            if j > start {
                return Some(j);
            }
            if c[i] == '-' || c[i] == '_' {
                return Some((i..n).find(|&j| c[j] != '-' && c[j] != '_').unwrap_or(n));
            }
            if apos(c[i]) {
                let j = (i..n).find(|&j| !apos(c[j])).unwrap_or(n);
                if j - i >= 2 {
                    return Some(j);
                }
            }
            if c[i].is_alphabetic() {
                let mut k = i;
                while k < n && c[k].is_alphabetic() {
                    if c[k].is_lowercase() && k + 1 < n && c[k + 1].is_uppercase() {
                        return Some(k + 1);
                    }
                    k += 1;
                }
                let mut j = k;
                while j + 1 < n && apos(c[j]) && c[j + 1].is_alphabetic() {
                    j += 2;
                }
                return Some(j);
            }
            if !(c[i] == '-' || c[i] == '_' || apos(c[i]) || c[i].is_ascii_digit()) {
                return Some(i + 1);
            }
            if apos(c[i]) && (i..n).all(|j| apos(c[j])) {
                return Some(n);
            }
            None
        })();
        match end {
            Some(e) => {
                out.push(c[i..e].iter().collect());
                i = e;
            }
            None => i += 1,
        }
    }
    out
}

pub(super) fn retokenize(tokens: &[Tk]) -> Vec<Word> {
    let mut words: Vec<Word> = Vec::new();
    let mut currency: Option<char> = None;
    for (i, token) in tokens.iter().enumerate() {
        let mut tks: Vec<Tk> = match token.alias.is_none() && token.phonemes.is_none() {
            true => subtokenize(&token.text)
                .into_iter()
                .map(|t| Tk {
                    text: t,
                    whitespace: false,
                    is_head: true,
                    prespace: false,
                    ..token.clone()
                })
                .collect(),
            false => vec![token.clone()],
        };
        if tks.is_empty() {
            continue;
        }
        let last = tks.len() - 1;
        tks[last].whitespace = token.whitespace;
        let n = tks.len();
        for j in 0..n {
            let (before, after) = tks.split_at_mut(j);
            let (tk, after) = after.split_first_mut().unwrap();
            if tk.alias.is_some() || tk.phonemes.is_some() {
            } else if tk.tag == "$"
                && tk.text.chars().count() == 1
                && currency_units(tk.text.chars().next().unwrap()).is_some()
            {
                currency = tk.text.chars().next();
                tk.phonemes = Some(String::new());
            } else if tk.tag == ":" && (tk.text == "-" || tk.text == "–") {
                tk.phonemes = Some("—".into());
            } else if PUNCT_TAGS.contains(&tk.tag.as_str())
                && !tk.text.chars().all(|c| c.is_ascii_alphabetic())
            {
                tk.phonemes = Some(match tk.tag.as_str() {
                    "-LRB-" => "(".into(),
                    "-RRB-" => ")".into(),
                    "``" => "“".into(),
                    "\"\"" | "''" => "”".into(),
                    _ => tk.text.chars().filter(|c| PUNCTS.contains(*c)).collect(),
                });
            } else if currency.is_some() {
                if tk.tag != "CD" {
                    currency = None;
                } else if j + 1 == n && (i + 1 == tokens.len() || tokens[i + 1].tag != "CD") {
                    tk.currency = currency;
                }
            } else if j > 0 && !after.is_empty() && tk.text == "2" {
                let l = before[j - 1].text.chars().last();
                let r = after[0].text.chars().next();
                if l.is_some_and(char::is_alphabetic) && r.is_some_and(char::is_alphabetic) {
                    tk.alias = Some("to".into());
                }
            }
        }
        for mut tk in tks {
            if tk.alias.is_some() || tk.phonemes.is_some() {
                words.push(Word::One(tk));
            } else if let Some(Word::Group(g)) = words
                .last_mut()
                .filter(|w| matches!(w, Word::Group(g) if !g.last().unwrap().whitespace))
            {
                tk.is_head = false;
                g.push(tk);
            } else if tk.whitespace {
                words.push(Word::One(tk));
            } else {
                words.push(Word::Group(vec![tk]));
            }
        }
    }
    words
        .into_iter()
        .map(|w| match w {
            Word::Group(mut g) if g.len() == 1 => Word::One(g.pop().unwrap()),
            w => w,
        })
        .collect()
}
