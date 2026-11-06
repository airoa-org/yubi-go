//! Renders representative panel states to PNGs for review without a Quest:
//!
//!     cargo test panel::preview -- --nocapture
//!     open target/panel-preview/*.png

use super::ui::*;
use std::time::Duration;

fn thumb(shade: u8) -> Vec<u8> {
    let (w, h) = (448usize, 252usize);
    let mut out = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let d = (y * w + x) * 3;
            out[d] = (x * 255 / w) as u8 / 3 + shade / 2;
            out[d + 1] = shade;
            out[d + 2] = (y * 255 / h) as u8 / 3 + shade / 3;
        }
    }
    out
}

fn cam_ok() -> Cam {
    Cam { width: 640, height: 480, hz: 29.9, nominal: 30.0, live: true, ..Default::default() }
}

fn base<'a>(thumbs: &'a [Vec<u8>; 3]) -> State<'a> {
    State {
        cams: [cam_ok(), cam_ok(), cam_ok()],
        thumbs: [Some(&thumbs[0]), Some(&thumbs[1]), Some(&thumbs[2])],
        rec_failed: false,
        buttons: Buttons::default(),
        task: Some("sort the dominoes by colour"),
        step: None,
        completed: false,
        jaws: [Jaw::Open(0.43), Jaw::Open(0.76)],
        encoders: [Pose { hz: 98.2, live: true, extrapolated: false }; 2],
        controllers: [Pose { hz: 71.9, live: true, extrapolated: false }; 2],
        hmd: Pose { hz: 71.9, live: true, extrapolated: false },
        battery: Some(Battery {
            pct: 68,
            plugged: false,
            charging: false,
            eta: Some(Eta::Empty(Duration::from_secs(83 * 60))),
        }),
        disk: Some(Disk {
            free: 96_000_000_000,
            used: 40_500_000_000,
            recording_left: Some(Duration::from_secs(4 * 3600 + 19 * 60)),
        }),
        production: true,
        offline_min: None,
        unbound: 0,
        upload: Upload::Idle(String::new()),
        episodes_on_disk: 12,
        queued: 0,
        robot_id: "yubi-go",
        address: Some("198.51.100.123"),
        episodes: &[],
        calib: None,
        refused: &[],
        qr: false,
    }
}

/// The panel as the headset last saw it: every patch pasted in order.
struct Screen(Ui, Vec<u8>);

impl Screen {
    fn draw(&mut self, s: &State) {
        self.0.draw(s).iter().for_each(|p| p.paste(&mut self.1, W));
    }
}

fn save_png(name: &str, ui: &Screen) {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/panel-preview");
    std::fs::create_dir_all(&dir).unwrap();
    use reactive_svg::resvg::tiny_skia;
    let mut pm = tiny_skia::Pixmap::new(W as u32, TEX_H as u32).unwrap();
    for (i, px) in pm.pixels_mut().iter_mut().enumerate() {
        let s = &ui.1[i * 4..][..4];
        let a = s[3] as u32;
        let p = |c: u8| (c as u32 * a / 255) as u8;
        *px = tiny_skia::PremultipliedColorU8::from_rgba(p(s[0]), p(s[1]), p(s[2]), s[3])
            .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
    }
    pm.save_png(dir.join(format!("{name}.png"))).unwrap();
    println!("wrote target/panel-preview/{name}.png");
}

fn ink(ui: &Screen) -> usize {
    ui.1.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count()
}

#[test]
fn preview() {
    let thumbs = [thumb(70), thumb(90), thumb(50)];
    let mut ui = Screen(Ui::default(), vec![0; W * TEX_H * 4]);

    let mut s = base(&thumbs);
    s.buttons = Buttons { recording: true, markable: true };
    s.step = Some("1/15 pick a block of colour [A] and put it in the box on the right");
    ui.draw(&s);
    assert!(ink(&ui) > W * TEX_H / 2, "the panel rendered mostly empty");
    save_png("rec_on", &ui);

    // A take that is already recorded: the band spends its big line on the
    // task, not on saying the take is done — the episode count and the upload
    // row say that, and the operator can act on the task.
    let mut s = base(&thumbs);
    s.completed = true;
    s.step = Some("1/15 pick a block of colour [A] and put it in the box on the right");
    ui.draw(&s);
    save_png("recorded", &ui);

    let mut s = base(&thumbs);
    s.cams[0].live = false;
    s.thumbs[2] = None;
    s.jaws[0] = Jaw::Uncalibrated;
    let refused = [
        "left wrist cam: stalled, replug the hub".to_string(),
        "grippers: needs calibration".to_string(),
        "battery: battery too low".to_string(),
    ];
    s.refused = &refused;
    ui.draw(&s);
    save_png("refused", &ui);

    let mut s = base(&thumbs);
    let reasons = vec![
        "left wrist cam: stalled, replug the hub".to_string(),
        "head cam: no data".to_string(),
        "right controller: rate far too low".to_string(),
        "grippers: needs calibration".to_string(),
    ];
    let episodes = vec![
        Episode {
            name: "episode_1756857242000".to_string(),
            at: "09:14".to_string(),
            task: "sort the dominoes by colour".to_string(),
            bytes: 2_100_000_000,
            secs: Some(42),
            stage: Stage::Waiting,
        },
        Episode {
            name: "episode_1756857100000".to_string(),
            at: "09:11".to_string(),
            task: "pick up the cup".to_string(),
            bytes: 1_800_000_000,
            secs: Some(96),
            stage: Stage::Uploaded,
        },
    ];
    s.unbound = 4;
    s.offline_min = Some(28);
    s.episodes = &episodes;
    s.task = Some(
        "a very long task name that would reach the count at the band's right end, and then some",
    );
    s.refused = &reasons;
    ui.draw(&s);
    save_png("setup", &ui);

    let mut s = base(&thumbs);
    s.episodes = &episodes;
    ui.draw(&s);
    save_png("queue", &ui);

    let mut s = base(&thumbs);
    s.calib =
        Some(Calib { prompt: Some("Close both grippers fully"), held: [0.9, 0.9], messages: &[] });
    ui.draw(&s);
    save_png("calib", &ui);
}

/// More states: refusals, calibration, the QR page and long names.
#[test]
fn preview_more() {
    use crate::misc::calib::{Problem, Prompt};
    let thumbs = [thumb(70), thumb(90), thumb(50)];
    let mut ui = Screen(Ui::default(), vec![0; W * TEX_H * 4]);
    let sorting = |thumbs| State { task: Some("sort the dominoes by colour"), ..base(thumbs) };

    let mut s = sorting(&thumbs);
    s.buttons = Buttons { recording: true, markable: true };
    s.step = Some("1/15 pick a block of colour [A] and put it in the box on the right");
    ui.draw(&s);
    save_png("more_rec_on", &ui);

    let mut s = sorting(&thumbs);
    s.cams[0].live = false;
    s.cams[0].bytes_flowing = true;
    s.thumbs[2] = None;
    s.jaws = [Jaw::Uncalibrated, Jaw::Stopped];
    // As `Reason::shown` words them.
    let refused = [
        "left wrist cam: stalled, replug the hub".to_string(),
        "grippers: needs calibration".to_string(),
        "task: nothing assigned".to_string(),
        "battery: battery too low".to_string(),
        "right controller: not tracked".to_string(),
    ];
    s.refused = &refused;
    s.production = false;
    s.upload = Upload::NoTarget(crate::hub::upload::NO_FILE.into());
    s.address = None;
    ui.draw(&s);
    save_png("more_refused", &ui);

    let mut s = sorting(&thumbs);
    let messages = [Problem::Wrapped(0, 4.0).text(), Problem::Silent(1).text()];
    s.calib =
        Some(Calib { prompt: Some(Prompt::Release.text()), held: [0.9, 0.4], messages: &messages });
    ui.draw(&s);
    save_png("more_calib", &ui);

    let mut s = sorting(&thumbs);
    s.calib = Some(Calib { prompt: None, held: [0.0; 2], messages: &messages });
    ui.draw(&s);
    save_png("more_calib_failed", &ui);

    let mut s = sorting(&thumbs);
    s.qr = true;
    ui.draw(&s);
    save_png("more_qr", &ui);
    s.address = None;
    ui.draw(&s);
    save_png("more_qr_no_network", &ui);

    let mut s = sorting(&thumbs);
    s.task = Some("sort the dominoes by colour, then stack every red one on the blue tray");
    s.robot_id = "yubi-go-rig-number-seven";
    ui.draw(&s);
    save_png("long_names", &ui);

    // A task typed in Japanese still renders: the panel carries a CJK font.
    s.task = Some("ドミノを3色に分けて並べてから、赤いものを全部青いトレーに積み上げる");
    ui.draw(&s);
    save_png("long_names_cjk", &ui);
}
