use crate::Error;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, OnceLock};
use wasmtime::component::{Component, Linker, ResourceTable};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{FsPerms, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};
use wasmtime_wasi_http::{WasiHttpCtx, WasiHttpCtxView, WasiHttpView};

wasmtime::component::bindgen!({
    path: "wit/core.wit", world: "core",
});

type Mounts = Vec<(PathBuf, String, Access)>;
static RUNTIME: OnceLock<Runtime> = OnceLock::new();
static PAYLOAD: OnceLock<BTreeMap<String, String>> = OnceLock::new();

/// The build's `key=value` stamp, as the component's `version` reports it:
/// which upstream commits the payload was built from. Empty until `start`,
/// or for a pre-release payload built before the component carried it.
pub fn payload() -> &'static BTreeMap<String, String> {
    PAYLOAD.get_or_init(BTreeMap::new)
}
static PREOPENS: Mutex<Mounts> = Mutex::new(Vec::new());

/// What `core` may do with a preopened directory. Spelled out at the call
/// site because the two are not interchangeable: a read-only mount is the
/// only thing that stops upstream's GC from deleting inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    ReadOnly,
    ReadWrite,
}

impl Access {
    fn perms(self) -> FsPerms {
        match self {
            Access::ReadOnly => FsPerms::ReadOnly,
            Access::ReadWrite => FsPerms::ReadWrite,
        }
    }
}

pub fn add_preopen(host: &Path, guest: &str, access: Access) {
    let mut mounts = PREOPENS.lock().unwrap_or_else(|e| e.into_inner());
    if !mounts.iter().any(|(_, g, _)| g == guest) {
        mounts.push((host.to_owned(), guest.to_owned(), access));
    }
}
pub fn start_with(payload: &Path, preopens: &[(&Path, &str, Access)]) -> Result<(), Error> {
    for (host, guest, access) in preopens {
        add_preopen(host, guest, *access);
    }
    start(payload)
}
/// Only the cwasm reaches the device, so Wasmtime's own version check is the
/// guard: a cwasm from a different wasmtime is refused at load.
pub fn engine_config() -> wasmtime::Config {
    #[cfg_attr(target_os = "android", allow(unused_mut))]
    let mut config = wasmtime::Config::new();
    // Only the host compiles; `opt_level` is not checked when a cwasm loads.
    #[cfg(not(target_os = "android"))]
    config.cranelift_opt_level(wasmtime::OptLevel::Speed);
    config
}
pub const PRECOMPILE: &str = "cargo build-cwasm";
pub fn start(payload: &Path) -> Result<(), Error> {
    static STARTING: Mutex<()> = Mutex::new(());
    let _starting = STARTING.lock().unwrap_or_else(|e| e.into_inner());
    if ready() {
        return Ok(());
    }
    let began = std::time::Instant::now();
    let load = || -> wasmtime::Result<Runtime> {
        let engine = Engine::new(&engine_config())?;
        let compiled = std::fs::read(payload.join("airoa-core.cwasm"))?;
        // Android external storage is noexec: deserialize into anonymous memory.
        let component = unsafe { Component::deserialize(&engine, &compiled) }?;
        Ok(Runtime { engine, component, idle: Mutex::new(Vec::new()) })
    };
    let runtime = load().map_err(|e| Error::Start(format!("{e:#} — run: {PRECOMPILE}")))?;
    let version = runtime
        .invoke("version", json!({}), |_| Err(Error::Python("recorder unavailable".into())))?;
    let _ = PAYLOAD.set(
        version["payload"]
            .as_str()
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    );
    log::info!(
        "airoa_core component startup: {} ms; python {} on {}; core {}",
        began.elapsed().as_millis(),
        version["python"],
        version["platform"],
        self::payload().get("airoa_core_commit").map_or("unknown", String::as_str)
    );
    let _ = RUNTIME.set(runtime);
    Ok(())
}
pub fn ready() -> bool {
    RUNTIME.get().is_some()
}

/// Host callbacks one operation may make before it is cut off, so a guest
/// that loops on its host cannot hang the caller. Chosen by hand.
const HOST_REQUESTS: usize = 32;
/// Idle workers kept for reuse: one for a call, one for the call nested
/// inside it (see `invoke`).
const SPARE_WORKERS: usize = 2;
/// A call slower than this is logged. Chosen by hand.
const SLOW_CALL_MS: u128 = 250;

struct Runtime {
    engine: Engine,
    component: Component,
    idle: Mutex<Vec<Worker>>,
}
struct Worker {
    commands: mpsc::Sender<Command>,
    mounts: Mounts,
}
struct Command {
    op: String,
    args: String,
    events: mpsc::Sender<Event>,
}
enum Event {
    Recorder(Value, mpsc::Sender<Result<String, String>>),
    Done(Result<Value, Error>),
}
impl Runtime {
    fn invoke(
        &self,
        op: &str,
        args: Value,
        mut host: impl FnMut(&Value) -> Result<Value, Error>,
    ) -> Result<Value, Error> {
        if !args.is_object() {
            return Err(Error::Python(format!("{op}: args must be an object")));
        }
        let began = std::time::Instant::now();
        let mounts = PREOPENS.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let cached = {
            let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
            idle.retain(|worker| worker.mounts == mounts);
            idle.pop()
        };
        let worker = match cached {
            Some(w) => w,
            None => self.spawn(mounts)?,
        };
        let (events, receive) = mpsc::channel();
        worker
            .commands
            .send(Command { op: op.into(), args: args.to_string(), events })
            .map_err(broken)?;
        // Callbacks hold no pool lock, because a nested call needs an
        // instance of its own — the Component ABI forbids reentering an
        // active one. That nesting is not hypothetical: closing an episode
        // goes `task` -> host callback -> the writer thread -> a second call
        // for `build_metadata`, once per take. Keeping spare workers is what
        // stops that second call rebuilding a CPython from nothing, which is
        // half a second at the moment the operator has just gestured stop.
        // A failed callback is still answered, so the guest unwinds and its
        // instance stays reusable; the caller gets the host's error.
        let mut failed = None;
        for count in 0..=HOST_REQUESTS {
            match receive.recv().map_err(broken)? {
                Event::Recorder(request, reply) => {
                    if count == HOST_REQUESTS {
                        break;
                    }
                    let answer = host(&request).map(|v| v.to_string()).map_err(|e| {
                        let text = e.to_string();
                        failed.get_or_insert(e);
                        text
                    });
                    let _ = reply.send(answer);
                }
                Event::Done(result) => {
                    if result.is_ok() {
                        let mut idle = self.idle.lock().unwrap_or_else(|e| e.into_inner());
                        if idle.len() < SPARE_WORKERS {
                            idle.push(worker);
                        }
                    }
                    let ms = began.elapsed().as_millis();
                    if ms >= SLOW_CALL_MS {
                        log::warn!("airoa_core {op} took {ms} ms");
                    }
                    return match failed {
                        Some(e) => Err(e),
                        None => result,
                    };
                }
            }
        }
        Err(Error::Python(format!("{op}: exceeded {HOST_REQUESTS} host requests")))
    }
    fn spawn(&self, mounts: Mounts) -> Result<Worker, Error> {
        let engine = self.engine.clone();
        let component = self.component.clone();
        let thread_mounts = mounts.clone();
        let (commands, receive) = mpsc::channel::<Command>();
        std::thread::Builder::new()
            .name("airoa-component".into())
            .spawn(move || {
                let instantiate = || -> wasmtime::Result<_> {
                    let mut wasi = WasiCtxBuilder::new();
                    wasi.stderr(std::io::stderr());
                    wasi.stdout(std::io::stderr());
                    for (host, guest, access) in &thread_mounts {
                        wasi.preopened_dir(host, guest, access.perms())?;
                    }
                    let state = State {
                        wasi: wasi.build(),
                        table: ResourceTable::new(),
                        http: WasiHttpCtx::new(),
                        events: None,
                    };
                    let mut store = Store::new(&engine, state);
                    let mut linker = Linker::new(&engine);
                    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;
                    wasmtime_wasi_http::p2::add_only_http_to_linker_sync(&mut linker)?;
                    airoa::core::recorder::add_to_linker::<_, wasmtime::component::HasSelf<_>>(
                        &mut linker,
                        |s| s,
                    )?;
                    let bindings = Core::instantiate(&mut store, &component, &linker)?;
                    Ok((store, bindings))
                };
                let (mut store, bindings) = match instantiate() {
                    Ok(instance) => instance,
                    Err(e) => {
                        if let Ok(command) = receive.recv() {
                            let _ = command
                                .events
                                .send(Event::Done(Err(Error::Start(format!("component: {e:#}")))));
                        }
                        return;
                    }
                };
                while let Ok(command) = receive.recv() {
                    store.data_mut().events = Some(command.events.clone());
                    let result =
                        match bindings.call_dispatch(&mut store, &command.op, &command.args) {
                            Ok(Ok(value)) => serde_json::from_str(&value).map_err(broken),
                            Ok(Err(Failure::Refused(message))) => Err(Error::Refused(message)),
                            Ok(Err(Failure::Python(message))) => Err(Error::Python(message)),
                            Err(e) => Err(Error::Python(format!("component trap: {e:#}"))),
                        };
                    store.data_mut().events = None;
                    if command.events.send(Event::Done(result)).is_err() {
                        break;
                    }
                }
            })
            .map_err(broken)?;
        Ok(Worker { commands, mounts })
    }
}

fn broken(e: impl std::fmt::Display) -> Error {
    Error::Python(e.to_string())
}
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    http: WasiHttpCtx,
    events: Option<mpsc::Sender<Event>>,
}
impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}
impl WasiHttpView for State {
    fn http(&mut self) -> WasiHttpCtxView<'_> {
        WasiHttpCtxView { ctx: &mut self.http, table: &mut self.table, hooks: Default::default() }
    }
}
impl airoa::core::recorder::Host for State {
    fn invoke(&mut self, request: String) -> Result<String, String> {
        let request = serde_json::from_str(&request).map_err(|e| e.to_string())?;
        let (reply, receive) = mpsc::channel();
        self.events
            .as_ref()
            .ok_or("no active operation")?
            .send(Event::Recorder(request, reply))
            .map_err(|e| e.to_string())?;
        receive.recv().map_err(|e| e.to_string())?
    }
}
/// Operations run once; callbacks may use the SDK on any thread.
/// Already completed I/O is not rolled back on failure.
pub fn call_with_host(
    op: &str,
    args: Value,
    host: impl FnMut(&Value) -> Result<Value, Error>,
) -> Result<Value, Error> {
    RUNTIME.get().ok_or(Error::NotStarted)?.invoke(op, args, host)
}
pub fn call(op: &str, args: Value) -> Result<Value, Error> {
    call_with_host(op, args, |_| Err(Error::Python("recorder unavailable".into())))
}
pub(crate) fn call_str(op: &str, args: Value) -> Result<String, Error> {
    let value = call(op, args)?;
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Python(format!("{op}: expected string, got {value}")))
}
