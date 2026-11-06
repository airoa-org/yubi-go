//! The status block under the band: storage, identity, network and uploads,
//! and the button hints.

use super::*;

fn status_top() -> f32 {
    (BAND_Y + BAND_TASK_H) as f32 + 4.0
}

pub(super) fn storage(s: &State) -> String {
    let top = status_top();
    let mut out = String::new();

    if let Some(disk) = s.disk {
        let (line, colour) = match disk.recording_left {
            None => ("No room to record".to_string(), ALERT),
            Some(d) => {
                let colour = if d < Duration::from_secs(600) { WARN } else { ACCENT };
                let left = span(d);
                (format!("{left} to record"), colour)
            }
        };
        out += &Row::at(BAND_X, top).icon("disk", colour).text(&line, colour).svg();
    }

    let bar_y = top + 24.0;
    match s.disk {
        Some(d) => {
            let (used, free) = (gb(d.used), gb(d.free));
            let line = format!("{used} / {free} free");
            out += &text(BAND_X, bar_y, IDLE, &line);
        }
        None => out += &text(BAND_X, bar_y, IDLE, "Storage unknown"),
    }
    out
}

fn upload_colour(s: &State) -> (String, Level, &'static str) {
    let (_, level) = s.upload.verdict();
    let sentence = match &s.upload {
        Upload::NoTarget(why) => format!("Not uploading: {why}"),
        Upload::Unreachable(why) => {
            format!("Bucket unreachable: {why}")
        }
        Upload::Retrying(e) => format!("Retry failed: {e}"),
        _ => String::new(),
    };
    let colour = match (&s.upload, level) {
        (Upload::NoTarget(why), _) if why == NO_FILE => IDLE,
        (_, Level::Ok) => ACCENT,
        (_, Level::Warn) => WARN,
        (_, Level::Bad) => ALERT,
    };
    (sentence, level, colour)
}

pub(super) fn net(s: &State) -> String {
    let net_y = status_top();
    let (sentence, level, colour) = upload_colour(s);
    let mut head = Row::at(NET_X, net_y + 3.0)
        .icon("cloud", colour)
        .text(&s.upload.word(s.queued), colour)
        .gap(16.0)
        .text(&format!("{} eps", s.episodes_on_disk), IDLE);
    if s.unbound > 0 {
        // A gap, not leading spaces: SVG drops a text run's leading whitespace.
        let unbound = format!("unbound {}", s.unbound);
        head = head.gap(tw("  ")).text(&unbound, WARN);
    }
    let mut out = head.svg();

    let offline = s.offline_min.filter(|_| s.production);
    let alerting = offline.is_some() || level != Level::Ok;
    let queue_y = net_y + 26.0;
    let qmax = if alerting { 2 } else { 3 };
    for (i, e) in s.episodes.iter().take(qmax).enumerate() {
        let y = queue_y + i as f32 * 21.0;
        let (badge, colour) = match e.stage {
            Stage::Uploaded => ("ok", ACCENT),
            Stage::Recording => ("rec", ALERT),
            Stage::Waiting => ("clock", IDLE),
        };
        let took = e.secs.map(|secs| match secs {
            t if t < 60 => format!(" {t}s"),
            t => format!(" {}m{}s", t / 60, t % 60),
        });
        let tail = format!("{}{}", gb(e.bytes), took.unwrap_or_default());
        let head = match e.task.is_empty() {
            true => e.name.clone(),
            false => format!("{} {}", e.at, e.task),
        };
        let row = Row::at(NET_X, y).icon(badge, colour);
        // 12 px between the name and the size after it.
        let room = W as f32 - BAND_X - tw(&tail) - 12.0 - row.end();
        out += &row.text(&truncate(&head, room), IDLE).svg();
        out += &text_end(W as f32 - BAND_X, y, IDLE, &tail);
    }
    if alerting {
        let mut row = Row::at(NET_X, queue_y + qmax as f32 * 21.0).icon("alert", WARN);
        if let Some(mins) = offline {
            let down = "Backend down";
            row = row.text(&format!("{down}{}", ago(mins)), WARN).gap(tw("  "));
        }
        if level != Level::Ok {
            let line = sentence.lines().next().unwrap_or("").trim();
            let room = W as f32 - BAND_X - row.end();
            row = row.text(&truncate(line, room), colour);
        }
        out += &row.svg();
    }
    out
}

pub(super) fn id(s: &State) -> String {
    let mut out = String::new();
    let (ix, iy, _, _) = PANEL_ID;
    let (ix, iy) = (ix as f32, iy as f32);
    let mode_word = match s.production {
        true => "Production",
        false => "Standalone",
    };
    match s.address {
        Some(ip) => {
            // The longest IPv4 (15 chars, 162 px) must fit right of the QR.
            out += &qr(ix + 12.0, iy + 17.0, 80.0, &format!("http://{ip}:8080"));
            let tx = ix + 106.0;
            out += &text(tx, iy + 8.0, IDLE, mode_word);
            out += &text(tx, iy + 32.0, PRIMARY, &truncate(s.robot_id, 140.0));
            out += &text(tx, iy + 56.0, ACCENT, ip);
            out += &text(tx, iy + 80.0, IDLE, ":8080");
        }
        None => {
            let row = Row::at(ix + 14.0, iy + 12.0).icon("wifi_off", ALERT);
            out += &row.text("No network", ALERT).svg();
            out += &text(ix + 14.0, iy + 44.0, IDLE, mode_word);
            out += &text(ix + 14.0, iy + 68.0, PRIMARY, &truncate(s.robot_id, 240.0));
        }
    }
    out
}

pub(super) fn hints(s: &State) -> String {
    let top = (BAND_Y + 254) as f32 + 10.0;
    let keys = |x: f32, keys: &[(&str, &str, bool)]| {
        let mut row = Row::at(x, top);
        for (i, &(key, what, on)) in keys.iter().enumerate() {
            if i > 0 {
                row = row.gap(33.0);
            }
            row = row.key(key, if on { PRIMARY } else { TRACK });
            row = row.text(what, if on { IDLE } else { TRACK });
        }
        row
    };
    let left = [("Y", "Calibrate", true)];
    let right = [
        ("A", if s.buttons.recording { "Stop" } else { "Record" }, true),
        ("B", if s.buttons.markable { "Failed" } else { "Battery" }, true),
    ];
    let width = keys(0.0, &right).end();
    keys(BAND_X, &left).svg() + &keys(W as f32 - BAND_X - width, &right).svg()
}

#[cfg(test)]
mod fit {
    use super::*;

    fn elements(fragment: &str) -> Vec<(f32, f32)> {
        let mut out = Vec::new();
        let mut rest = fragment;
        while let Some(at) = rest.find(" y=\"") {
            let tail = &rest[at + 4..];
            let end = tail.find('"').unwrap();
            let y: f32 = tail[..end].parse().unwrap_or(0.0);
            let xat = rest[..at].rfind(" x=\"").map(|i| {
                let t = &rest[i + 4..];
                t[..t.find('"').unwrap()].parse().unwrap_or(0.0)
            });
            let h = tail
                .find("height=\"")
                .filter(|&i| i < 60)
                .map(|i| {
                    let t = &tail[i + 8..];
                    t[..t.find('"').unwrap()].parse().unwrap_or(0.0)
                })
                .unwrap_or(0.0f32);
            if let Some(x) = xat {
                out.push((x, y + h.max(6.0)));
            }
            rest = &rest[at + 4..];
        }
        out
    }

    fn crowded_episodes() -> Vec<Episode> {
        (0..3)
            .map(|i| Episode {
                name: format!("episode_{i}"),
                at: "09:14".into(),
                task: "長いタスク名のサンプルをここに".into(),
                bytes: 2_100_000_000,
                secs: Some(42),
                stage: Stage::Waiting,
            })
            .collect()
    }

    /// The status block at its most crowded: every alert, long counts and a
    /// long upload error.
    fn crowded(episodes: &[Episode]) -> State<'_> {
        State {
            cams: [Cam::default(); 3],
            thumbs: [None; 3],
            rec_failed: false,
            buttons: Buttons::default(),
            task: None,
            step: None,
            completed: false,
            jaws: [Jaw::Uncalibrated, Jaw::Uncalibrated],
            encoders: [Pose::default(); 2],
            controllers: [Pose::default(); 2],
            hmd: Pose::default(),
            battery: None,
            disk: Some(Disk {
                free: 96_000_000_000,
                used: 40_000_000_000,
                recording_left: Some(std::time::Duration::from_secs(600)),
            }),
            production: true,
            offline_min: Some(28),
            unbound: 4,
            upload: Upload::Retrying(
                "meta.json: 502 Bad Gateway and then some very long text".into(),
            ),
            episodes_on_disk: 9999,
            queued: 12,
            robot_id: "yubi-go",
            address: Some("198.51.100.41"),
            episodes,
            calib: None,
            refused: &[],
            qr: false,
        }
    }

    #[test]
    fn the_status_block_stays_inside_its_panels() {
        let episodes = crowded_episodes();
        {
            let s = crowded(&episodes);
            let fragment = [storage(&s), net(&s), id(&s), hints(&s)].concat();
            let (_, sy, sw, sh) = PANEL_STORAGE;
            let (nx, ny, _, nh) = PANEL_NET;
            let (_, hy, _, hh) = PANEL_HINTS;
            for (x, bottom) in elements(&fragment) {
                let y_top = bottom - 22.0;
                if y_top >= hy as f32 - 6.0 {
                    assert!(bottom <= (hy + hh) as f32 + 1.0, "hint element runs to {bottom}");
                    continue;
                }
                if x < sw as f32 {
                    assert!(
                        bottom <= (sy + sh) as f32 + 1.0,
                        "storage element at x={x} runs to {bottom}, panel ends {}",
                        sy + sh
                    );
                }
                if x >= nx as f32 {
                    assert!(
                        bottom <= (ny + nh) as f32 + 1.0,
                        "network element at x={x} runs to {bottom}, panel ends {}",
                        ny + nh
                    );
                }
            }
        }
    }

    /// Every run of text as (left, right, top, text), using `tw` for the width
    /// and honouring `text-anchor="end"`.
    fn runs(fragment: &str) -> Vec<(f32, f32, f32, String)> {
        let attr = |tag: &str, key: &str| -> Option<f32> {
            let at = tag.find(&format!(" {key}=\""))? + key.len() + 3;
            tag[at..].split('"').next()?.parse().ok()
        };
        let mut out = Vec::new();
        for piece in fragment.split("<text").skip(1) {
            let (tag, rest) = piece.split_once('>').unwrap();
            let content = rest.split("</text>").next().unwrap().replace("&amp;", "&");
            let (x, y) = (attr(tag, "x").unwrap_or(0.0), attr(tag, "y").unwrap_or(0.0));
            let w = tw(&content) * if tag.contains("class=\"big\"") { 2.0 } else { 1.0 };
            let left = if tag.contains("text-anchor=\"end\"") { x - w } else { x };
            out.push((left, left + w, y, content));
        }
        out
    }

    /// Text must stay inside the panel it starts in, and no two runs on one
    /// line may overlap.
    #[test]
    fn no_text_runs_out_of_its_panel_or_into_its_neighbour() {
        let episodes = crowded_episodes();
        {
            let mut s = crowded(&episodes);
            s.buttons = Buttons { recording: true, markable: true };
            for (panel, fragment) in [
                (PANEL_STORAGE, storage(&s)),
                (PANEL_ID, id(&s)),
                (PANEL_NET, net(&s)),
                (PANEL_HINTS, hints(&s)),
            ] {
                let (px, _, pw, _) = panel;
                let mut runs = runs(&fragment);
                for (left, right, _, text) in &runs {
                    assert!(
                        *left >= px as f32 && *right <= (px + pw) as f32 + 1.0,
                        "{text:?} spans {left:.0}..{right:.0}, panel is {px}..{}",
                        px + pw
                    );
                }
                runs.sort_by(|a, b| (a.2, a.0).partial_cmp(&(b.2, b.0)).unwrap());
                for pair in runs.windows(2) {
                    let ((_, right, y, a), (left, _, y2, b)) = (&pair[0], &pair[1]);
                    if y == y2 {
                        // 1 px, because the panel writes x rounded to a whole pixel.
                        assert!(*right <= left + 1.0, "{a:?} runs into {b:?}");
                    }
                }
            }
        }
    }
}
