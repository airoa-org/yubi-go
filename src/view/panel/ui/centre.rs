//! The centre surface: a running calibration sweep, or why a take was refused.

use super::*;

fn centre_row(n: usize) -> f32 {
    CENTRE_Y as f32 + 20.0 + n as f32 * ROW_H * 1.5
}

pub(super) fn calib_rows(calib: &Calib) -> String {
    let mut out = String::new();
    let x = 28.0;
    let wide = W as f32 - 56.0;
    match calib.prompt {
        Some(p) => {
            out += &text(x, centre_row(0), IDLE, "Calibrating the grippers");
            out += &text(x, centre_row(2), ACCENT, &truncate(p, wide));
            for (i, held) in calib.held.iter().enumerate() {
                let y = centre_row(3 + i);
                let side = match i {
                    0 => "Left",
                    _ => "Right",
                };
                out += &text(x, y, IDLE, side);
                out += &Row::at(x + 88.0, y).meter(180.0, 12.0, *held, ACCENT).svg();
            }
        }
        None => {
            out += &text(x, centre_row(0), ALERT, "Calibration failed");
            out += &text(x, centre_row(2), ACCENT, "Press Y to retry");
        }
    }
    for (i, line) in calib.messages.iter().enumerate() {
        out += &text(x, centre_row(5 + i), ALERT, &truncate(line, wide));
    }
    out
}

pub(super) fn refusal_rows(refused: &[String]) -> String {
    let mut out = text(60.0, 628.0, ALERT, "Cannot start recording");
    out += &text(60.0, 661.0, IDLE, "The rig is not ready");
    let step = ROW_H * 1.5;
    let per_col = (((CENTRE_Y + CENTRE_H) as f32 - 12.0 - centre_row(3)) / step) as usize;
    let cols = [(28.0, 464.0), (520.0, 464.0)];
    let space = per_col * cols.len();
    let overflows = refused.len() > space;
    let shown = if overflows { space.saturating_sub(1) } else { space };
    for (i, reason) in refused.iter().take(shown).enumerate() {
        let (x, wide) = cols[i / per_col];
        out += &text(x, centre_row(3 + i % per_col), ALERT, &truncate(reason, wide));
    }
    if overflows {
        let i = shown;
        let (x, _) = cols[i / per_col];
        let more = refused.len() - shown;
        let more = format!("+{more} more");
        out += &text(x, centre_row(3 + i % per_col), ALERT, &more);
    }
    out
}
