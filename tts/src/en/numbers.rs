//! English number words, matching what misaki gets from num2words. Only the words matter: misaki
//! splits on anything but a-z and drops "and".

const ONES: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];
const TENS: [&str; 10] =
    ["", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety"];
const SCALES: [&str; 7] =
    ["", "thousand", "million", "billion", "trillion", "quadrillion", "quintillion"];

fn below_thousand(n: u64, out: &mut Vec<String>) {
    if n >= 100 {
        out.push(ONES[(n / 100) as usize].into());
        out.push("hundred".into());
    }
    let n = n % 100;
    if n >= 20 {
        out.push(match n % 10 {
            0 => TENS[(n / 10) as usize].into(),
            d => format!("{}-{}", TENS[(n / 10) as usize], ONES[d as usize]),
        });
    } else if n > 0 {
        out.push(ONES[n as usize].into());
    }
}

pub(super) fn cardinal(n: u64) -> String {
    if n == 0 {
        return "zero".into();
    }
    let mut groups = Vec::new();
    let mut m = n;
    while m > 0 {
        groups.push(m % 1000);
        m /= 1000;
    }
    let mut out = Vec::new();
    for (scale, &g) in groups.iter().enumerate().rev() {
        if g == 0 {
            continue;
        }
        below_thousand(g, &mut out);
        if scale > 0 {
            out.push(SCALES[scale].into());
        }
    }
    out.join(" ")
}

pub(super) fn ordinal_words(n: u64) -> String {
    let words = cardinal(n);
    let (head, last) = match words.rfind([' ', '-']) {
        Some(i) => (&words[..=i], &words[i + 1..]),
        None => ("", words.as_str()),
    };
    let last = match last {
        "one" => "first".into(),
        "two" => "second".into(),
        "three" => "third".into(),
        "five" => "fifth".into(),
        "eight" => "eighth".into(),
        "nine" => "ninth".into(),
        "twelve" => "twelfth".into(),
        w if w.ends_with('y') => format!("{}ieth", &w[..w.len() - 1]),
        w => format!("{w}th"),
    };
    format!("{head}{last}")
}

pub(super) fn year(n: u64) -> String {
    let (high, low) = (n / 100, n % 100);
    if high == 0 || (high % 10 == 0 && low < 10) || high >= 100 {
        return cardinal(n);
    }
    let low = match low {
        0 => "hundred".into(),
        l if l < 10 => format!("oh-{}", cardinal(l)),
        l => cardinal(l),
    };
    format!("{} {low}", cardinal(high))
}

pub(super) fn decimal(word: &str) -> Option<String> {
    let (int, frac) = word.split_once('.')?;
    let int: u64 = if int.is_empty() { 0 } else { int.parse().ok()? };
    let frac = frac.trim_end_matches('0');
    if frac.is_empty() {
        return Some(cardinal(int));
    }
    let digits: Vec<String> =
        frac.chars().filter_map(|d| d.to_digit(10)).map(|d| cardinal(d as u64)).collect();
    Some(format!("{} point {}", cardinal(int), digits.join(" ")))
}
