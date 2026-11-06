//! `cargo build-cwasm [aarch64-linux-android]`: airoa_core at the commits in
//! build/PAYLOAD plus our adapters → componentize-py → AOT cwasm.
//!
//! The component, build/airoa-core.wasm, is tracked with the stamp of its sources, because
//! building it needs read access to the private airoa_data_collection. A clone whose sources
//! match the stamp only runs the AOT step, which needs nothing private.
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn main() -> Result<()> {
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR"));
    let target = std::env::args().nth(1);
    let name = if target.is_some() { "airoa-core-aarch64.cwasm" } else { "airoa-core.cwasm" };
    let out = sdk.join("build").join(name);
    let sidecar = out.with_extension("cwasm.src");
    let pins = std::fs::read_to_string(sdk.join("build/PAYLOAD"))?;
    let wasm = sdk.join("build/airoa-core.wasm");
    let wasm_sidecar = wasm.with_extension("wasm.src");
    let sources = stamp_sources(sdk, &pins)?;
    if !(wasm.exists() && std::fs::read_to_string(&wasm_sidecar).is_ok_and(|s| s == sources)) {
        eprintln!("{} is stale: rebuilding it from airoa_data_collection", wasm.display());
        let work = sdk.join("build/.work");
        let _ = std::fs::remove_dir_all(&work);
        let core = checkout(&work, &pins, "airoa_data_collection", "airoa_core_commit=")
            .map_err(|e| format!("{e}: rebuilding needs read access to airoa_data_collection"))?;
        let metadata = checkout(&work, &pins, "airoa-metadata", "airoa_metadata_commit=")?;
        compile_wasm(sdk, &work, &pins, &core.join("airoa_core/src"), &metadata)?;
        std::fs::write(&wasm_sidecar, &sources)?;
    }
    let stamp = stamp_cwasm(&wasm)?;
    if out.exists() && std::fs::read_to_string(&sidecar).is_ok_and(|s| s == stamp) {
        eprintln!("{} is current", out.display());
        return Ok(());
    }
    std::fs::write(&out, compile_cwasm(&wasm, target.as_deref())?)?;
    Ok(std::fs::write(sidecar, stamp)?)
}

/// What the component is built from: the pins, the locked Python, the WIT and our adapters.
/// It is committed, so it is FNV-1a over sorted paths: `DefaultHasher` and `read_dir`
/// order both differ between machines.
fn stamp_sources(sdk: &Path, pins: &str) -> Result<String> {
    let mut files: Vec<PathBuf> =
        ["uv.lock", "wkg.lock", "wit/core.wit", "wit/python.wit"].map(PathBuf::from).into();
    let mut dirs = vec![PathBuf::from("python")];
    while let Some(dir) = dirs.pop() {
        for entry in std::fs::read_dir(sdk.join(&dir))? {
            let path = dir.join(entry?.file_name());
            match sdk.join(&path).is_dir() {
                true if path.ends_with("__pycache__") => {}
                true => dirs.push(path),
                false => files.push(path),
            }
        }
    }
    files.sort();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut feed = |bytes: &[u8]| {
        for &b in (bytes.len() as u64).to_le_bytes().iter().chain(bytes) {
            hash = (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(pins.as_bytes());
    for file in files {
        feed(file.to_string_lossy().as_bytes());
        feed(&std::fs::read(sdk.join(file))?);
    }
    Ok(format!("{hash:016x}"))
}

/// The component and this builder (wasmtime included). It stays on this machine, so
/// `DefaultHasher` is fine.
fn stamp_cwasm(wasm: &Path) -> Result<String> {
    let mut hasher = std::hash::DefaultHasher::new();
    (std::fs::read(wasm)?, std::fs::read(std::env::current_exe()?)?).hash(&mut hasher);
    Ok(hasher.finish().to_string())
}

/// `repo` at the commit `key` pins, shallow, under `work`.
fn checkout(work: &Path, pins: &str, repo: &str, key: &str) -> Result<PathBuf> {
    let commit = pins.lines().find_map(|l| l.strip_prefix(key)).ok_or(key)?;
    let dir = work.join(repo);
    std::fs::create_dir_all(&dir)?;
    let url = format!("https://github.com/airoa-org/{repo}.git");
    for args in [
        &["init", "-q"][..],
        &["fetch", "-q", "--depth", "1", &url, commit],
        &["checkout", "-q", "FETCH_HEAD"],
    ] {
        run(Command::new("git").arg("-C").arg(&dir).args(args))?;
    }
    Ok(dir)
}

/// Our adapters, airoa_core, airoa-metadata and their wheels, componentized
/// into build/airoa-core.wasm.
fn compile_wasm(sdk: &Path, work: &Path, pins: &str, core: &Path, metadata: &Path) -> Result<()> {
    // uv.lock, hashes included, as a requirements file pip can install from.
    let locked = work.join("requirements.txt");
    let mut export = Command::new("uv");
    export.current_dir(sdk).args(["export", "-q", "--frozen", "--no-dev", "-o"]);
    run(export.arg(&locked))?;
    let wheels = work.join("wheels");
    let mut pip = Command::new("uv");
    pip.args(["pip", "install", "-q", "-r"]).arg(&locked);
    run(pip.arg("--target").arg(&wheels))?;
    // The device gets only the cwasm, so the component reports its own pins.
    let generated = work.join("generated");
    std::fs::create_dir_all(&generated)?;
    std::fs::write(generated.join("airoa_payload.py"), format!("PAYLOAD = {pins:?}\n"))?;
    run(Command::new("wkg").current_dir(sdk).args(["fetch", "wit"]))?;
    let wasm = sdk.join("build/airoa-core.wasm");
    let mut py = Command::new("uv");
    py.current_dir(sdk).args(["run", "-q", "--frozen", "--only-group", "build"]);
    py.args(["componentize-py", "-d"]).arg(sdk.join("wit"));
    py.args(["-w", "python", "componentize"]);
    for path in [&sdk.join("python"), core, metadata, &wheels, &generated] {
        py.arg("-p").arg(path);
    }
    run(py.args(["component", "-o"]).arg(&wasm))
}

/// The wasm ahead-of-time compiled for `target`, or for this host.
fn compile_cwasm(wasm: &Path, target: Option<&str>) -> Result<Vec<u8>> {
    let mut config = airoa_sdk::engine_config();
    if let Some(target) = target {
        config.target(target)?;
    }
    Ok(wasmtime::Engine::new(&config)?.precompile_component(&std::fs::read(wasm)?)?)
}

fn run(cmd: &mut Command) -> Result<()> {
    match cmd.status()?.success() {
        true => Ok(()),
        false => Err(format!("{cmd:?} failed").into()),
    }
}
