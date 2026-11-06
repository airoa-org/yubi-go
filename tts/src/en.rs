//! English text to Kokoro phonemes: a port of misaki 0.9.4's `en.G2P` as
//! Kokoro's `KPipeline(lang_code='a')` runs it for `af_heart`, which is what
//! `generate.py` feeds the embedded clips. `tests/en.rs` holds the port to
//! misaki's output phrase by phrase. The lexicons, `en_gold.tsv` and
//! `en_silver.tsv`, are written from misaki's by `tools/tts/tables_en.py`.
//!
//! Two parts of misaki are not ported, and are where the output can differ:
//! - spaCy. Tokens come from a small rule tokenizer, and part-of-speech tags
//!   from a few word-order rules ([`tokens::tag`]). Tags only choose between a
//!   heteronym's readings (close, record, use) and a few function words.
//! - The espeak-ng fallback, which is GPL C. A word outside the lexicon is
//!   read as two lexicon words if it splits into them, and spelled out letter
//!   by letter otherwise ([`En::fallback`]).

use std::collections::HashMap;

use unicode_normalization::UnicodeNormalization;

mod numbers;
mod tokens;

use numbers::{cardinal, decimal, ordinal_words, year};
use tokens::{retokenize, subtokenize, tag, tokenize};

const PRIMARY: char = 'ˈ';
const SECONDARY: char = 'ˌ';
const VOWELS: &str = "AIOQWYaiuæɑɒɔəɛɜɪʊʌᵻ";
const CONSONANTS: &str = "bdfhjklmnpstvwzðŋɡɹɾʃʒʤʧθ";
const DIPHTHONGS: &str = "AIOQWYʤʧ";
const US_TAUS: &str = "AIOWYiuæɑəɛɪɹʊʌ";
const PUNCTS: &str = ";:,.!?—…\"“”";
const NON_QUOTE_PUNCTS: &str = ";:,.!?—…";
const SUBTOKEN_JUNKS: &str = "',-._‘’/";
const PUNCT_TAGS: [&str; 10] = [".", ",", "-LRB-", "-RRB-", "``", "\"\"", "''", ":", "$", "#"];
const ORDINALS: [&str; 4] = ["st", "nd", "rd", "th"];

#[derive(Clone)]
enum Entry {
    Word(String),
    /// A heteronym: its reading per tag, `None` where misaki has null.
    Tagged(Vec<(String, Option<String>)>),
}

pub struct En {
    golds: HashMap<String, Entry>,
    silvers: HashMap<String, Entry>,
}

#[derive(Clone, Default)]
struct Tk {
    text: String,
    tag: String,
    whitespace: bool,
    phonemes: Option<String>,
    alias: Option<String>,
    currency: Option<char>,
    is_head: bool,
    prespace: bool,
}

enum Word {
    One(Tk),
    Group(Vec<Tk>),
}

#[derive(Clone, Copy, Default)]
struct Ctx {
    /// Whether the next word starts with a vowel; `None` at a clause end.
    future_vowel: Option<bool>,
    future_to: bool,
}

fn load(text: &str) -> HashMap<String, Entry> {
    let mut d = HashMap::new();
    for line in text.lines() {
        let Some((word, ps)) = line.split_once('\t') else { continue };
        let entry = match ps.contains('=') {
            false => Entry::Word(ps.to_string()),
            true => Entry::Tagged(
                ps.split('|')
                    .filter_map(|kv| kv.split_once('='))
                    .map(|(k, v)| (k.to_string(), (v != "~").then(|| v.to_string())))
                    .collect(),
            ),
        };
        d.insert(word.to_string(), entry);
    }
    // misaki's grow_dictionary: "word" also answers "Word", and "Word" "word".
    let mut grown = HashMap::new();
    for (k, v) in &d {
        if k.chars().count() < 2 {
            continue;
        }
        if *k == k.to_lowercase() {
            if *k != capitalize(k) {
                grown.insert(capitalize(k), v.clone());
            }
        } else if *k == capitalize(&k.to_lowercase()) {
            grown.insert(k.to_lowercase(), v.clone());
        }
    }
    grown.extend(d);
    grown
}

impl En {
    pub fn new() -> Result<En, String> {
        Ok(En {
            golds: load(include_str!("en_gold.tsv")),
            silvers: load(include_str!("en_silver.tsv")),
        })
    }

    pub fn phonemes(&self, text: &str) -> String {
        let tokens = tag(tokenize(text.trim_start()));
        let mut words = retokenize(&tokens);
        let mut ctx = Ctx::default();
        for w in words.iter_mut().rev() {
            match w {
                Word::One(tk) => {
                    if tk.phonemes.is_none() {
                        tk.phonemes = self.lexicon(tk, ctx);
                    }
                    if tk.phonemes.is_none() {
                        tk.phonemes = self.fallback(&tk.text);
                    }
                    ctx = token_context(ctx, tk.phonemes.as_deref(), tk);
                }
                Word::Group(w) => ctx = self.resolve_group(w, ctx),
            }
        }
        let mut out = String::new();
        for w in words {
            let tk = match w {
                Word::One(tk) => tk,
                Word::Group(w) => merge(&w, true),
            };
            out.push_str(&tk.phonemes.unwrap_or_default().replace('ɾ', "T").replace('ʔ', "t"));
            if tk.whitespace {
                out.push(' ');
            }
        }
        out.trim().to_string()
    }

    fn resolve_group(&self, w: &mut [Tk], mut ctx: Ctx) -> Ctx {
        let (mut left, mut right) = (0, w.len());
        let mut fall_back = false;
        while left < right {
            let ps = match w[left..right].iter().any(|t| t.alias.is_some() || t.phonemes.is_some())
            {
                true => None,
                false => {
                    let tk = merge(&w[left..right], false);
                    self.lexicon(&tk, ctx).map(|ps| (ps, tk))
                }
            };
            if let Some((ps, tk)) = ps {
                ctx = token_context(ctx, Some(&ps), &tk);
                w[left].phonemes = Some(ps);
                for x in &mut w[left + 1..right] {
                    x.phonemes = Some(String::new());
                }
                right = left;
                left = 0;
            } else if left + 1 < right {
                left += 1;
            } else {
                right -= 1;
                let tk = &mut w[right];
                if tk.phonemes.is_none() {
                    if tk.text.chars().all(|c| SUBTOKEN_JUNKS.contains(c)) {
                        tk.phonemes = Some(String::new());
                    } else {
                        fall_back = true;
                        break;
                    }
                }
                left = 0;
            }
        }
        if fall_back {
            let tk = merge(w, false);
            w[0].phonemes = self.fallback(&tk.text);
            for x in &mut w[1..] {
                x.phonemes = Some(String::new());
            }
        } else {
            resolve_tokens(w);
        }
        ctx
    }

    /// misaki hands what its lexicon cannot read to espeak-ng. Here, as
    /// espeak reads them: re- or un- before a lexicon word keeps the word's
    /// stress (replug ɹᵻplˈʌɡ); two lexicon words joined lose the second's
    /// stress (subtask sˈʌbtæsk). Other words are spelled letter by letter.
    /// Digits read as numbers and punctuation stays, so 7:30 is
    /// sˈɛvən:θˈɜɹTi as espeak has it.
    fn fallback(&self, text: &str) -> Option<String> {
        if text.bytes().any(|b| b.is_ascii_digit()) {
            let mut ps = String::new();
            for part in subtokenize(text) {
                if part.bytes().all(|b| b.is_ascii_digit()) {
                    ps.push_str(&self.get_number(&part, None, true)?);
                } else if part.chars().any(char::is_alphabetic) {
                    ps.push_str(&self.fallback(&part)?);
                } else {
                    ps.extend(part.chars().filter(|c| PUNCTS.contains(*c)));
                }
            }
            return Some(ps);
        }
        let word: String = text.chars().filter(|c| c.is_ascii_alphabetic()).collect();
        let lower = word.to_lowercase();
        for (prefix, ps) in [("re", "ɹᵻ"), ("un", "ʌn")] {
            if let Some(rest) = lower.strip_prefix(prefix).filter(|r| r.len() >= 3) {
                if let Some(rest) = self.plain(rest) {
                    return Some(format!("{ps}{rest}"));
                }
            }
        }
        let n = lower.len();
        for cut in (3..n.saturating_sub(2)).rev() {
            let (a, b) = lower.split_at(cut);
            if let (Some(a), Some(b)) = (self.plain(a), self.plain(b)) {
                return Some(a + &apply_stress(&b, Some(-2.0)));
            }
        }
        self.get_nnp(&word)
    }

    fn plain(&self, word: &str) -> Option<String> {
        match self.golds.get(word).or_else(|| self.silvers.get(word))? {
            Entry::Word(ps) => Some(ps.clone()),
            Entry::Tagged(v) => v.iter().find(|(k, _)| k == "DEFAULT").and_then(|(_, p)| p.clone()),
        }
    }

    fn gold(&self, word: &str) -> Option<String> {
        match self.golds.get(word)? {
            Entry::Word(ps) => Some(ps.clone()),
            Entry::Tagged(_) => None,
        }
    }

    fn get_nnp(&self, word: &str) -> Option<String> {
        let mut ps = String::new();
        for c in word.chars().filter(|c| c.is_alphabetic()) {
            ps.push_str(&self.gold(&c.to_uppercase().to_string())?);
        }
        let ps = apply_stress(&ps, Some(0.0));
        Some(match ps.rfind(SECONDARY) {
            Some(i) => format!("{}{PRIMARY}{}", &ps[..i], &ps[i + SECONDARY.len_utf8()..]),
            None => ps,
        })
    }

    fn special(&self, word: &str, tag: &str, stress: Option<f32>, ctx: Ctx) -> Option<String> {
        const SYMBOLS: [(&str, &str); 4] =
            [("%", "percent"), ("&", "and"), ("+", "plus"), ("@", "at")];
        if let Some((_, w)) = SYMBOLS.iter().find(|(s, _)| *s == word) {
            return self.lookup(w, "", None, Some(ctx));
        }
        let stripped = word.trim_matches('.');
        let bare = word.replace('.', "");
        if stripped.contains('.')
            && is_alpha(&bare)
            && word.split('.').map(|s| s.chars().count()).max().unwrap_or(0) < 3
        {
            return self.get_nnp(word);
        }
        match word {
            "a" | "A" => return Some(if tag == "DT" { "ɐ" } else { "ˈA" }.into()),
            "am" | "Am" | "AM" => {
                if tag.starts_with("NN") {
                    return self.get_nnp(word);
                }
                if ctx.future_vowel.is_none() || word != "am" || stress.is_some_and(|s| s > 0.0) {
                    return self.gold("am");
                }
                return Some("ɐm".into());
            }
            "an" | "An" | "AN" => {
                if word == "AN" && tag.starts_with("NN") {
                    return self.get_nnp(word);
                }
                return Some("ɐn".into());
            }
            "I" if tag == "PRP" => return Some(format!("{SECONDARY}I")),
            "by" | "By" | "BY" if parent(tag) == "ADV" => return Some("bˈI".into()),
            _ => {}
        }
        if word == "to" || word == "To" || (word == "TO" && (tag == "TO" || tag == "IN")) {
            return match ctx.future_vowel {
                None => self.gold("to"),
                Some(false) => Some("tə".into()),
                Some(true) => Some("tʊ".into()),
            };
        }
        if word == "in" || word == "In" || (word == "IN" && tag != "NNP") {
            let stress = if ctx.future_vowel.is_none() || tag != "IN" { "ˈ" } else { "" };
            return Some(format!("{stress}ɪn"));
        }
        if word == "the" || word == "The" || (word == "THE" && tag == "DT") {
            return Some(if ctx.future_vowel == Some(true) { "ði" } else { "ðə" }.into());
        }
        if tag == "IN" && (word.eq_ignore_ascii_case("vs") || word.eq_ignore_ascii_case("vs.")) {
            return self.lookup("versus", "", None, Some(ctx));
        }
        if word == "used" || word == "Used" || word == "USED" {
            let key =
                if (tag == "VBD" || tag == "JJ") && ctx.future_to { "VBD" } else { "DEFAULT" };
            if let Some(Entry::Tagged(v)) = self.golds.get("used") {
                return v.iter().find(|(k, _)| k == key).and_then(|(_, p)| p.clone());
            }
        }
        None
    }

    fn is_known(&self, word: &str) -> bool {
        if self.golds.contains_key(word)
            || ["%", "&", "+", "@"].contains(&word)
            || self.silvers.contains_key(word)
        {
            return true;
        }
        if !is_alpha(word)
            || !word.chars().all(|c| c == '\'' || c == '-' || c.is_ascii_alphabetic())
        {
            return false;
        }
        if word.chars().count() == 1 {
            return true;
        }
        if word == word.to_uppercase() && self.golds.contains_key(&word.to_lowercase()) {
            return true;
        }
        let rest: String = word.chars().skip(1).collect();
        rest == rest.to_uppercase()
    }

    fn lookup(
        &self,
        word: &str,
        tag: &str,
        stress: Option<f32>,
        ctx: Option<Ctx>,
    ) -> Option<String> {
        let mut word = word.to_string();
        let mut is_nnp = false;
        if word == word.to_uppercase() && !self.golds.contains_key(&word) {
            word = word.to_lowercase();
            is_nnp = tag == "NNP";
        }
        let mut entry = self.golds.get(&word);
        if entry.is_none() && !is_nnp {
            entry = self.silvers.get(&word);
        }
        let ps = match entry {
            None => None,
            Some(Entry::Word(ps)) => Some(ps.clone()),
            Some(Entry::Tagged(v)) => {
                let has = |k: &str| v.iter().any(|(key, _)| key == k);
                let key = if ctx.is_some_and(|c| c.future_vowel.is_none()) && has("None") {
                    "None".to_string()
                } else if has(tag) {
                    tag.to_string()
                } else {
                    parent(tag).to_string()
                };
                v.iter()
                    .find(|(k, _)| *k == key)
                    .or_else(|| v.iter().find(|(k, _)| k == "DEFAULT"))
                    .and_then(|(_, p)| p.clone())
            }
        };
        if ps.is_none() || (is_nnp && !ps.as_deref().unwrap_or("").contains(PRIMARY)) {
            if let Some(nnp) = self.get_nnp(&word) {
                return Some(nnp);
            }
        }
        ps.map(|ps| apply_stress(&ps, stress))
    }

    fn stem_s(
        &self,
        word: &str,
        tag: &str,
        stress: Option<f32>,
        ctx: Option<Ctx>,
    ) -> Option<String> {
        if word.chars().count() < 3 || !word.ends_with('s') {
            return None;
        }
        let stem = if !word.ends_with("ss") && self.is_known(&word[..word.len() - 1]) {
            word[..word.len() - 1].to_string()
        } else if (word.ends_with("'s")
            || (word.len() > 4 && word.ends_with("es") && !word.ends_with("ies")))
            && self.is_known(&word[..word.len() - 2])
        {
            word[..word.len() - 2].to_string()
        } else if word.len() > 4
            && word.ends_with("ies")
            && self.is_known(&format!("{}y", &word[..word.len() - 3]))
        {
            format!("{}y", &word[..word.len() - 3])
        } else {
            return None;
        };
        self.lookup(&stem, tag, stress, ctx).map(|s| suffix_s(&s))
    }

    fn stem_ed(
        &self,
        word: &str,
        tag: &str,
        stress: Option<f32>,
        ctx: Option<Ctx>,
    ) -> Option<String> {
        if word.chars().count() < 4 || !word.ends_with('d') {
            return None;
        }
        let stem = if !word.ends_with("dd") && self.is_known(&word[..word.len() - 1]) {
            &word[..word.len() - 1]
        } else if word.len() > 4
            && word.ends_with("ed")
            && !word.ends_with("eed")
            && self.is_known(&word[..word.len() - 2])
        {
            &word[..word.len() - 2]
        } else {
            return None;
        };
        self.lookup(stem, tag, stress, ctx).map(|s| suffix_ed(&s))
    }

    fn stem_ing(
        &self,
        word: &str,
        tag: &str,
        stress: Option<f32>,
        ctx: Option<Ctx>,
    ) -> Option<String> {
        if word.chars().count() < 5 || !word.ends_with("ing") {
            return None;
        }
        let base = &word[..word.len() - 3];
        let doubled = {
            let b = word.as_bytes();
            let n = b.len();
            word.ends_with("cking")
                || (n > 5 && b[n - 4] == b[n - 5] && b"bcdgklmnprstvxz".contains(&b[n - 4]))
        };
        let stem = if word.len() > 5 && self.is_known(base) {
            base.to_string()
        } else if self.is_known(&format!("{base}e")) {
            format!("{base}e")
        } else if word.len() > 5 && doubled && self.is_known(&word[..word.len() - 4]) {
            word[..word.len() - 4].to_string()
        } else {
            return None;
        };
        self.lookup(&stem, tag, stress, ctx).map(|s| suffix_ing(&s))
    }

    fn get_word(&self, word: &str, tag: &str, stress: Option<f32>, ctx: Ctx) -> Option<String> {
        if let Some(ps) = self.special(word, tag, stress, ctx) {
            return Some(ps);
        }
        let lower = word.to_lowercase();
        let rest: String = word.chars().skip(1).collect();
        let mut word = word.to_string();
        if word.chars().count() > 1
            && is_alpha(&word.replace('\'', ""))
            && word != lower
            && (tag != "NNP" || word.chars().count() > 7)
            && !self.golds.contains_key(&word)
            && !self.silvers.contains_key(&word)
            && (word == word.to_uppercase() || rest == rest.to_lowercase())
            && (self.golds.contains_key(&lower)
                || self.silvers.contains_key(&lower)
                || [Self::stem_s, Self::stem_ed, Self::stem_ing].iter().any(|f| {
                    f(self, &lower, tag, stress, Some(ctx)).is_some_and(|p| !p.is_empty())
                }))
        {
            word = lower;
        }
        if self.is_known(&word) {
            return self.lookup(&word, tag, stress, Some(ctx));
        }
        if let Some(stem) = word.strip_suffix("s'") {
            let possessive = format!("{stem}'s");
            if self.is_known(&possessive) {
                return self.lookup(&possessive, tag, stress, Some(ctx));
            }
        }
        if let Some(stem) = word.strip_suffix('\'') {
            if self.is_known(stem) {
                return self.lookup(stem, tag, stress, Some(ctx));
            }
        }
        self.stem_s(&word, tag, stress, Some(ctx))
            .or_else(|| self.stem_ed(&word, tag, stress, Some(ctx)))
            .or_else(|| self.stem_ing(&word, tag, Some(stress.unwrap_or(0.5)), Some(ctx)))
    }

    /// misaki's `Lexicon.__call__`.
    fn lexicon(&self, tk: &Tk, ctx: Ctx) -> Option<String> {
        let word: String =
            tk.alias.as_ref().unwrap_or(&tk.text).replace(['‘', '’'], "'").nfkc().collect();
        let stress = match (word == word.to_lowercase(), word == word.to_uppercase()) {
            (true, _) => None,
            (false, true) => Some(2.0),
            (false, false) => Some(0.5),
        };
        if let Some(ps) = self.get_word(&word, &tk.tag, stress, ctx) {
            return Some(self.append_currency(ps, tk.currency));
        }
        if is_number(&word, tk.is_head) {
            return self.get_number(&word, tk.currency, tk.is_head);
        }
        None
    }

    fn append_currency(&self, ps: String, currency: Option<char>) -> String {
        match currency.and_then(currency_units) {
            Some((unit, _)) => match self.stem_s(&format!("{unit}s"), "", None, None) {
                Some(c) => format!("{ps} {c}"),
                None => ps,
            },
            None => ps,
        }
    }

    fn get_number(&self, word: &str, currency: Option<char>, is_head: bool) -> Option<String> {
        let suffix_len = word
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_lowercase() || *c == '\'')
            .map(char::len_utf8)
            .sum::<usize>();
        let (mut word, suffix) = word.split_at(word.len() - suffix_len);
        let suffix = (!suffix.is_empty()).then_some(suffix);
        let mut result: Vec<String> = Vec::new();
        let mut push = |w: &str, result: &mut Vec<String>| {
            let stress = if w == "point" { Some(-2.0) } else { None };
            if let Some(ps) = self.lookup(w, "", stress, None) {
                result.push(ps);
            }
        };
        let extend = |words: &str,
                      result: &mut Vec<String>,
                      push: &mut dyn FnMut(&str, &mut Vec<String>)| {
            for w in words.split(|c: char| !c.is_ascii_lowercase()).filter(|w| !w.is_empty()) {
                if w != "and" {
                    push(w, result);
                }
            }
        };
        if let Some(rest) = word.strip_prefix('-') {
            push("minus", &mut result);
            word = rest;
        }
        let ordinal = suffix.is_some_and(|s| ORDINALS.contains(&s));
        if is_digits(word) && ordinal {
            extend(&ordinal_words(word.parse().ok()?), &mut result, &mut push);
        } else if result.is_empty()
            && word.len() == 4
            && currency.and_then(currency_units).is_none()
            && is_digits(word)
        {
            extend(&year(word.parse().ok()?), &mut result, &mut push);
        } else if !is_head && !word.contains('.') {
            let num = word.replace(',', "");
            if num.starts_with('0') || num.len() > 3 {
                for n in num.chars() {
                    extend(&cardinal(n.to_digit(10)? as u64), &mut result, &mut push);
                }
            } else if num.len() == 3 && !num.ends_with("00") {
                extend(&cardinal(num[..1].parse().ok()?), &mut result, &mut push);
                if &num[1..2] == "0" {
                    if let Some(o) = self.lookup("O", "", Some(-2.0), None) {
                        result.push(o);
                    }
                    extend(&cardinal(num[2..].parse().ok()?), &mut result, &mut push);
                } else {
                    extend(&cardinal(num[1..].parse().ok()?), &mut result, &mut push);
                }
            } else {
                extend(&cardinal(num.parse().ok()?), &mut result, &mut push);
            }
        } else if word.matches('.').count() > 1 || !is_head {
            for num in word.replace(',', "").split('.') {
                if num.is_empty() {
                    continue;
                }
                if num.starts_with('0') || (num.len() != 2 && num[1..].chars().any(|n| n != '0')) {
                    for n in num.chars() {
                        extend(&cardinal(n.to_digit(10)? as u64), &mut result, &mut push);
                    }
                } else {
                    extend(&cardinal(num.parse().ok()?), &mut result, &mut push);
                }
            }
        } else if let Some(units) = currency.and_then(currency_units).filter(|_| is_currency(word))
        {
            let parts: Vec<&str> = word.split('.').collect();
            let mut pairs: Vec<(u64, &str)> = parts
                .iter()
                .zip([units.0, units.1])
                .map(|(n, u)| (n.replace(',', "").parse().unwrap_or(0), u))
                .collect();
            if pairs.len() > 1 {
                if pairs[1].0 == 0 {
                    pairs.truncate(1);
                } else if pairs[0].0 == 0 {
                    pairs.remove(0);
                }
            }
            for (i, (num, unit)) in pairs.into_iter().enumerate() {
                if i > 0 {
                    push("and", &mut result);
                }
                extend(&cardinal(num), &mut result, &mut push);
                let unit_ps = if num != 1 && unit != "pence" {
                    self.stem_s(&format!("{unit}s"), "", None, None)
                } else {
                    self.lookup(unit, "", None, None)
                };
                result.extend(unit_ps);
            }
        } else {
            let words = if is_digits(word) {
                cardinal(word.parse().ok()?)
            } else if !word.contains('.') {
                let n: u64 = word.replace(',', "").parse().ok()?;
                if ordinal {
                    ordinal_words(n)
                } else {
                    cardinal(n)
                }
            } else if let Some(frac) = word.replace(',', "").strip_prefix('.') {
                let digits: Vec<String> = frac
                    .chars()
                    .filter_map(|d| d.to_digit(10))
                    .map(|d| cardinal(d as u64))
                    .collect();
                format!("point {}", digits.join(" "))
            } else {
                decimal(&word.replace(',', ""))?
            };
            extend(&words, &mut result, &mut push);
        }
        if result.is_empty() {
            return None;
        }
        let ps = result.join(" ");
        Some(match suffix {
            Some("s" | "'s") => suffix_s(&ps),
            Some("ed" | "'d") => suffix_ed(&ps),
            Some("ing") => suffix_ing(&ps),
            _ => ps,
        })
    }
}

fn merge(tokens: &[Tk], with_unk: bool) -> Tk {
    let mut text = String::new();
    for (i, tk) in tokens.iter().enumerate() {
        text.push_str(&tk.text);
        if i + 1 < tokens.len() && tk.whitespace {
            text.push(' ');
        }
    }
    let weight = |t: &Tk| {
        t.text
            .chars()
            .map(|c| if c.to_lowercase().eq(std::iter::once(c)) { 1 } else { 2 })
            .sum::<usize>()
    };
    let mut best = &tokens[0];
    for tk in tokens {
        if weight(tk) > weight(best) {
            best = tk;
        }
    }
    let phonemes = with_unk.then(|| {
        let mut ps = String::new();
        for tk in tokens {
            if tk.prespace
                && !ps.is_empty()
                && !ps.ends_with(char::is_whitespace)
                && tk.phonemes.as_ref().is_some_and(|p| !p.is_empty())
            {
                ps.push(' ');
            }
            ps.push_str(tk.phonemes.as_deref().unwrap_or(""));
        }
        ps
    });
    Tk {
        text,
        tag: best.tag.clone(),
        whitespace: tokens[tokens.len() - 1].whitespace,
        phonemes,
        alias: None,
        currency: tokens.iter().filter_map(|t| t.currency).max(),
        is_head: tokens[0].is_head,
        prespace: tokens[0].prespace,
    }
}

fn token_context(ctx: Ctx, ps: Option<&str>, tk: &Tk) -> Ctx {
    let mut vowel = ctx.future_vowel;
    if let Some(ps) = ps.filter(|p| !p.is_empty()) {
        if let Some(c) = ps.chars().find(|c| {
            VOWELS.contains(*c) || CONSONANTS.contains(*c) || NON_QUOTE_PUNCTS.contains(*c)
        }) {
            vowel = if NON_QUOTE_PUNCTS.contains(c) { None } else { Some(VOWELS.contains(c)) };
        }
    }
    let future_to = tk.text == "to"
        || tk.text == "To"
        || (tk.text == "TO" && (tk.tag == "TO" || tk.tag == "IN"));
    Ctx { future_vowel: vowel, future_to }
}

fn stress_weight(ps: &str) -> usize {
    ps.chars().map(|c| if DIPHTHONGS.contains(c) { 2 } else { 1 }).sum()
}

fn resolve_tokens(tokens: &mut [Tk]) {
    let text = merge(tokens, false).text;
    let kinds: std::collections::HashSet<u8> = text
        .chars()
        .filter(|c| !SUBTOKEN_JUNKS.contains(*c))
        .map(|c| {
            if c.is_alphabetic() {
                0
            } else if c.is_ascii_digit() {
                1
            } else {
                2
            }
        })
        .collect();
    let prespace = text.contains(' ') || text.contains('/') || kinds.len() > 1;
    let n = tokens.len();
    for (i, tk) in tokens.iter_mut().enumerate() {
        if tk.phonemes.is_none() {
            if i == n - 1
                && tk.text.chars().count() == 1
                && NON_QUOTE_PUNCTS.contains(tk.text.as_str())
            {
                tk.phonemes = Some(tk.text.clone());
            } else if tk.text.chars().all(|c| SUBTOKEN_JUNKS.contains(c)) {
                tk.phonemes = Some(String::new());
            }
        } else if i > 0 {
            tk.prespace = prespace;
        }
    }
    if prespace {
        return;
    }
    let mut indices: Vec<(bool, usize, usize)> = tokens
        .iter()
        .enumerate()
        .filter_map(|(i, tk)| {
            tk.phonemes
                .as_ref()
                .filter(|p| !p.is_empty())
                .map(|p| (p.contains(PRIMARY), stress_weight(p), i))
        })
        .collect();
    if indices.len() == 2 && tokens[indices[0].2].text.chars().count() == 1 {
        let i = indices[1].2;
        tokens[i].phonemes = tokens[i].phonemes.as_ref().map(|p| apply_stress(p, Some(-0.5)));
        return;
    }
    if indices.len() < 2
        || indices.iter().filter(|(b, _, _)| *b).count() <= indices.len().div_ceil(2)
    {
        return;
    }
    indices.sort();
    let half = indices.len() / 2;
    for &(_, _, i) in &indices[..half] {
        tokens[i].phonemes = tokens[i].phonemes.as_ref().map(|p| apply_stress(p, Some(-0.5)));
    }
}

fn apply_stress(ps: &str, stress: Option<f32>) -> String {
    let has_vowel = ps.chars().any(|c| VOWELS.contains(c));
    let no_marks = !ps.contains(PRIMARY) && !ps.contains(SECONDARY);
    let Some(stress) = stress else { return ps.to_string() };
    if stress < -1.0 {
        ps.replace([PRIMARY, SECONDARY], "")
    } else if stress == -1.0 || ((stress == 0.0 || stress == -0.5) && ps.contains(PRIMARY)) {
        ps.replace(SECONDARY, "").replace(PRIMARY, &SECONDARY.to_string())
    } else if [0.0, 0.5, 1.0].contains(&stress) && no_marks {
        if has_vowel {
            restress(&format!("{SECONDARY}{ps}"))
        } else {
            ps.to_string()
        }
    } else if stress >= 1.0 && !ps.contains(PRIMARY) && ps.contains(SECONDARY) {
        ps.replace(SECONDARY, &PRIMARY.to_string())
    } else if stress > 1.0 && no_marks {
        if has_vowel {
            restress(&format!("{PRIMARY}{ps}"))
        } else {
            ps.to_string()
        }
    } else {
        ps.to_string()
    }
}

/// Moves each stress mark to just before the next vowel.
fn restress(ps: &str) -> String {
    let chars: Vec<char> = ps.chars().collect();
    let mut ips: Vec<(f64, char)> = chars.iter().enumerate().map(|(i, &c)| (i as f64, c)).collect();
    for (i, &c) in chars.iter().enumerate() {
        if c == PRIMARY || c == SECONDARY {
            if let Some(j) = (i..chars.len()).find(|&j| VOWELS.contains(chars[j])) {
                ips[i] = (j as f64 - 0.5, c);
            }
        }
    }
    ips.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    ips.into_iter().map(|(_, c)| c).collect()
}

fn suffix_s(stem: &str) -> String {
    match stem.chars().last() {
        Some(c) if "ptkfθ".contains(c) => format!("{stem}s"),
        Some(c) if "szʃʒʧʤ".contains(c) => format!("{stem}ᵻz"),
        _ => format!("{stem}z"),
    }
}

fn suffix_ed(stem: &str) -> String {
    let chars: Vec<char> = stem.chars().collect();
    match chars.last() {
        Some(c) if "pkfθʃsʧ".contains(*c) => format!("{stem}t"),
        Some('d') => format!("{stem}ᵻd"),
        Some(c) if *c != 't' => format!("{stem}d"),
        _ if chars.len() < 2 => format!("{stem}ɪd"),
        _ if US_TAUS.contains(chars[chars.len() - 2]) => {
            format!("{}ɾᵻd", chars[..chars.len() - 1].iter().collect::<String>())
        }
        _ => format!("{stem}ᵻd"),
    }
}

fn suffix_ing(stem: &str) -> String {
    let chars: Vec<char> = stem.chars().collect();
    if chars.len() > 1 && chars[chars.len() - 1] == 't' && US_TAUS.contains(chars[chars.len() - 2])
    {
        return format!("{}ɾɪŋ", chars[..chars.len() - 1].iter().collect::<String>());
    }
    format!("{stem}ɪŋ")
}

fn parent(tag: &str) -> &str {
    if tag.starts_with("VB") {
        "VERB"
    } else if tag.starts_with("NN") {
        "NOUN"
    } else if tag.starts_with("ADV") || tag.starts_with("RB") {
        "ADV"
    } else if tag.starts_with("ADJ") || tag.starts_with("JJ") {
        "ADJ"
    } else {
        tag
    }
}

fn is_alpha(s: &str) -> bool {
    !s.is_empty() && s.chars().all(char::is_alphabetic)
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

fn is_number(word: &str, is_head: bool) -> bool {
    if !word.bytes().any(|b| b.is_ascii_digit()) {
        return false;
    }
    let mut word = word;
    for s in ["ing", "'d", "ed", "'s", "st", "nd", "rd", "th", "s"] {
        if let Some(w) = word.strip_suffix(s) {
            word = w;
            break;
        }
    }
    word.chars()
        .enumerate()
        .all(|(i, c)| c.is_ascii_digit() || c == ',' || c == '.' || (is_head && i == 0 && c == '-'))
}

fn is_currency(word: &str) -> bool {
    match word.matches('.').count() {
        0 => true,
        1 => word.split('.').nth(1).is_some_and(|cents| cents.len() < 3),
        _ => false,
    }
}

fn currency_units(c: char) -> Option<(&'static str, &'static str)> {
    match c {
        '$' => Some(("dollar", "cent")),
        '£' => Some(("pound", "pence")),
        '€' => Some(("euro", "cent")),
        _ => None,
    }
}
