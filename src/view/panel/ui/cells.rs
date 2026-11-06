//! One camera cell: the picture, its chrome, and the sensor row under it.

use super::*;
use reactive_svg::image;

/// One camera cell, back to front: the picture, the chrome over it, and the
/// readings that change every tick on their own so the chrome stays cached.
pub(super) fn cell<'a>(out: &mut Vec<Element<'a>>, s: &State<'a>, cell: usize) {
    let cam = s.cams[cell];
    let (cx, cy) = CELL_ORIGIN[cell];
    let (x0, y0) = (cx as f32, cy as f32);
    let ok = cam.live && !cam.starved;
    let colour = if ok { PRIMARY } else { ALERT };
    let video = reactive_svg::Rect { x: cx, y: cy + CELL_LABEL_H, w: THUMB_W, h: THUMB_H };
    if let Some(rgb) = s.thumbs[cell] {
        out.push(image(format!("c{cell}/thumb"), video, rgb));
    }

    let (w, vy, vh) = (CELL_W as f32, video.y as f32, THUMB_H as f32);
    let mut chrome = layout(&format!("c{cell}_bars")).to_string();
    if s.thumbs[cell].is_none() {
        chrome += layout(&format!("c{cell}_nothumb"));
    }
    if !ok {
        chrome += layout(&format!("c{cell}_wash"));
    }
    chrome += &text(x0 + 8.0, y0 + 2.0, colour, cell_label(cell));
    out.push(svg(format!("c{cell}/chrome"), chrome));

    // The camera's own reading, right-aligned on the title row; a short
    // gauge after it while it is live, the fault word instead when not.
    let reading = match cam.live {
        false => {
            let why = match cam.bytes_flowing {
                true => "Stalled, replug",
                false => "No signal",
            };
            text_end(x0 + w - 8.0, y0 + 2.0, ALERT, why)
        }
        true => {
            let line = match cam.nominal > 0.0 {
                true => format!("{}x{} {:.1}/{:.0}", cam.width, cam.height, cam.hz, cam.nominal),
                false => format!("{}x{} {:.1}", cam.width, cam.height, cam.hz),
            };
            text_end(x0 + w - 8.0, y0 + 2.0, colour, &line)
        }
    };
    out.push(svg(format!("c{cell}/reading"), reading));
    out.push(svg(format!("c{cell}/sensors"), sensors(s, cell, x0, vy + vh + 2.0)));
}

/// One reading in a cell's bottom row: an icon, a word, a gauge if it has one,
/// then a number.
struct Sensor {
    icon: &'static str,
    colour: &'static str,
    word: String,
    gauge: Gauge,
    suffix: Option<String>,
}

enum Gauge {
    None,
    Bar(f32),
    /// Drawn inside the battery icon rather than after the word.
    Battery(f32),
}

/// The row under the picture: what is mounted beside this lens.
fn sensors(s: &State, cell: usize, x0: f32, ry: f32) -> String {
    let row = match cell {
        2 => [tracking(s.hmd), battery(s.battery)],
        side => [tracking(s.controllers[side]), jaw(s.jaws[side], s.encoders[side])],
    };
    let mut out = Row::at(x0 + 8.0, ry);
    for r in row {
        out = match r.gauge {
            Gauge::Battery(frac) => out.battery(frac, r.colour),
            _ => out.icon(r.icon, r.colour),
        };
        out = out.text(&r.word, r.colour);
        if let Gauge::Bar(frac) = r.gauge {
            out = out.gap(8.0).meter(84.0, 8.0, frac, r.colour);
        }
        if let Some(suffix) = &r.suffix {
            out = out.gap(8.0).text(suffix, IDLE);
        }
        out = out.gap(18.0);
    }
    out.svg()
}

fn tracking(p: Pose) -> Sensor {
    let (word, colour) = match (p.live, p.extrapolated) {
        (true, true) => ("Not tracked".to_string(), ALERT),
        (true, false) => (format!("Tracked: {:.0}Hz", p.hz), ACCENT),
        (false, _) => ("No data".to_string(), ALERT),
    };
    Sensor { icon: "eye", colour, word, gauge: Gauge::None, suffix: None }
}

fn battery(b: Option<Battery>) -> Sensor {
    let Some(b) = b else {
        return Sensor {
            icon: "battery",
            colour: IDLE,
            word: "Unknown".to_string(),
            gauge: Gauge::None,
            suffix: None,
        };
    };
    let colour = match b.pct {
        p if p <= 10 => ALERT,
        p if p <= 20 => WARN,
        _ => ACCENT,
    };
    let note = match b.eta {
        Some(Eta::Full(d)) => format!(" full in {}", span(d)),
        Some(Eta::Empty(d)) => format!(" {}", span(d)),
        None if b.charging => " charging".to_string(),
        None if b.plugged => " plugged in".to_string(),
        None => String::new(),
    };
    Sensor {
        icon: "battery",
        colour,
        word: format!("{}%{note}", b.pct),
        gauge: Gauge::Battery(b.pct as f32 / 100.0),
        suffix: None,
    }
}

fn jaw(jaw: Jaw, encoder: Pose) -> Sensor {
    let (word, gauge, colour) = match jaw {
        Jaw::Stopped => ("No data", Gauge::None, ALERT),
        Jaw::Uncalibrated => ("Calibrate", Gauge::None, WARN),
        Jaw::Open(f) => ("", Gauge::Bar(f), ACCENT),
    };
    let suffix = encoder.live.then(|| format!("{:.0}Hz", encoder.hz));
    Sensor { icon: "angle", colour, word: word.to_string(), gauge, suffix }
}
