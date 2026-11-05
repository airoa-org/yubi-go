//! The headset's LeRobot export, run on a laptop over takes copied off it by
//! `pull.py` (`docs/lerobot-export.md`).

mod ffmpeg;

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use yubi_go::record::record::{clean_folder, folder_of, META};

const USAGE: &str = "\
usage: yubi-export EPISODES OUT [--folder FOLDER] [--skip-failed] [--skip-untracked] [EPISODE ...]

Writes the episodes in EPISODES (a pull.py copy) as one LeRobot v2.1 dataset
in OUT, which must not exist. Named EPISODEs, or every episode filed under
FOLDER, or every episode. --skip-failed leaves out the failed tries and keeps
the subtasks a failed episode finished. --skip-untracked leaves out the
subtasks during which a hand or the headset lost tracking. Needs ffmpeg with
libx264 on PATH.";

fn main() {
    if let Err(e) = run() {
        eprintln!("yubi-export: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let mut positional = Vec::new();
    let mut folder = None;
    let mut leave = yubi_go::record::export::Leave::default();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--folder" => folder = Some(args.next().context("--folder needs a folder")?),
            "--skip-failed" => leave.failed = true,
            "--skip-untracked" => leave.untracked = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            _ => positional.push(a),
        }
    }
    if positional.len() < 2 {
        bail!("{USAGE}");
    }
    let base = positional.remove(0);
    let out = PathBuf::from(positional.remove(0));
    if out.exists() {
        bail!("{} already exists", out.display());
    }
    let folder = folder.map(|f| clean_folder(&f)).transpose().map_err(anyhow::Error::msg)?;
    let folder = folder.filter(|f| !f.is_empty());
    let takes = takes(Path::new(&base), folder.as_deref(), &positional)?;
    if takes.is_empty() {
        bail!("no finished episodes in {base} match");
    }
    eprintln!("exporting {} episodes", takes.len());

    let part = PathBuf::from(format!("{}.partial", out.display()));
    let _ = std::fs::remove_dir_all(&part);
    let since = std::time::Instant::now();
    let result =
        yubi_go::record::export::export(&takes, &part, leave, &ffmpeg::Ffmpeg, &mut |step| {
            eprint!("\r\x1b[K{step}");
            let _ = std::io::stderr().flush();
        });
    eprintln!();
    let summary = match result {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&part);
            return Err(e);
        }
    };
    std::fs::rename(&part, &out)?;
    for w in &summary.warnings {
        eprintln!("warning: {w}");
    }
    eprintln!(
        "{} LeRobot episodes, {} frames in {:.0?}: {}",
        summary.episodes,
        summary.frames,
        since.elapsed(),
        out.display()
    );
    Ok(())
}

/// The finished takes chosen, oldest first: a take's name is its start in
/// milliseconds, so name order is time order.
fn takes(base: &Path, folder: Option<&str>, named: &[String]) -> anyhow::Result<Vec<PathBuf>> {
    if !named.is_empty() {
        let dirs: Vec<PathBuf> = named.iter().map(|n| base.join(n)).collect();
        if let Some(d) = dirs.iter().find(|d| !d.join(META).is_file()) {
            bail!("{} is not a finished episode", d.display());
        }
        return Ok(dirs);
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(base)
        .with_context(|| format!("cannot read {}", base.display()))?
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|d| d.join(META).is_file())
        .filter(|d| {
            folder.is_none_or(|f| {
                let of = folder_of(d);
                of == f || of.starts_with(&format!("{f}/"))
            })
        })
        .collect();
    dirs.sort();
    Ok(dirs)
}
