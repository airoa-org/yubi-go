//! The task band: the task, and the step the hands are on.

use super::*;

pub(super) fn band(s: &State) -> String {
    let y0 = BAND_Y as f32;
    let mut out = String::new();

    // A finished take shows in the episode count and the upload row, so a
    // completed task drops its step and the task name takes the large line.
    let second = s.step.filter(|_| !s.completed).map(|step| ("Step: ", step, PRIMARY));
    let (label, name, colour) = match s.task {
        Some(task) => ("Task: ", task, IDLE),
        None => ("", "No task assigned", ALERT),
    };
    let colour = match (second.is_some(), s.task.is_some()) {
        (false, true) => PRIMARY,
        _ => colour,
    };
    let badge = if s.buttons.recording { 128.0 } else { 0.0 };

    match second {
        Some((slabel, value, scolour)) => {
            let top = y0 + 8.0;
            let room = W as f32 - 2.0 * BAND_X - badge - 12.0;
            let name = truncate(name, room - tw(label));
            out += &Row::at(BAND_X, top).text(label, IDLE).text(&name, colour).svg();
            let room = W as f32 - 2.0 * BAND_X;
            let lines = wrap(value, (room - tw(slabel) * 2.0) / 2.0, room / 2.0, 2);
            let y = y0 + if lines.len() > 1 { 36.0 } else { 56.0 };
            let mut lines = lines.into_iter();
            let first = lines.next().unwrap_or("");
            out += &Row::at(BAND_X, y).big(slabel, IDLE).big(first, scolour).svg();
            for (n, line) in lines.enumerate() {
                out += &big(BAND_X, y + (n + 1) as f32 * ROW_H * 2.0, scolour, line);
            }
        }
        None => {
            let top = y0 + (BAND_TASK_H as f32 - ROW_H * 2.0) / 2.0;
            let room = (W as f32 - 2.0 * BAND_X - 12.0) / 2.0;
            let name = truncate(name, room - tw(label));
            out += &Row::at(BAND_X, top).big(label, IDLE).big(&name, colour).svg();
        }
    }
    out
}
