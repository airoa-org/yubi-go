//! The SVG primitives the panel is drawn from: one shape, or one run of text,
//! each in the panel's own coordinate space. Nothing here knows what a rig is.

use super::svg::esc;
use super::ui::{BASE, PANEL_QR, ROW_H, SCRIM, TRACK};

pub(super) fn text(x: f32, top: f32, colour: &str, s: &str) -> String {
    format!(r#"<text x="{x:.0}" y="{:.0}" fill="{colour}">{}</text>"#, top + BASE, esc(s))
}

pub(super) fn text_end(x: f32, top: f32, colour: &str, s: &str) -> String {
    format!(
        r#"<text x="{x:.0}" y="{:.0}" text-anchor="end" fill="{colour}">{}</text>"#,
        top + BASE,
        esc(s)
    )
}

pub(super) fn big(x: f32, top: f32, colour: &str, s: &str) -> String {
    format!(
        r#"<text x="{x:.0}" y="{:.0}" class="big" fill="{colour}">{}</text>"#,
        top + 32.0,
        esc(s)
    )
}

pub(super) fn qr_page(address: Option<&str>) -> String {
    let (x0, y0, w, h) = PANEL_QR;
    let (x0, y0, w, h) = (x0 as f32, y0 as f32, w as f32, h as f32);
    let mut out = format!(
        r##"<rect x="{x0:.0}" y="{y0:.0}" width="{w:.0}" height="{h:.0}" fill="#ffffff"/>"##
    );
    let Some(ip) = address else {
        let none = "No network";
        out += &text(x0 + (w - tw(none)) / 2.0, y0 + (h - ROW_H) / 2.0, SCRIM, none);
        return out;
    };
    let url = format!("http://{ip}:8080");
    let side = w - 176.0;
    out += &qr(x0 + (w - side) / 2.0, y0 + 56.0, side, &url);
    out += &text(x0 + (w - tw(&url)) / 2.0, y0 + h - 76.0, SCRIM, &url);
    out
}

pub(super) fn qr(x0: f32, y0: f32, side: f32, data: &str) -> String {
    let Ok(code) = qrcodegen::QrCode::encode_text(data, qrcodegen::QrCodeEcc::Medium) else {
        return String::new();
    };
    let n = code.size();
    let cell = side / n as f32;
    let mut out = format!(
        r##"<rect x="{:.0}" y="{:.0}" width="{:.0}" height="{:.0}" fill="#ffffff"/>"##,
        x0 - 6.0,
        y0 - 6.0,
        side + 12.0,
        side + 12.0,
    );
    let mut path = String::new();
    for y in 0..n {
        for x in 0..n {
            if code.get_module(x, y) {
                path += &format!(
                    "M{:.2} {:.2}h{cell:.2}v{cell:.2}h-{cell:.2}z ",
                    x0 + x as f32 * cell,
                    y0 + y as f32 * cell,
                );
            }
        }
    }
    out += &format!(r#"<path d="{path}" fill="{SCRIM}"/>"#);
    out
}

/// One line of the panel, laid out left to right from `x` with its text top
/// at `top`. Each part moves the cursor past itself, and every part centres on
/// the text line, so no caller adds widths or nudges an icon by hand.
pub(super) struct Row {
    x: f32,
    top: f32,
    svg: String,
}

/// The icons are 28 px squares, and `alert`, `ok` and `rec` are 24 px ones
/// that centre in the same slot, so text after any icon starts in one column.
const ICON_SLOT: f32 = 28.0;
const ICON_GAP: f32 = 6.0;

impl Row {
    pub(super) fn at(x: f32, top: f32) -> Self {
        Self { x, top, svg: String::new() }
    }

    /// Where the next part would start.
    pub(super) fn end(&self) -> f32 {
        self.x
    }

    pub(super) fn svg(self) -> String {
        self.svg
    }

    pub(super) fn gap(mut self, px: f32) -> Self {
        self.x += px;
        self
    }

    pub(super) fn text(mut self, s: &str, colour: &str) -> Self {
        self.svg += &text(self.x, self.top, colour, s);
        self.x += tw(s);
        self
    }

    /// The 36 px face, from the same top.
    pub(super) fn big(mut self, s: &str, colour: &str) -> Self {
        self.svg += &big(self.x, self.top, colour, s);
        self.x += tw(s) * 2.0;
        self
    }

    pub(super) fn icon(mut self, name: &str, colour: &str) -> Self {
        let side = if matches!(name, "alert" | "ok" | "rec") { 24.0 } else { ICON_SLOT };
        let (x, y) = (self.x + (ICON_SLOT - side) / 2.0, self.middle() - side / 2.0);
        self.svg +=
            &format!(r##"<use href="#icon_{name}" x="{x:.0}" y="{y:.0}" color="{colour}"/>"##);
        self.x += ICON_SLOT + ICON_GAP;
        self
    }

    /// The 40x20 battery icon, filled to `frac`.
    pub(super) fn battery(mut self, frac: f32, colour: &str) -> Self {
        let (x, y) = (self.x, self.middle() - 10.0);
        let fill = (27.0 * frac.clamp(0.0, 1.0)).round();
        self.svg += &format!(
            r##"<use href="#icon_battery" x="{x:.0}" y="{y:.0}" color="{colour}"/><rect x="{:.0}" y="{:.0}" width="{fill:.0}" height="10" fill="{colour}"/>"##,
            x + 4.0,
            y + 5.0,
        );
        self.x += 40.0 + ICON_GAP;
        self
    }

    /// A controller button: its letter in a 22 px ring.
    pub(super) fn key(mut self, letter: &str, colour: &str) -> Self {
        let (cx, cy) = (self.x + 11.0, self.middle());
        self.svg += &format!(
            r#"<circle cx="{cx:.0}" cy="{cy:.0}" r="11" fill="none" stroke="{colour}" stroke-width="2"/><text x="{cx:.0}" y="{:.0}" text-anchor="middle" fill="{colour}" font-size="14px">{}</text>"#,
            cy + 5.0,
            esc(letter),
        );
        self.x += 30.0;
        self
    }

    pub(super) fn meter(mut self, w: f32, h: f32, frac: f32, colour: &str) -> Self {
        let (x, y) = (self.x, self.middle() - h / 2.0);
        let fill = (w * frac.clamp(0.0, 1.0)).round();
        self.svg += &format!(
            r#"<rect x="{x:.0}" y="{y:.0}" width="{w:.0}" height="{h:.0}" fill="{TRACK}"/><rect x="{x:.0}" y="{y:.0}" width="{fill:.0}" height="{h:.0}" fill="{colour}"/>"#
        );
        self.x += w;
        self
    }

    fn middle(&self) -> f32 {
        self.top + ROW_H / 2.0
    }
}

/// Break on spaces into at most `lines` rows of `first`/`rest` px.
pub(super) fn wrap(s: &str, first: f32, rest: f32, lines: usize) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::new();
    let mut left = s;
    while !left.is_empty() && out.len() < lines {
        let room = if out.is_empty() { first } else { rest };
        let cut = cut(left, room);
        if out.len() + 1 == lines || cut.len() == left.len() {
            out.push(cut);
            break;
        }
        let at = cut.rfind(' ').filter(|&i| tw(&left[..i]) > room * 0.55).unwrap_or(cut.len());
        out.push(left[..at].trim_end());
        left = left[at..].trim_start();
    }
    out
}

/// Estimated advance in px: the mono face is 0.6 em (10.8 at 18 px) and the
/// CJK face a full em. Used for icon runs and truncation, never for
/// alignment SVG can do itself (`text-anchor="end"`).
pub(super) fn tw(s: &str) -> f32 {
    s.chars().map(|c| if c.is_ascii() { 10.8 } else { 18.0 }).sum()
}

/// `s` cut to `px`, with an ellipsis in place of what did not fit. ASCII text
/// gets "...": the mono face is subset to ASCII, so "…" would pull the whole
/// run into the proportional CJK face.
pub(super) fn truncate(s: &str, px: f32) -> std::borrow::Cow<'_, str> {
    if tw(s) <= px {
        return s.into();
    }
    let dots = if s.is_ascii() { "..." } else { "…" };
    format!("{}{dots}", cut(s, px - tw(dots)).trim_end()).into()
}

fn cut(s: &str, px: f32) -> &str {
    let mut used = 0.0;
    for (i, c) in s.char_indices() {
        used += if c.is_ascii() { 10.8 } else { 18.0 };
        if used > px {
            return &s[..i];
        }
    }
    s
}
