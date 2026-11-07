//! Extensions built as WebAssembly components, run by wasmtime: the host side of the contract in
//! `crates/ext/wit/extension.wit`.
//!
//! Loading compiles a component, in an engine of its own, instantiates it once and asks who it is (`describe`) and
//! what it registers (`register`); what it registers becomes the same types, fields, operations, types and
//! views a linked extension's would, each callback a call into the component by its handle.
//!
//! A guest's memory only grows, so an instance holds the most it ever held until it is dropped: instances live by the
//! lifetime of their job, never a timer. A job - an operation's run, a tool's run - gets an instance made for it and
//! dropped when it returns. Readings - a family's members, a field's reading, a sniff - share one instance,
//! reused while it is used and reclaimed after
//! `READING_FRAMES` frames unused (the host counts frames: `frame`). Long-running jobs keep theirs: the views live in
//! one instance, which goes when its last view closes and it answers that it may be reclaimed; an extraction session in
//! one of its own, which goes with the session. Making one is cheap and dropping one returns its memory. An instance that
//! traps is dropped, not reused.
//!
//! What a call lends - an asset, a frame's UI - the host keeps by pointer in the instance's resource table for the
//! call alone, and takes back before the call returns; a row a view opens is closed before its frame is drawn on again.
//! An extension sees no files, clock, randomness or network of the host's: the WASI its standard library imports is
//! linked with nothing behind it (fixed clocks, no environment, standard error kept to report a failure). A call has a
//! deadline (the engine's epoch, ticking every 10 ms) and an instance a memory cap.
//!
//! A call blocks the thread making it until the component answers: it is made from ordinary code or a runtime's
//! blocking thread (`spawn_blocking`), never from a thread driving an async runtime - tokio refuses to block one.
//!
//! A component's compiled code is kept in the user's cache (`compiled`), by the hash of its bytes and of the engine's
//! settings, so only the first process to load a component compiles it.

use anyhow::{Context, Result, anyhow};
use pe2_ext::{Asset, Declared, Discs, Extraction, Field, Image, Name, Project, Registry, Source, Table, UiHost};
use pe2_ext::{Value, View};
use std::path::Path;
use std::sync::Weak;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use wasmtime::component::{Accessor, Component, HasSelf, Linker, Resource, ResourceAny, ResourceTable};
use wasmtime::{Config, Engine, ResourceLimiter, Store};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{HostMonotonicClock, HostWallClock, WasiCtx, WasiCtxView, WasiView};

use super::{Loadable, Origin};

mod bindings {
    wasmtime::component::bindgen!({
        world: "linked",
        path: "../ext/wit",
        imports: { default: trappable },
        exports: { default: async | store },
        with: {
            "pe2:extension/host.asset": super::LentAsset,
            "pe2:extension/host.project": super::LentProject,
            "pe2:extension/host.ui": super::Frame,
            "pe2:extension/host.texture": super::HostTexture,
            "pe2:extension/host.declarations": super::LentDeclarations,
            "pe2:extension/host.discs": super::LentDiscs,
            "pe2:extension/host.writer": super::HostWriter,
            "pe2:extension/media.media-buffer": super::HostMediaBuffer,
            "pe2:extension/types.stream-reader": super::HostStreamReader,
            "pe2:extension/unsandboxed.process": super::HostProcess,
        },
    });
}

use bindings::exports::pe2::extension::guest as g;
use bindings::pe2::extension::{host, media, types, unsandboxed};

/// The engine's epoch ticks this often: deadlines are counted in its ticks.
const TICK: Duration = Duration::from_millis(10);
/// A call that answers a question (has a picture, its looks, a field's reading).
const QUICK: u64 = 100;
/// A frame of a view (whose first frame may decode its picture).
const FRAME: u64 = 200;
/// A picture or a thumbnail, decoded.
const LONG: u64 = 1000;
/// An unsandboxed extension's operation, which may wait on a long-running program: an hour.
const UNSANDBOXED_LONG: u64 = 360_000;
/// An extraction session's beginning (reading every declaration it needs) or one asset's extraction.
const EXTRACTION: u64 = 6000;
/// An instance's memory, at most, unless its extension declares more for its job (`Registry::memory`).
const MEMORY: u64 = 256 << 20;
/// The most any job may be given: a 32-bit memory's whole range.
const MOST_MEMORY: u64 = 4 << 30;
/// A reading instance grown past this after a call is dropped, not kept: its memory goes back, as a guest's never
/// shrinks.
const RESIDENT: usize = 64 << 20;
/// How many frames a reading instance stays unused before it is reclaimed.
const READING_FRAMES: u32 = 60;
/// An extension's engine, deterministic - NaNs canonical, relaxed SIMD deterministic, an epoch for deadlines. Instances are
/// made as they are asked for and let go of whole: how many there are is what is running, and no limit stands in for that.
/// A memory starts as a copy-on-write view of the component's image, which is what makes one cheap.
fn new_engine() -> Result<Engine> {
    let mut config = Config::new();
    config.wasm_component_model_async(true);
    config.epoch_interruption(true);
    config.cranelift_nan_canonicalization(true);
    config.relaxed_simd_deterministic(true);
    let engine = Engine::new(&config).map_err(anyhow::Error::from)?;
    tick(&engine);
    Ok(engine)
}

/// Tick an engine's epoch with every other's, on one thread, while it lives.
fn tick(engine: &Engine) {
    static TICKED: OnceLock<Mutex<Vec<wasmtime::EngineWeak>>> = OnceLock::new();
    let started = TICKED.get().is_some();
    let engines = TICKED.get_or_init(Mutex::default);
    if let Ok(mut engines) = engines.lock() {
        engines.push(engine.weak());
    }
    if !started {
        std::thread::spawn(move || {
            loop {
                if let Ok(mut engines) = engines.lock() {
                    engines.retain(|weak| weak.upgrade().inspect(Engine::increment_epoch).is_some());
                }
                std::thread::sleep(TICK);
            }
        });
    }
}

/// A clock that never moves: an extension's output may not depend on time.
struct Still;

impl HostWallClock for Still {
    fn resolution(&self) -> Duration {
        Duration::from_secs(1)
    }

    fn now(&self) -> Duration {
        Duration::ZERO
    }
}

impl HostMonotonicClock for Still {
    fn resolution(&self) -> u64 {
        1
    }

    fn now(&self) -> u64 {
        0
    }
}

/// A lent asset's pointer, its lifetime erased to keep it in a resource table.
///
/// # Safety
/// It must be taken back before what it points to goes.
#[expect(clippy::transmute_ptr_to_ptr, reason = "a cast cannot extend a trait object's lifetime")]
unsafe fn erase_source<'a>(source: *const (dyn Source + 'a)) -> *const (dyn Source + 'static) {
    // SAFETY: the same pointer, its lifetime the caller's to keep.
    unsafe { std::mem::transmute(source) }
}

/// A lent frame's pointer, its lifetime erased to keep it in a resource table.
///
/// # Safety
/// It must be taken back before what it points to goes.
#[expect(clippy::transmute_ptr_to_ptr, reason = "a cast cannot extend a trait object's lifetime")]
unsafe fn erase_frame<'a>(host: *mut (dyn UiHost + 'a)) -> *mut (dyn UiHost + 'static) {
    // SAFETY: the same pointer, its lifetime the caller's to keep.
    unsafe { std::mem::transmute(host) }
}

/// A lent project's pointer, its lifetime erased to keep it in a resource table.
///
/// # Safety
/// It must be taken back before what it points to goes.
#[expect(clippy::transmute_ptr_to_ptr, reason = "a cast cannot extend a trait object's lifetime")]
unsafe fn erase_project<'a>(project: *const (dyn Project + 'a)) -> *const (dyn Project + 'static) {
    // SAFETY: the same pointer, its lifetime the caller's to keep.
    unsafe { std::mem::transmute(project) }
}

/// A frame's row, its lifetime erased to keep it in a resource table.
///
/// # Safety
/// It must be dropped before the frame it borrows.
unsafe fn erase_row<'a>(row: Box<dyn UiHost + 'a>) -> Box<dyn UiHost + 'static> {
    // SAFETY: the same box, its lifetime the caller's to keep.
    unsafe { std::mem::transmute(row) }
}

/// Lent declarations' pointer, its lifetime erased to keep it in a resource table.
///
/// # Safety
/// It must be taken back before what it points to goes.
#[expect(clippy::transmute_ptr_to_ptr, reason = "a cast cannot extend a trait object's lifetime")]
unsafe fn erase_declared<'a>(declared: *const (dyn Declared + 'a)) -> *const (dyn Declared + 'static) {
    // SAFETY: the same pointer, its lifetime the caller's to keep.
    unsafe { std::mem::transmute(declared) }
}

/// Lent discs' pointer, its lifetime erased to keep it in a resource table.
///
/// # Safety
/// It must be taken back before what it points to goes.
#[expect(clippy::transmute_ptr_to_ptr, reason = "a cast cannot extend a trait object's lifetime")]
unsafe fn erase_discs<'a>(discs: *mut (dyn Discs + 'a)) -> *mut (dyn Discs + 'static) {
    // SAFETY: the same pointer, its lifetime the caller's to keep.
    unsafe { std::mem::transmute(discs) }
}

/// The declarations lent to an extraction session's beginning, by pointer.
pub struct LentDeclarations {
    declared: *const (dyn Declared + 'static),
}

// SAFETY: lent declarations are read only during the call that lent them, on the thread making the call.
unsafe impl Send for LentDeclarations {}

/// A release's discs lent for one asset's extraction, by pointer.
pub struct LentDiscs {
    discs: *mut (dyn Discs + 'static),
}

// SAFETY: lent discs are read only during the call that lent them, on the thread making the call.
unsafe impl Send for LentDiscs {}

/// The project lent for a call, by pointer.
pub struct LentProject {
    project: *const (dyn Project + 'static),
}

// SAFETY: a lent project is asked only during the call that lent it, on the thread making the call.
unsafe impl Send for LentProject {}

/// An asset lent for a call: the host's own, by pointer.
pub struct LentAsset {
    source: *const (dyn Source + 'static),
    node_type: Option<String>,
}

// SAFETY: a lent asset is read only during the call that lent it, on the thread making the call.
unsafe impl Send for LentAsset {}

/// A program running for an unsandboxed extension's component. Dropped, it is stopped.
pub struct HostProcess {
    child: std::process::Child,
    input: Option<std::process::ChildStdin>,
    output: std::process::ChildStdout,
    /// The end of what it says on its standard error, kept by a thread of its own: unread, it would stop the program.
    said: Arc<Mutex<Vec<u8>>>,
}

impl Drop for HostProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// How much of what a program says on its standard error is kept.
const SAID: usize = 4 << 10;

/// A stream given to a component's operation.
pub struct HostStreamReader(pe2_ext::StreamInput);

/// Bytes the host holds for a component: a frame's planes, a stretch of sound - shared by every handle to them.
pub struct HostMediaBuffer(Arc<Vec<u8>>);

/// A file an operation makes, held for the component writing it: the host's writer (`CallHost::writer`) until it is
/// finished.
pub struct HostWriter(Option<Box<dyn pe2_ext::FileWriter>>);

/// A frame's UI as a view draws on it: the host's, lent for the frame, or a row, a section or a body of it.
pub enum Frame {
    Lent(*mut (dyn UiHost + 'static)),
    Child(Box<dyn UiHost + 'static>),
}

// SAFETY: a frame is drawn on only during the call that lent it, on the thread making the call.
unsafe impl Send for Frame {}

/// An image a view made, held for it.
pub struct HostTexture {
    key: String,
    image: Arc<Image>,
}

/// An instance's memory as it grows: linear memory never shrinks, so this is what it holds until it is dropped.
#[derive(Default)]
struct Meter {
    bytes: AtomicUsize,
}

/// An instance's limits: the cap on each memory, its job's, and its growth counted on its meter.
struct Limits {
    meter: Arc<Meter>,
    cap: u64,
}

impl ResourceLimiter for Limits {
    fn memory_growing(&mut self, current: usize, desired: usize, _maximum: Option<usize>) -> wasmtime::Result<bool> {
        if desired as u64 > self.cap {
            return Ok(false);
        }
        self.meter.bytes.fetch_add(desired.saturating_sub(current), Ordering::Relaxed);
        Ok(true)
    }

    fn table_growing(&mut self, _current: usize, _desired: usize, _maximum: Option<usize>) -> wasmtime::Result<bool> {
        Ok(true)
    }
}

/// What an extension's instances hold.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Memory {
    /// Its live instances, and the memory they hold.
    pub instances: usize,
    pub bytes: usize,
    /// The most an instance of it has held.
    pub peak: usize,
}

/// An instance's state: its resources, WASI with nothing behind it, its limits.
pub struct State {
    id: String,
    wasi: WasiCtx,
    table: ResourceTable,
    limits: Limits,
    /// The frames open, the lent one first and its innermost row last: only the last is drawn on.
    frames: Vec<u32>,
    /// The assets the project's queries lent during the call, taken back when it returns.
    found: Vec<u32>,
    /// The settings kept for its extension, by scope (TOML), as `settings` gives them.
    settings: Arc<str>,
    /// What it is outside the sandbox, where it is: it alone launches programs.
    unsandboxed: Option<Arc<Unsandboxed>>,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.wasi, table: &mut self.table }
    }
}

impl State {
    fn lent(&self, asset: &Resource<LentAsset>) -> wasmtime::Result<&dyn Source> {
        let lent = self.table.get(asset)?;
        // SAFETY: lent for the call running now (`Runner::lend`), taken back before it returns.
        Ok(unsafe { &*lent.source })
    }

    fn frame(&mut self, ui: &Resource<Frame>) -> wasmtime::Result<&mut dyn UiHost> {
        if self.frames.last() != Some(&ui.rep()) {
            return Err(wasmtime::Error::msg("a view drew on a frame while a row of it was open"));
        }
        Ok(match self.table.get_mut(ui)? {
            // SAFETY: lent for the frame being drawn (`ComponentView::show`), taken back before it returns.
            Frame::Lent(host) => unsafe { &mut **host },
            Frame::Child(child) => &mut **child,
        })
    }

    /// The project lent for the call running now: its pointer, which is valid until the call returns.
    fn project(&self, project: &Resource<LentProject>) -> wasmtime::Result<*const (dyn Project + 'static)> {
        Ok(self.table.get(project)?.project)
    }

    /// An asset a query found, lent to the guest until the call returns.
    fn found(&mut self, asset: Option<Asset<'_>>) -> wasmtime::Result<Option<Resource<LentAsset>>> {
        let Some(asset) = asset else { return Ok(None) };
        // SAFETY: the project's asset outlives the call; the lent asset is taken back when the call returns.
        let source = unsafe { erase_source(asset.source()) };
        let lent = LentAsset { source, node_type: asset.node_type().map(str::to_string) };
        let lent = self.table.push(lent)?;
        self.found.push(lent.rep());
        Ok(Some(lent))
    }

    /// A child frame of the innermost one, open until the guest drops it.
    fn child(&mut self, child: Box<dyn UiHost + 'static>) -> wasmtime::Result<Resource<Frame>> {
        let child = self.table.push(Frame::Child(child))?;
        self.frames.push(child.rep());
        Ok(child)
    }
}

impl host::Host for State {
    fn log(&mut self, message: String) -> wasmtime::Result<()> {
        eprintln!("{}: {message}", self.id);
        Ok(())
    }

    fn settings(&mut self) -> wasmtime::Result<String> {
        Ok(self.settings.to_string())
    }
}

impl host::HostAsset for State {
    fn path(&mut self, asset: Resource<LentAsset>) -> wasmtime::Result<String> {
        Ok(self.lent(&asset)?.path().to_string())
    }

    fn uid(&mut self, asset: Resource<LentAsset>) -> wasmtime::Result<Option<String>> {
        Ok(self.lent(&asset)?.uid().map(str::to_string))
    }

    fn node_type(&mut self, asset: Resource<LentAsset>) -> wasmtime::Result<Option<String>> {
        Ok(self.table.get(&asset)?.node_type.clone())
    }

    fn declaration(&mut self, asset: Resource<LentAsset>) -> wasmtime::Result<String> {
        Ok(self.lent(&asset)?.declaration().to_string())
    }

    fn has_file(&mut self, asset: Resource<LentAsset>, name: String) -> wasmtime::Result<bool> {
        Ok(self.lent(&asset)?.has_file(&name))
    }

    fn read(&mut self, asset: Resource<LentAsset>, name: String) -> wasmtime::Result<Result<Vec<u8>, String>> {
        Ok(self.lent(&asset)?.read(&name).map_err(|e| format!("{e:#}")))
    }

    fn size(&mut self, asset: Resource<LentAsset>, name: String) -> wasmtime::Result<Option<u64>> {
        Ok(self.lent(&asset)?.size(&name))
    }

    fn read_range(
        &mut self,
        asset: Resource<LentAsset>,
        name: String,
        offset: u64,
        len: u32,
    ) -> wasmtime::Result<Result<Vec<u8>, String>> {
        Ok(self.lent(&asset)?.read_range(&name, offset, len as usize).map_err(|e| format!("{e:#}")))
    }

    fn writer(
        &mut self,
        asset: Resource<LentAsset>,
        name: String,
    ) -> wasmtime::Result<Result<Resource<HostWriter>, String>> {
        let made = match self.lent(&asset)?.writer(&name) {
            Some(made) => made.map_err(|e| format!("{e:#}")),
            None => Err(format!("{}: lent for no operation's run, it makes no files", self.lent(&asset)?.path())),
        };
        Ok(match made {
            Ok(writer) => Ok(self.table.push(HostWriter(Some(writer)))?),
            Err(e) => Err(e),
        })
    }

    fn entries(&mut self, asset: Resource<LentAsset>, folder: String) -> wasmtime::Result<Result<Vec<String>, String>> {
        Ok(self
            .lent(&asset)?
            .entries(&folder)
            .unwrap_or_else(|| Err(anyhow!("lent for no operation's run")))
            .map_err(|e| format!("{e:#}")))
    }

    fn read_entry(&mut self, asset: Resource<LentAsset>, path: String) -> wasmtime::Result<Result<Vec<u8>, String>> {
        Ok(self
            .lent(&asset)?
            .read_entry(&path)
            .unwrap_or_else(|| Err(anyhow!("lent for no operation's run")))
            .map_err(|e| format!("{e:#}")))
    }

    fn place(&mut self, asset: Resource<LentAsset>, path: String) -> wasmtime::Result<Option<String>> {
        Ok(self.lent(&asset)?.place(&path))
    }

    fn node_named(
        &mut self,
        asset: Resource<LentAsset>,
        reference: String,
    ) -> wasmtime::Result<Option<host::NodeNamed>> {
        let named = self.lent(&asset)?.node_named(&reference);
        Ok(named.map(|named| host::NodeNamed { path: named.path, uid: named.uid, types: named.types }))
    }

    fn output_of(
        &mut self,
        asset: Resource<LentAsset>,
        path: String,
        address: String,
    ) -> wasmtime::Result<Result<types::Value, String>> {
        let told = self
            .lent(&asset)?
            .output_of(&path, &address)
            .unwrap_or_else(|| Err(anyhow!("lent for no operation's run")));
        Ok(match told {
            Ok(value) => Ok(value_to_wit(value, &mut self.table).map_err(|e| wasmtime::Error::msg(format!("{e:#}")))?),
            Err(e) => Err(format!("{e:#}")),
        })
    }

    fn build_config(&mut self, asset: Resource<LentAsset>) -> wasmtime::Result<Option<host::BuildConfig>> {
        let told = self.lent(&asset)?.build_config();
        Ok(told.map(|c| host::BuildConfig {
            name: c.name,
            release: c.release,
            target: c.target,
            folder: c.folder,
            known: c.known,
        }))
    }

    fn assets_of(
        &mut self,
        asset: Resource<LentAsset>,
        node_type: String,
    ) -> wasmtime::Result<Result<Vec<(String, String)>, String>> {
        Ok(self
            .lent(&asset)?
            .assets_of(&node_type)
            .unwrap_or_else(|| Err(anyhow!("lent for no operation's run")))
            .map_err(|e| format!("{e:#}")))
    }

    fn temp(&mut self, asset: Resource<LentAsset>) -> wasmtime::Result<Result<String, String>> {
        if self.unsandboxed.is_none() {
            return Ok(Err(format!("{}: it runs in the sandbox", self.id)));
        }
        Ok(self
            .lent(&asset)?
            .temp()
            .unwrap_or_else(|| Err(anyhow!("lent for no operation's run")))
            .map_err(|e| format!("{e:#}")))
    }

    fn keep(
        &mut self,
        asset: Resource<LentAsset>,
        path: String,
        name: String,
    ) -> wasmtime::Result<Result<types::File, String>> {
        if self.unsandboxed.is_none() {
            return Ok(Err(format!("{}: it runs in the sandbox", self.id)));
        }
        let kept = self.lent(&asset)?.keep(&path, &name).unwrap_or_else(|| Err(anyhow!("lent for no operation's run")));
        Ok(kept.map(file_to_wit).map_err(|e| format!("{e:#}")))
    }

    fn path_of(&mut self, asset: Resource<LentAsset>, name: String) -> wasmtime::Result<Option<String>> {
        if self.unsandboxed.is_none() {
            return Ok(None);
        }
        Ok(self.lent(&asset)?.path_of(&name))
    }

    fn drop(&mut self, asset: Resource<LentAsset>) -> wasmtime::Result<()> {
        self.found.retain(|&rep| rep != asset.rep());
        self.table.delete(asset)?;
        Ok(())
    }
}

impl host::HostWriter for State {
    fn append(&mut self, writer: Resource<HostWriter>, bytes: Vec<u8>) -> wasmtime::Result<Result<(), String>> {
        let Some(inner) = self.table.get_mut(&writer)?.0.as_mut() else { return Ok(Err("finished".into())) };
        Ok(inner.append(&bytes).map_err(|e| format!("{e:#}")))
    }

    fn finish(&mut self, writer: Resource<HostWriter>) -> wasmtime::Result<Result<types::File, String>> {
        let Some(inner) = self.table.get_mut(&writer)?.0.take() else { return Ok(Err("finished".into())) };
        Ok(inner.finish().map(file_to_wit).map_err(|e| format!("{e:#}")))
    }

    fn drop(&mut self, writer: Resource<HostWriter>) -> wasmtime::Result<()> {
        self.table.delete(writer)?;
        Ok(())
    }
}

impl host::HostProject for State {
    fn asset(&mut self, project: Resource<LentProject>, path: String) -> wasmtime::Result<Option<Resource<LentAsset>>> {
        // SAFETY: lent for the call running now, taken back before it returns.
        let project = unsafe { &*self.project(&project)? };
        self.found(project.asset(&path))
    }

    fn resolve(
        &mut self,
        project: Resource<LentProject>,
        reference: String,
    ) -> wasmtime::Result<Option<Resource<LentAsset>>> {
        // SAFETY: as `asset`.
        let project = unsafe { &*self.project(&project)? };
        let Some(reference) = parse_value(&reference) else { return Ok(None) };
        self.found(project.resolve(&reference))
    }

    fn find(
        &mut self,
        project: Resource<LentProject>,
        node_type: String,
    ) -> wasmtime::Result<Vec<Resource<LentAsset>>> {
        // SAFETY: as `asset`.
        let project = unsafe { &*self.project(&project)? };
        let mut out = vec![];
        for asset in project.find(&node_type) {
            out.extend(self.found(Some(asset))?);
        }
        Ok(out)
    }

    fn selection(&mut self, project: Resource<LentProject>) -> wasmtime::Result<Option<Resource<LentAsset>>> {
        // SAFETY: as `asset`.
        let project = unsafe { &*self.project(&project)? };
        self.found(project.selection())
    }

    fn chosen(
        &mut self,
        project: Resource<LentProject>,
        path: String,
        family: String,
    ) -> wasmtime::Result<Option<String>> {
        // SAFETY: as `asset`.
        let project = unsafe { &*self.project(&project)? };
        Ok(project.chosen(&path, &family))
    }

    fn picture(
        &mut self,
        project: Resource<LentProject>,
        path: String,
        look: Option<String>,
    ) -> wasmtime::Result<Option<Result<types::Image, String>>> {
        let project = self.project(&project)?;
        Ok(elsewhere(project, move |p| {
            p.picture(&path, look.as_deref()).map(|made| made.map(to_wit).map_err(|e| format!("{e:#}")))
        }))
    }

    fn target(&mut self, project: Resource<LentProject>) -> wasmtime::Result<Option<String>> {
        // SAFETY: as `asset`.
        let project = unsafe { &*self.project(&project)? };
        Ok(project.target())
    }

    fn revision(&mut self, project: Resource<LentProject>) -> wasmtime::Result<u64> {
        // SAFETY: as `asset`.
        let project = unsafe { &*self.project(&project)? };
        Ok(project.revision())
    }

    fn image(
        &mut self,
        project: Resource<LentProject>,
        path: String,
        address: String,
    ) -> wasmtime::Result<Option<Result<types::Image, String>>> {
        let project = self.project(&project)?;
        Ok(elsewhere(project, move |p| {
            p.image(&path, &address).map(|made| made.map(to_wit).map_err(|e| format!("{e:#}")))
        }))
    }

    fn table(
        &mut self,
        project: Resource<LentProject>,
        path: String,
        address: String,
    ) -> wasmtime::Result<Option<Result<String, String>>> {
        let project = self.project(&project)?;
        Ok(elsewhere(project, move |p| {
            p.table(&path, &address).map(|made| made.map(|table| table.to_string()).map_err(|e| format!("{e:#}")))
        }))
    }

    fn bytes(
        &mut self,
        project: Resource<LentProject>,
        path: String,
        address: String,
    ) -> wasmtime::Result<Option<Result<Vec<u8>, String>>> {
        let project = self.project(&project)?;
        Ok(elsewhere(project, move |p| p.bytes(&path, &address).map(|made| made.map_err(|e| format!("{e:#}")))))
    }

    fn request(
        &mut self,
        project: Resource<LentProject>,
        path: String,
        address: String,
    ) -> wasmtime::Result<host::Requested> {
        let project = self.project(&project)?;
        Ok(match elsewhere(project, move |p| p.request(&path, &address)) {
            pe2_ext::Requested::Ready(bytes) => host::Requested::Ready(bytes),
            pe2_ext::Requested::Pending { waited } => host::Requested::Pending(waited),
            pe2_ext::Requested::Failed(why) => host::Requested::Failed(why),
        })
    }

    fn call(
        &mut self,
        project: Resource<LentProject>,
        service: String,
        arguments: String,
    ) -> wasmtime::Result<Result<String, String>> {
        let project = self.project(&project)?;
        // SAFETY: as `asset`.
        if !unsafe { &*project }.may_call(&self.id, &service) {
            return Ok(Err(format!("{service}: not a service of {} or of an extension it requires", self.id)));
        }
        let Some(arguments) = parse_value(&arguments) else { return Ok(Err("its arguments are not TOML".into())) };
        Ok(elsewhere(project, move |p| {
            p.call(&service, &arguments).map(|answer| answer.to_string()).map_err(|e| format!("{e:#}"))
        }))
    }

    fn drop(&mut self, project: Resource<LentProject>) -> wasmtime::Result<()> {
        self.table.delete(project)?;
        Ok(())
    }
}

/// A value as it crosses: TOML.
impl host::HostDeclarations for State {
    fn of_type(
        &mut self,
        declarations: Resource<LentDeclarations>,
        node_type: String,
    ) -> wasmtime::Result<Vec<(String, Vec<u8>)>> {
        // SAFETY: lent for the call running now (`ComponentExtraction::begin`), taken back before it returns.
        let declared = unsafe { &*self.table.get(&declarations)?.declared };
        let _span = tracing::info_span!("declarations lent", of = %node_type).entered();
        Ok(declared.encoded(&node_type).as_ref().clone())
    }

    fn drop(&mut self, declarations: Resource<LentDeclarations>) -> wasmtime::Result<()> {
        // A borrow the guest drops: the host takes the declarations back itself.
        let _ = declarations;
        Ok(())
    }
}

impl host::HostDiscs for State {
    fn read(&mut self, discs: Resource<LentDiscs>, at: Vec<u8>) -> wasmtime::Result<Result<Vec<u8>, String>> {
        // SAFETY: lent for the call running now (`ComponentExtraction::extract`), taken back before it returns.
        let discs = unsafe { &mut *self.table.get(&discs)?.discs };
        let at: Table = match pe2_ext::cbor::decode(&at) {
            Ok(at) => at,
            Err(e) => return Ok(Err(format!("not a location: {e:#}"))),
        };
        Ok(discs.read(&at).map_err(|e| format!("{e:#}")))
    }

    fn size(&mut self, discs: Resource<LentDiscs>, at: Vec<u8>) -> wasmtime::Result<Result<u64, String>> {
        // SAFETY: as `read`.
        let discs = unsafe { &mut *self.table.get(&discs)?.discs };
        let at: Table = match pe2_ext::cbor::decode(&at) {
            Ok(at) => at,
            Err(e) => return Ok(Err(format!("not a location: {e:#}"))),
        };
        Ok(discs.size(&at).map_err(|e| format!("{e:#}")))
    }

    fn read_range(
        &mut self,
        discs: Resource<LentDiscs>,
        at: Vec<u8>,
        offset: u64,
        len: u32,
    ) -> wasmtime::Result<Result<Vec<u8>, String>> {
        // SAFETY: as `read`.
        let discs = unsafe { &mut *self.table.get(&discs)?.discs };
        let at: Table = match pe2_ext::cbor::decode(&at) {
            Ok(at) => at,
            Err(e) => return Ok(Err(format!("not a location: {e:#}"))),
        };
        Ok(discs.read_range(&at, offset, len as usize).map_err(|e| format!("{e:#}")))
    }

    fn drop(&mut self, discs: Resource<LentDiscs>) -> wasmtime::Result<()> {
        let _ = discs;
        Ok(())
    }
}

/// Run a component's future to its end on this thread. Within another component's call - one extension asking another
/// through the host - that call's runtime is driving this thread, and tokio refuses to block on another inside it: the
/// future is polled here instead, which suffices for a guest that does not sleep (one that does is stopped by its
/// deadline). Otherwise on the instance's own runtime.
fn drive<F: std::future::Future>(runtime: &Runtime, future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() { pollster::block_on(future) } else { runtime.block_on(future) }
}

/// An instance's runtime, shut down without blocking when it goes: an instance a nested call made is dropped within
/// the calling component's runtime, where tokio refuses to block.
struct Runtime(std::mem::ManuallyDrop<tokio::runtime::Runtime>);

impl std::ops::Deref for Runtime {
    type Target = tokio::runtime::Runtime;

    fn deref(&self) -> &tokio::runtime::Runtime {
        &self.0
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        // SAFETY: taken once, here, and never used again.
        unsafe { std::mem::ManuallyDrop::take(&mut self.0) }.shutdown_background();
    }
}

/// Ask the lent project: what it asks may run another extension's component, inline (`Runner::call`).
fn elsewhere<R>(project: *const (dyn Project + 'static), ask: impl FnOnce(&dyn Project) -> R) -> R {
    // SAFETY: lent for the call running now, which this is part of.
    ask(unsafe { &*project })
}

/// An image as the contract carries it.
fn to_wit(image: Image) -> types::Image {
    let to_u32 = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
    types::Image { width: to_u32(image.width), height: to_u32(image.height), rgba: image.rgba }
}

fn parse_value(text: &str) -> Option<Value> {
    toml::from_str::<Table>(&format!("v = {text}")).ok()?.remove("v")
}

/// What a field was given: one that does not parse is an error, never a removal.
fn field_value(key: &str, text: &str) -> wasmtime::Result<Value> {
    parse_value(text).ok_or_else(|| wasmtime::Error::msg(format!("{key}: not TOML")))
}

impl host::HostTexture for State {
    fn new(&mut self, key: String, image: types::Image) -> wasmtime::Result<Resource<HostTexture>> {
        let image = Image { width: image.width as usize, height: image.height as usize, rgba: image.rgba };
        Ok(self.table.push(HostTexture { key, image: Arc::new(image) })?)
    }

    fn drop(&mut self, texture: Resource<HostTexture>) -> wasmtime::Result<()> {
        self.table.delete(texture)?;
        Ok(())
    }
}

impl unsandboxed::Host for State {
    fn set_up(&mut self, name: String) -> wasmtime::Result<Result<(), String>> {
        let id = self.id.clone();
        let Some(unsandboxed) = &self.unsandboxed else {
            return Ok(Err(format!("{id}: it runs in the sandbox, and fetches nothing")));
        };
        let Some(setup) = unsandboxed.setup.iter().find(|setup| setup.name == name) else {
            return Ok(Err(format!("{id} fetches nothing called {name} for this platform")));
        };
        let Some(store) = &unsandboxed.store else {
            return Ok(Err(format!("{name}: there is no place on this computer to keep it")));
        };
        if setup.fetched(store) {
            return Ok(Ok(()));
        }
        if unsandboxed.off.contains(&setup.key()) {
            return Ok(Err(format!(
                "{name} is turned off on this computer: turn it on in {id}'s Fetches in the editor's Extensions, or \
                 `pe2-cli project extension setup {id} --enable \"{name}\"`"
            )));
        }
        let cancel = std::sync::atomic::AtomicBool::new(false);
        let fetched = super::setup::fetch(setup, store, unsandboxed.folder.as_deref(), &mut |_, _| {}, &cancel);
        Ok(fetched.map_err(|e| format!("{id}: {name}: {e:#}")))
    }

    fn spawn(&mut self, tool: String, args: Vec<String>) -> wasmtime::Result<Result<Resource<HostProcess>, String>> {
        let Some(unsandboxed) = &self.unsandboxed else {
            return Ok(Err(format!("{}: it runs in the sandbox", self.id)));
        };
        let (folder, fetched) = (unsandboxed.folder.as_deref(), unsandboxed.fetched());
        // A name it launches is another program with arguments of its own, before those passed.
        let (program, args) = match unsandboxed.launch.iter().find(|launch| launch.name == tool) {
            Some(launch) => {
                let (program, mut before) = launch.resolved(folder, &fetched);
                before.extend(args);
                (program, before)
            }
            None => (super::unsandboxed::program(folder, &fetched, &tool), args),
        };
        let spawned = std::process::Command::new(&program)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                let missing = e.kind() == std::io::ErrorKind::NotFound;
                let notes = unsandboxed.install.as_ref().filter(|_| missing);
                let mut see = notes
                    .map_or_default(|notes| format!("; see {}'s installation notes: {}", self.id, notes.display()));
                // What it fetches may be what is missing.
                let unset = unsandboxed.unset();
                if missing && !unset.is_empty() {
                    let (id, unset) = (&self.id, unset.join(", "));
                    see = format!(
                        "; {id} has not fetched what it requires on this machine yet ({unset}): \
                         `pe2-cli project extension setup {id}`, or its Fetches in the editor's Extensions{see}"
                    );
                }
                return Ok(Err(format!("{}: {e}{see}", program.display())));
            }
        };
        let (input, output, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
        let (Some(output), Some(mut stderr)) = (output, stderr) else {
            let _ = child.kill();
            return Ok(Err(format!("{}: no pipes", program.display())));
        };
        let said: Arc<Mutex<Vec<u8>>> = Arc::default();
        let kept = said.clone();
        std::thread::spawn(move || {
            use std::io::Read;
            let mut piece = [0; 1 << 12];
            while let Ok(n) = stderr.read(&mut piece) {
                if n == 0 {
                    break;
                }
                if let Ok(mut kept) = kept.lock() {
                    kept.extend_from_slice(&piece[..n]);
                    let over = kept.len().saturating_sub(SAID);
                    kept.drain(..over);
                }
            }
        });
        Ok(Ok(self.table.push(HostProcess { child, input, output, said })?))
    }
}

impl unsandboxed::HostProcess for State {
    fn write(&mut self, process: Resource<HostProcess>, bytes: Vec<u8>) -> wasmtime::Result<Result<(), String>> {
        use std::io::Write;
        let Some(input) = self.table.get_mut(&process)?.input.as_mut() else {
            return Ok(Err("its input ended".into()));
        };
        Ok(input.write_all(&bytes).map_err(|e| e.to_string()))
    }

    fn write_buffer(
        &mut self,
        process: Resource<HostProcess>,
        buffer: Resource<HostMediaBuffer>,
    ) -> wasmtime::Result<Result<(), String>> {
        use std::io::Write;
        let bytes = self.table.get(&buffer)?.0.clone();
        let Some(input) = self.table.get_mut(&process)?.input.as_mut() else {
            return Ok(Err("its input ended".into()));
        };
        Ok(input.write_all(&bytes).map_err(|e| e.to_string()))
    }

    fn close_input(&mut self, process: Resource<HostProcess>) -> wasmtime::Result<()> {
        self.table.get_mut(&process)?.input = None;
        Ok(())
    }

    fn read(&mut self, process: Resource<HostProcess>, len: u32) -> wasmtime::Result<Result<Vec<u8>, String>> {
        use std::io::Read;
        let mut bytes = vec![0; len as usize];
        Ok(match self.table.get_mut(&process)?.output.read(&mut bytes) {
            Ok(n) => {
                bytes.truncate(n);
                Ok(bytes)
            }
            Err(e) => Err(e.to_string()),
        })
    }

    fn wait(&mut self, process: Resource<HostProcess>) -> wasmtime::Result<Result<(), String>> {
        let running = self.table.get_mut(&process)?;
        running.input = None;
        // What it still writes, read: it would stop on a full pipe.
        let _ = std::io::copy(&mut running.output, &mut std::io::sink());
        let status = running.child.wait();
        let said =
            running.said.lock().map_or_else(|_| String::new(), |s| String::from_utf8_lossy(&s).trim().to_string());
        Ok(match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(format!("{status}: {said}")),
            Err(e) => Err(e.to_string()),
        })
    }

    fn drop(&mut self, process: Resource<HostProcess>) -> wasmtime::Result<()> {
        self.table.delete(process)?;
        Ok(())
    }
}

impl types::Host for State {}

impl types::HostStreamReader for State {
    fn header(&mut self, reader: Resource<HostStreamReader>) -> wasmtime::Result<String> {
        Ok(toml::to_string(&self.table.get(&reader)?.0.header()).unwrap_or_default())
    }

    fn next(&mut self, reader: Resource<HostStreamReader>) -> wasmtime::Result<Result<Option<types::Value>, String>> {
        let stream = self.table.get(&reader)?.0.clone();
        Ok(match stream.next() {
            Ok(Some(item)) => value_to_wit(item, &mut self.table).map(Some).map_err(|e| format!("{e:#}")),
            Ok(None) => Ok(None),
            Err(e) => Err(format!("{e:#}")),
        })
    }

    fn seek(&mut self, reader: Resource<HostStreamReader>, position: u64) -> wasmtime::Result<()> {
        self.table.get(&reader)?.0.seek(position);
        Ok(())
    }

    fn drop(&mut self, reader: Resource<HostStreamReader>) -> wasmtime::Result<()> {
        self.table.delete(reader)?;
        Ok(())
    }
}

impl media::Host for State {
    fn pixel_formats(&mut self) -> wasmtime::Result<Vec<media::PixelFormat>> {
        Ok(pe2_ext::media::pixel_formats().into_iter().map(pixel_format_to_wit).collect())
    }

    fn sample_formats(&mut self) -> wasmtime::Result<Vec<media::SampleFormat>> {
        Ok(pe2_ext::media::sample_formats().into_iter().map(sample_format_to_wit).collect())
    }
}

impl media::HostMediaBuffer for State {
    fn new(&mut self, data: Vec<u8>) -> wasmtime::Result<Resource<HostMediaBuffer>> {
        Ok(self.table.push(HostMediaBuffer(Arc::new(data)))?)
    }

    fn size(&mut self, buffer: Resource<HostMediaBuffer>) -> wasmtime::Result<u64> {
        Ok(self.table.get(&buffer)?.0.len() as u64)
    }

    fn bytes(&mut self, buffer: Resource<HostMediaBuffer>) -> wasmtime::Result<Vec<u8>> {
        Ok(self.table.get(&buffer)?.0.to_vec())
    }

    fn share(&mut self, buffer: Resource<HostMediaBuffer>) -> wasmtime::Result<Resource<HostMediaBuffer>> {
        let bytes = self.table.get(&buffer)?.0.clone();
        Ok(self.table.push(HostMediaBuffer(bytes))?)
    }

    fn drop(&mut self, buffer: Resource<HostMediaBuffer>) -> wasmtime::Result<()> {
        self.table.delete(buffer)?;
        Ok(())
    }
}

fn pixel_format_to_wit(format: pe2_ext::media::PixelFormat) -> media::PixelFormat {
    use pe2_ext::media::PixelFormat as F;
    match format {
        F::Rgba8888 => media::PixelFormat::Rgba8888,
        F::Bgra8888 => media::PixelFormat::Bgra8888,
        F::Yuv420p => media::PixelFormat::Yuv420p,
        F::Nv12 => media::PixelFormat::Nv12,
    }
}

fn pixel_format_from_wit(format: media::PixelFormat) -> pe2_ext::media::PixelFormat {
    use pe2_ext::media::PixelFormat as F;
    match format {
        media::PixelFormat::Rgba8888 => F::Rgba8888,
        media::PixelFormat::Bgra8888 => F::Bgra8888,
        media::PixelFormat::Yuv420p => F::Yuv420p,
        media::PixelFormat::Nv12 => F::Nv12,
    }
}

fn sample_format_to_wit(format: pe2_ext::media::SampleFormat) -> media::SampleFormat {
    match format {
        pe2_ext::media::SampleFormat::S16Interleaved => media::SampleFormat::S16Interleaved,
        pe2_ext::media::SampleFormat::F32Planar => media::SampleFormat::F32Planar,
    }
}

fn sample_format_from_wit(format: media::SampleFormat) -> pe2_ext::media::SampleFormat {
    match format {
        media::SampleFormat::S16Interleaved => pe2_ext::media::SampleFormat::S16Interleaved,
        media::SampleFormat::F32Planar => pe2_ext::media::SampleFormat::F32Planar,
    }
}

fn rect(r: types::Rect) -> pe2_ext::Rect {
    pe2_ext::Rect { x: r.x, y: r.y, width: r.width, height: r.height }
}

fn style(style: types::Style) -> pe2_ext::Style {
    match style {
        types::Style::Normal => pe2_ext::Style::Normal,
        types::Style::Weak => pe2_ext::Style::Weak,
        types::Style::Note => pe2_ext::Style::Note,
        types::Style::Strong => pe2_ext::Style::Strong,
        types::Style::Error => pe2_ext::Style::Error,
    }
}

impl host::HostUi for State {
    fn label(&mut self, ui: Resource<Frame>, text: String, style: types::Style) -> wasmtime::Result<()> {
        self.frame(&ui)?.label(&text, self::style(style));
        Ok(())
    }

    fn table(
        &mut self,
        ui: Resource<Frame>,
        id: String,
        rows: Vec<Vec<host::Cell>>,
    ) -> wasmtime::Result<Option<(u32, u32)>> {
        let rows: Vec<Vec<pe2_ext::Cell>> = rows
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .map(|cell| match cell {
                        host::Cell::Empty => pe2_ext::Cell::Empty,
                        host::Cell::Text((text, s)) => pe2_ext::Cell::Text(text, style(s)),
                        host::Cell::Reference(text) => {
                            pe2_ext::Cell::Reference(parse_value(&text).unwrap_or(Value::String(text)))
                        }
                        host::Cell::Button((i, hint)) => pe2_ext::Cell::Button(icon(i), hint),
                    })
                    .collect()
            })
            .collect();
        let clicked = self.frame(&ui)?.table(&id, &rows);
        Ok(clicked.map(|(r, c)| (u32::try_from(r).unwrap_or(u32::MAX), u32::try_from(c).unwrap_or(u32::MAX))))
    }

    fn button(&mut self, ui: Resource<Frame>, text: String) -> wasmtime::Result<bool> {
        Ok(self.frame(&ui)?.button(&text))
    }

    fn icon_button(&mut self, ui: Resource<Frame>, which: types::Icon, hint: String) -> wasmtime::Result<bool> {
        Ok(self.frame(&ui)?.icon_button(icon(which), &hint))
    }

    fn checkbox(&mut self, ui: Resource<Frame>, text: String, value: bool) -> wasmtime::Result<bool> {
        let mut value = value;
        self.frame(&ui)?.checkbox(&text, &mut value);
        Ok(value)
    }

    fn badge(&mut self, ui: Resource<Frame>, text: String) -> wasmtime::Result<()> {
        self.frame(&ui)?.badge(&text);
        Ok(())
    }

    fn toggle(&mut self, ui: Resource<Frame>, text: String, on: bool) -> wasmtime::Result<bool> {
        Ok(self.frame(&ui)?.toggle(&text, on))
    }

    fn chip(&mut self, ui: Resource<Frame>, text: String, selected: bool) -> wasmtime::Result<bool> {
        Ok(self.frame(&ui)?.chip(&text, selected))
    }

    fn available(&mut self, ui: Resource<Frame>) -> wasmtime::Result<(f32, f32, f32)> {
        Ok(self.frame(&ui)?.available())
    }

    fn gpu_viewport(
        &mut self,
        ui: Resource<Frame>,
        id: u64,
        width: f32,
        height: f32,
    ) -> wasmtime::Result<host::ViewportInput> {
        let input = self.frame(&ui)?.gpu_viewport(id, width, height);
        Ok(host::ViewportInput {
            hovered: input.hovered,
            pointer: input.pointer,
            drag: input.drag,
            held: input.held,
            pressed: input.pressed,
            released: input.released,
            clicked: input.clicked,
            scroll: input.scroll,
            modifiers: (input.modifiers.shift, input.modifiers.ctrl, input.modifiers.alt),
            keys: input.keys,
        })
    }

    fn request_repaint(&mut self, ui: Resource<Frame>) -> wasmtime::Result<()> {
        self.frame(&ui)?.request_repaint();
        Ok(())
    }

    fn time(&mut self, ui: Resource<Frame>) -> wasmtime::Result<f64> {
        Ok(self.frame(&ui)?.time())
    }

    fn image(
        &mut self,
        ui: Resource<Frame>,
        texture: Resource<HostTexture>,
        width: f32,
        height: f32,
    ) -> wasmtime::Result<()> {
        let texture = self.table.get(&texture)?;
        let texture = pe2_ext::Texture::shared(texture.key.clone(), texture.image.clone());
        self.frame(&ui)?.image(&texture, width, height);
        Ok(())
    }

    fn reference(&mut self, ui: Resource<Frame>, reference: String) -> wasmtime::Result<()> {
        if let Some(reference) = parse_value(&reference) {
            self.frame(&ui)?.reference(&reference);
        }
        Ok(())
    }

    fn reference_taking(
        &mut self,
        ui: Resource<Frame>,
        label: String,
        node_type: String,
        reference: Option<String>,
    ) -> wasmtime::Result<Option<String>> {
        let reference = reference.and_then(|text| parse_value(&text));
        Ok(self.frame(&ui)?.reference_taking(&label, &node_type, reference.as_ref()).map(|taken| taken.to_string()))
    }

    fn editing(&mut self, ui: Resource<Frame>) -> wasmtime::Result<Option<(String, String)>> {
        Ok(self.frame(&ui)?.editing().map(|(key, value)| (key, value.to_string())))
    }

    fn field(&mut self, ui: Resource<Frame>, key: String, value: String) -> wasmtime::Result<()> {
        if let Some(value) = parse_value(&value) {
            self.frame(&ui)?.field(&key, &value);
        }
        Ok(())
    }

    fn tooltip(&mut self, ui: Resource<Frame>, text: String) -> wasmtime::Result<()> {
        self.frame(&ui)?.tooltip(&text);
        Ok(())
    }

    fn text_edit(
        &mut self,
        ui: Resource<Frame>,
        id: String,
        text: String,
        hint: String,
        width: f32,
    ) -> wasmtime::Result<(String, bool)> {
        let mut text = text;
        let committed = self.frame(&ui)?.text_edit(&id, &mut text, &hint, width);
        Ok((text, committed))
    }

    fn combo(
        &mut self,
        ui: Resource<Frame>,
        id: String,
        selected: String,
        options: Vec<String>,
    ) -> wasmtime::Result<Option<u32>> {
        let chosen = self.frame(&ui)?.combo(&id, &selected, &options);
        Ok(chosen.and_then(|i| u32::try_from(i).ok()))
    }

    fn shown(&mut self, ui: Resource<Frame>) -> wasmtime::Result<Option<String>> {
        Ok(self.frame(&ui)?.shown())
    }

    fn editable(&mut self, ui: Resource<Frame>) -> wasmtime::Result<bool> {
        Ok(self.frame(&ui)?.editable())
    }

    fn set_step(&mut self, ui: Resource<Frame>, key: String, value: Option<String>) -> wasmtime::Result<()> {
        let value = value.map(|text| field_value(&key, &text)).transpose()?;
        self.frame(&ui)?.set_step(&key, value.as_ref());
        Ok(())
    }

    fn live(&mut self, ui: Resource<Frame>, fields: Vec<(String, String)>) -> wasmtime::Result<()> {
        let fields = fields
            .iter()
            .map(|(key, text)| Ok((key.as_str(), field_value(key, text)?)))
            .collect::<wasmtime::Result<Vec<_>>>()?;
        self.frame(&ui)?.live(&fields);
        Ok(())
    }

    fn set(&mut self, ui: Resource<Frame>, key: String, value: Option<String>) -> wasmtime::Result<()> {
        let value = value.map(|text| field_value(&key, &text)).transpose()?;
        self.frame(&ui)?.set(&key, value.as_ref());
        Ok(())
    }

    fn horizontal(&mut self, ui: Resource<Frame>) -> wasmtime::Result<Resource<Frame>> {
        let row = self.frame(&ui)?.horizontal();
        // SAFETY: the child borrows its frame, which outlives it: that is the innermost open frame until the child is
        // dropped, and every child is dropped before its frame is taken back.
        let row = unsafe { erase_row(row) };
        self.child(row)
    }

    fn vertical(&mut self, ui: Resource<Frame>) -> wasmtime::Result<Resource<Frame>> {
        let column = self.frame(&ui)?.vertical();
        // SAFETY: as `horizontal`.
        let column = unsafe { erase_row(column) };
        self.child(column)
    }

    fn wrapped(&mut self, ui: Resource<Frame>) -> wasmtime::Result<Resource<Frame>> {
        let row = self.frame(&ui)?.wrapped();
        // SAFETY: the child borrows its frame, which outlives it: that is the innermost open frame until the child is
        // dropped, and every child is dropped before its frame is taken back.
        let row = unsafe { erase_row(row) };
        self.child(row)
    }

    fn indented(&mut self, ui: Resource<Frame>) -> wasmtime::Result<Resource<Frame>> {
        let inner = self.frame(&ui)?.indented();
        // SAFETY: the child borrows its frame, which outlives it: that is the innermost open frame until the child is
        // dropped, and every child is dropped before its frame is taken back.
        let inner = unsafe { erase_row(inner) };
        self.child(inner)
    }

    fn section(&mut self, ui: Resource<Frame>, title: String) -> wasmtime::Result<Resource<Frame>> {
        let section = self.frame(&ui)?.section(&title);
        // SAFETY: the child borrows its frame, which outlives it: that is the innermost open frame until the child is
        // dropped, and every child is dropped before its frame is taken back.
        let section = unsafe { erase_row(section) };
        self.child(section)
    }

    fn collapsing(
        &mut self,
        ui: Resource<Frame>,
        id: String,
        title: String,
        note: String,
    ) -> wasmtime::Result<Option<Resource<Frame>>> {
        // SAFETY: as `horizontal`.
        let body = self.frame(&ui)?.collapsing(&id, &title, &note).map(|body| unsafe { erase_row(body) });
        body.map(|body| self.child(body)).transpose()
    }

    fn canvas(&mut self, ui: Resource<Frame>, canvas: host::Canvas) -> wasmtime::Result<()> {
        let mut pictures = Vec::with_capacity(canvas.pictures.len());
        for (texture, placed) in canvas.pictures {
            let texture = self.table.get(&texture)?;
            pictures.push(pe2_ext::Placed {
                texture: pe2_ext::Texture::shared(texture.key.clone(), texture.image.clone()),
                rect: rect(placed.rect),
                label: placed.label,
                link: placed.link,
            });
        }
        let boxes = canvas
            .boxes
            .into_iter()
            .map(|b| pe2_ext::Marked { name: b.name, rect: rect(b.rect), highlight: b.highlight })
            .collect();
        let canvas = pe2_ext::Canvas {
            width: canvas.width,
            height: canvas.height,
            pictures,
            boxes,
            summary: canvas.summary,
            toggle: canvas.toggle,
            legend: canvas.legend,
            grid: canvas.grid,
            traces: canvas
                .traces
                .into_iter()
                .map(|t| pe2_ext::Traced { corners: t.corners, strong: t.strong, label: t.label, link: t.link })
                .collect(),
        };
        self.frame(&ui)?.canvas(&canvas);
        Ok(())
    }

    fn picture(
        &mut self,
        ui: Resource<Frame>,
        path: String,
        look: Option<String>,
    ) -> wasmtime::Result<Option<Result<(Resource<HostTexture>, u32, u32), String>>> {
        let Some(shown) = self.frame(&ui)?.picture(&path, look.as_deref()) else { return Ok(None) };
        Ok(Some(match shown {
            Ok(texture) => {
                let (width, height) = (u32::try_from(texture.width())?, u32::try_from(texture.height())?);
                let held =
                    self.table.push(HostTexture { key: texture.key().to_string(), image: texture.image().clone() })?;
                Ok((held, width, height))
            }
            Err(e) => Err(e),
        }))
    }

    fn navigate(&mut self, ui: Resource<Frame>, path: String) -> wasmtime::Result<()> {
        self.frame(&ui)?.navigate(&path);
        Ok(())
    }

    fn selected(&mut self, ui: Resource<Frame>) -> wasmtime::Result<Option<String>> {
        Ok(self.frame(&ui)?.selected())
    }

    fn note(&mut self, ui: Resource<Frame>) -> wasmtime::Result<Option<String>> {
        Ok(self.frame(&ui)?.note())
    }

    fn leave_note(&mut self, ui: Resource<Frame>, text: String) -> wasmtime::Result<()> {
        self.frame(&ui)?.leave_note(&text);
        Ok(())
    }

    fn select(&mut self, ui: Resource<Frame>, part: String) -> wasmtime::Result<()> {
        self.frame(&ui)?.select(&part);
        Ok(())
    }

    fn choose(&mut self, ui: Resource<Frame>, group: String, member: String) -> wasmtime::Result<()> {
        self.frame(&ui)?.choose(&group, &member);
        Ok(())
    }

    fn tab(&mut self, ui: Resource<Frame>, at: f32) -> wasmtime::Result<()> {
        self.frame(&ui)?.tab(at);
        Ok(())
    }

    fn drop(&mut self, ui: Resource<Frame>) -> wasmtime::Result<()> {
        if self.frames.last() != Some(&ui.rep()) {
            return Err(wasmtime::Error::msg("a view closed a row while one inside it was open"));
        }
        self.frames.pop();
        self.table.delete(ui)?;
        Ok(())
    }
}

/// An instance of a component, with the store it runs in.
struct Runner {
    store: Store<State>,
    instance: bindings::Linked,
    runtime: Runtime,
    meter: Arc<Meter>,
    /// What the guest writes to standard error: a panic's message.
    stderr: MemoryOutputPipe,
}

impl Runner {
    /// The memory it holds.
    fn bytes(&self) -> usize {
        self.meter.bytes.load(Ordering::Relaxed)
    }

    /// Whether a job that ran well on it leaves it for the next step of its chain (`Shared::job`): where a chain runs,
    /// and it holds no more than a resident instance may.
    fn goes_on(&self) -> bool {
        crate::schedule::chained() && self.bytes() <= RESIDENT
    }
}

impl Runner {
    /// Run `call` on the instance, within `ticks` of the epoch.
    fn call<R>(
        &mut self,
        ticks: u64,
        call: impl AsyncFnOnce(&Accessor<State>, &g::Guest) -> wasmtime::Result<R>,
    ) -> Result<R> {
        self.store.set_epoch_deadline(ticks);
        let Runner { store, instance, runtime, .. } = self;
        let guest = instance.pe2_extension_guest();
        let out = drive(runtime, store.run_concurrent(async |accessor| call(accessor, guest).await));
        // What the project's queries lent goes with the call.
        let data = self.store.data_mut();
        for rep in std::mem::take(&mut data.found) {
            let _ = data.table.delete(Resource::<LentAsset>::new_own(rep));
        }
        // A call that traps says what the guest wrote first: a panic's message.
        let out = match out {
            Ok(Ok(out)) => return Ok(out),
            Ok(Err(e)) | Err(e) => e,
        };
        let said = String::from_utf8_lossy(&self.stderr.contents()).trim().to_string();
        Err(if said.is_empty() { anyhow!("{out:#}") } else { anyhow!("{out:#}: {said}") })
    }

    /// Lend the project for a call; `unlend_project` takes it back.
    fn lend_project(&mut self, project: &dyn Project) -> Result<Resource<LentProject>> {
        // SAFETY: the pointer is used only during the call it is lent for, and taken back before it returns.
        let project = unsafe { erase_project(project) };
        Ok(self.store.data_mut().table.push(LentProject { project })?)
    }

    fn unlend_project(&mut self, project: Resource<LentProject>) {
        let _ = self.store.data_mut().table.delete(project);
    }

    /// Lend an asset for a call; `unlend` takes it back.
    fn lend(&mut self, asset: &Asset) -> Result<Resource<LentAsset>> {
        // SAFETY: the pointer is used only during the call it is lent for, and taken back before it returns.
        let source = unsafe { erase_source(asset.source()) };
        let lent = LentAsset { source, node_type: asset.node_type().map(str::to_string) };
        Ok(self.store.data_mut().table.push(lent)?)
    }

    fn unlend(&mut self, asset: Resource<LentAsset>) {
        let _ = self.store.data_mut().table.delete(asset);
    }

    /// A call on a lent asset, with the project to ask about others.
    fn with_asset_in<R>(
        &mut self,
        asset: &Asset,
        project: &dyn Project,
        ticks: u64,
        call: impl AsyncFnOnce(
            &Accessor<State>,
            &g::Guest,
            Resource<LentAsset>,
            Resource<LentProject>,
        ) -> wasmtime::Result<R>,
    ) -> Result<R> {
        let lent = self.lend(asset)?;
        let lent_project = match self.lend_project(project) {
            Ok(lent_project) => lent_project,
            Err(why) => {
                self.unlend(lent);
                return Err(why);
            }
        };
        let (borrowed, borrowed_project) = (Resource::new_borrow(lent.rep()), Resource::new_borrow(lent_project.rep()));
        let out = self.call(ticks, async |accessor, guest| call(accessor, guest, borrowed, borrowed_project).await);
        self.unlend_project(lent_project);
        self.unlend(lent);
        out
    }

    /// A call on a lent asset.
    fn with_asset<R>(
        &mut self,
        asset: &Asset,
        ticks: u64,
        call: impl AsyncFnOnce(&Accessor<State>, &g::Guest, Resource<LentAsset>) -> wasmtime::Result<R>,
    ) -> Result<R> {
        let lent = self.lend(asset)?;
        let borrowed = Resource::new_borrow(lent.rep());
        let out = self.call(ticks, async |accessor, guest| call(accessor, guest, borrowed).await);
        self.unlend(lent);
        out
    }
}

/// A component, compiled once in an engine of its own, and its instances by the lifetime of their job: a job's (a
/// picture, an import, a tool's run) made for its call and dropped after it; the readings' (has a picture, a field's
/// reading, facts), reused while used, reclaimed after `READING_FRAMES` frames unused; the long-running views',
/// which goes when its last view closes and it says it may. A guest's memory only grows, so dropping its instance is
/// what gives it back.
pub struct Shared {
    id: String,
    engine: Engine,
    pre: bindings::LinkedPre<State>,
    /// The readings' instance, and how many more frames unused it stays.
    reading: Mutex<Option<(Runner, u32)>>,
    /// The instance the views live in, made with the first view and dropped with the last.
    views: Mutex<Option<Arc<Mutex<Runner>>>>,
    /// The views alive in it.
    open_views: AtomicUsize,
    /// What its extension declared a job of something it registered may take, by its qualified name; and the most of
    /// those its views may take, as they share one instance.
    caps: std::collections::HashMap<String, u64>,
    views_cap: u64,
    /// Every instance's meter, while it lives.
    meters: Mutex<Vec<Weak<Meter>>>,
    peak: AtomicUsize,
    /// The settings kept for its extension, by scope (TOML): every instance's, read as it registers.
    settings: Arc<str>,
    /// What it is outside the sandbox, where it is.
    unsandboxed: Option<Arc<Unsandboxed>>,
}

impl Shared {
    /// What a job of something its extension registered may take, by its name within the extension.
    fn cap(&self, local: &str) -> u64 {
        self.caps.get(&format!("{}@{local}", self.id)).copied().unwrap_or(MEMORY)
    }

    /// Have what an unsandboxed extension requires fetched before it runs (`Unsandboxed::have_required`).
    fn have_required(&self) -> Result<()> {
        self.unsandboxed
            .as_ref()
            .map_or(Ok(()), |unsandboxed| unsandboxed.have_required().with_context(|| self.id.clone()))
    }

    /// The time a long call of it may take, in ticks.
    fn long(&self) -> u64 {
        if self.unsandboxed.is_some() { UNSANDBOXED_LONG } else { LONG }
    }

    /// A new instance for a job that may take `cap` bytes; its extension registers when a call first needs it.
    fn instantiate(&self, cap: u64) -> Result<Runner> {
        let _span = tracing::info_span!("instantiate", of = %self.id).entered();
        let stderr = MemoryOutputPipe::new(64 << 10);
        let mut wasi = WasiCtx::builder();
        wasi.stderr(stderr.clone());
        if self.unsandboxed.is_some() {
            // It sees the system as a program does.
            wasi.inherit_env().inherit_network().allow_blocking_current_thread(true);
            wasi.preopened_dir("/", "/", wasmtime_wasi::FsPerms::ReadWrite)?;
        } else {
            wasi.wall_clock(Still).monotonic_clock(Still);
        }
        let wasi = wasi.build();
        let meter = Arc::new(Meter::default());
        if let Ok(mut meters) = self.meters.lock() {
            meters.retain(|m| m.strong_count() > 0);
            meters.push(Arc::downgrade(&meter));
        }
        let limits = Limits { meter: meter.clone(), cap: cap.min(MOST_MEMORY) };
        let state = State {
            id: self.id.clone(),
            wasi,
            table: ResourceTable::new(),
            limits,
            frames: vec![],
            found: vec![],
            settings: self.settings.clone(),
            unsandboxed: self.unsandboxed.clone(),
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(LONG);
        let runtime =
            Runtime(std::mem::ManuallyDrop::new(tokio::runtime::Builder::new_current_thread().enable_all().build()?));
        let instance = drive(&runtime, self.pre.instantiate_async(&mut store))?;
        Ok(Runner { store, instance, runtime, meter, stderr })
    }

    /// A job that may take `cap` bytes: run on an instance made for it, dropped when it returns.
    ///
    /// Within a chain of steps (`schedule::chain`), the instance the last step left goes on - the chain's own, made for as
    /// much - and this one is left for the next, where it ran well and holds no more than a resident instance may.
    fn job<R>(&self, cap: u64, call: impl FnOnce(&mut Runner) -> Result<R>) -> Result<R> {
        let _span = tracing::info_span!("job", of = %self.id).entered();
        self.have_required()?;
        let key = format!("{}/{cap}", self.id);
        let mut runner = match crate::schedule::take_held::<Runner>(&key) {
            Some(runner) => runner,
            None => self.instantiate(cap).with_context(|| self.id.clone())?,
        };
        let out = call(&mut runner).with_context(|| self.id.clone());
        self.peak.fetch_max(runner.bytes(), Ordering::Relaxed);
        if out.is_ok() && runner.goes_on() {
            let _ = crate::schedule::keep_held(&key, runner);
        }
        out
    }

    /// A reading: run on the readings' instance, made if there is none, its frames reset; a reading while it is busy
    /// on another thread is a job. One that failed, or grew past the budget, goes.
    fn read<R>(&self, call: impl FnOnce(&mut Runner) -> Result<R>) -> Result<R> {
        let _span = tracing::info_span!("reading", of = %self.id).entered();
        let Ok(mut reading) = self.reading.try_lock() else { return self.job(MEMORY, call) };
        self.have_required()?;
        let mut runner = match reading.take() {
            Some((runner, _)) => runner,
            None => self.instantiate(MEMORY).with_context(|| self.id.clone())?,
        };
        let out = call(&mut runner).with_context(|| self.id.clone());
        self.peak.fetch_max(runner.bytes(), Ordering::Relaxed);
        if out.is_ok() && runner.bytes() <= RESIDENT {
            *reading = Some((runner, READING_FRAMES));
        }
        out
    }

    /// A frame ended: the readings' instance, unused for `READING_FRAMES` frames, is reclaimed.
    pub fn frame(&self) {
        if let Ok(mut reading) = self.reading.try_lock() {
            match reading.as_mut() {
                Some((_, 0)) => *reading = None,
                Some((_, left)) => *left -= 1,
                None => {}
            }
        }
    }

    /// Reclaim the readings' instance now: where there are no frames (the agents' server, between its requests).
    pub fn release(&self) {
        if let Ok(mut reading) = self.reading.lock() {
            *reading = None;
        }
    }

    /// What its instances hold now, and the most one has held.
    pub fn memory(&self) -> Memory {
        let meters: Vec<Arc<Meter>> =
            self.meters.lock().map_or_default(|m| m.iter().filter_map(Weak::upgrade).collect());
        let bytes: usize = meters.iter().map(|m| m.bytes.load(Ordering::Relaxed)).sum();
        let largest = meters.iter().map(|m| m.bytes.load(Ordering::Relaxed)).max().unwrap_or(0);
        Memory { instances: meters.len(), bytes, peak: self.peak.load(Ordering::Relaxed).max(largest) }
    }

    /// A view opened in the views' instance.
    fn view_opened(&self) {
        self.open_views.fetch_add(1, Ordering::Relaxed);
    }

    /// A view closed: with the last, the views' instance goes - if it says it may (it may hold what it cannot
    /// rebuild cheaply: a scene its next view shows).
    fn view_closed(&self) {
        if self.open_views.fetch_sub(1, Ordering::Relaxed) != 1 {
            return;
        }
        let Ok(mut views) = self.views.lock() else { return };
        let reclaimable = views.as_ref().is_none_or(|runner| {
            runner
                .lock()
                .ok()
                .is_none_or(|mut r| r.call(QUICK, async |a, g| g.call_reclaimable(a).await).unwrap_or(true))
        });
        if reclaimable {
            *views = None;
        }
    }

    /// The views' instance.
    fn views(&self) -> Result<Arc<Mutex<Runner>>> {
        let mut views = self.views.lock().map_err(|_| anyhow!("{}: its views are poisoned", self.id))?;
        if let Some(runner) = views.as_ref() {
            return Ok(runner.clone());
        }
        let runner = Arc::new(Mutex::new(self.instantiate(self.views_cap)?));
        *views = Some(runner.clone());
        Ok(runner)
    }

    /// The views' instance failed: the next view gets a new one.
    fn views_failed(&self) {
        if let Ok(mut views) = self.views.lock() {
            *views = None;
        }
    }

    fn format(&self, field: u32, brief: bool, value: &Value) -> Option<String> {
        let text = value.to_string();
        self.read(|runner| {
            runner.call(QUICK, async |accessor, guest| guest.call_format(accessor, field, brief, text).await)
        })
        .ok()
        .flatten()
    }
}

/// A view living in its component's views' instance: of an asset, or a panel's.
struct ComponentView {
    shared: Arc<Shared>,
    runner: Option<Arc<Mutex<Runner>>>,
    state: Option<ResourceAny>,
    failed: Option<String>,
}

impl ComponentView {
    fn new(shared: Arc<Shared>, view: u32, asset: Option<&Asset>) -> ComponentView {
        let mut out = ComponentView { shared, runner: None, state: None, failed: None };
        let made = out.shared.views().and_then(|runner| {
            let state = {
                let mut r = runner.lock().map_err(|_| anyhow!("the views' instance is poisoned"))?;
                match asset {
                    Some(asset) => r.with_asset(asset, FRAME, async |a, g, lent| {
                        g.view_state().call_constructor(a, view, Some(lent)).await
                    }),
                    None => r.call(FRAME, async |a, g| g.view_state().call_constructor(a, view, None).await),
                }
            };
            Ok((runner, state?))
        });
        match made {
            Ok((runner, state)) => {
                out.runner = Some(runner);
                out.state = Some(state);
                out.shared.view_opened();
            }
            Err(e) => {
                out.shared.views_failed();
                out.failed = Some(format!("{}: {e:#}", out.shared.id));
            }
        }
        out
    }
}

impl View for ComponentView {
    fn show(&mut self, ui: &mut pe2_ext::Ui) {
        if let Some(failed) = &self.failed {
            ui.error(failed);
            return;
        }
        let (Some(runner), Some(state)) = (&self.runner, self.state) else { return };
        let lent = ui.asset().map(|asset| {
            // SAFETY: lent for this frame's call alone, and taken back before it returns.
            let source = unsafe { erase_source(asset.source()) };
            LentAsset { source, node_type: asset.node_type().map(str::to_string) }
        });
        // SAFETY: as the asset.
        let project = unsafe { erase_project(ui.project()) };
        // SAFETY: as the asset.
        let host = unsafe { erase_frame(ui.host()) };
        let shown = runner.lock().map_err(|_| anyhow!("the views' instance is poisoned")).and_then(|mut r| {
            let data = r.store.data_mut();
            let asset = lent.map(|lent| data.table.push(lent)).transpose()?;
            let project = data.table.push(LentProject { project })?;
            let frame = data.table.push(Frame::Lent(host))?;
            data.frames = vec![frame.rep()];
            let borrowed = (
                Resource::new_borrow(frame.rep()),
                Resource::new_borrow(project.rep()),
                asset.as_ref().map(|a| Resource::new_borrow(a.rep())),
            );
            let out = r.call(FRAME, async |a, g| {
                let (ui, project, asset) = borrowed;
                g.view_state().call_show(a, state, ui, project, asset).await
            });
            // Children the view left open close before the frame is taken back, innermost first.
            let data = r.store.data_mut();
            while let Some(rep) = data.frames.pop() {
                if rep != frame.rep() {
                    let _ = data.table.delete(Resource::<Frame>::new_own(rep));
                }
            }
            let _ = data.table.delete(frame);
            let _ = data.table.delete(project);
            if let Some(asset) = asset {
                let _ = data.table.delete(asset);
            }
            out
        });
        if let Err(e) = shown {
            self.shared.views_failed();
            self.failed = Some(format!("{}: {e:#}", self.shared.id));
            self.runner = None;
        }
    }
}

impl Drop for ComponentView {
    fn drop(&mut self) {
        if let (Some(runner), Some(state)) = (self.runner.take(), self.state.take()) {
            if let Ok(mut r) = runner.lock() {
                let Runner { store, runtime, .. } = &mut *r;
                let _ = drive(runtime, state.resource_drop_async(&mut *store));
                self.shared.peak.fetch_max(r.bytes(), Ordering::Relaxed);
            }
            drop(runner);
            self.shared.view_closed();
        }
    }
}

/// A stream's generator living in an instance of its own while the stream is read.
struct ComponentGenerator {
    shared: Arc<Shared>,
    runner: Runner,
    resource: Option<ResourceAny>,
    header: Table,
}

impl pe2_ext::Generator for ComponentGenerator {
    fn header(&self) -> Table {
        self.header.clone()
    }

    fn next(&mut self, call: &pe2_ext::Call<'_>, count: usize) -> Result<Vec<pe2_ext::Data>> {
        let resource = self.resource.context("closed")?;
        let node = Node::new(call.host);
        let asset = Asset::new(&node, call.host.node_type());
        let count = u32::try_from(count).unwrap_or(u32::MAX);
        let made = self.runner.with_asset(&asset, self.shared.long(), async |a, g, lent| {
            g.generator().call_next(a, resource, lent, count).await
        })?;
        self.shared.peak.fetch_max(self.runner.bytes(), Ordering::Relaxed);
        let table = &mut self.runner.store.data_mut().table;
        made.map_err(|e| anyhow!(e))?.into_iter().map(|v| value_from_wit(v, table)).collect()
    }

    fn seek(&mut self, call: &pe2_ext::Call<'_>, position: u64) -> Result<bool> {
        let resource = self.resource.context("closed")?;
        let node = Node::new(call.host);
        let asset = Asset::new(&node, call.host.node_type());
        let moved = self
            .runner
            .with_asset(&asset, LONG, async |a, g, lent| g.generator().call_seek(a, resource, lent, position).await)?;
        moved.map_err(|e| anyhow!(e))
    }
}

impl Drop for ComponentGenerator {
    fn drop(&mut self) {
        if let Some(resource) = self.resource.take() {
            let Runner { store, runtime, .. } = &mut self.runner;
            let _ = drive(runtime, resource.resource_drop_async(&mut *store));
        }
    }
}

/// An extraction session in an instance of its own, which keeps what the session read between its assets.
struct ComponentExtraction {
    id: String,
    runner: Runner,
    state: ResourceAny,
}

impl ComponentExtraction {
    fn begin(shared: &Shared, extractor: u32, cap: u64, declared: &dyn Declared) -> Result<ComponentExtraction> {
        let mut runner = shared.instantiate(cap)?;
        // SAFETY: the pointer is used only during this call, and taken back before it returns.
        let declared = unsafe { erase_declared(declared) };
        let lent = runner.store.data_mut().table.push(LentDeclarations { declared })?;
        let borrowed = Resource::new_borrow(lent.rep());
        let begun = runner.call(EXTRACTION, async |a, g| g.extraction().call_begin(a, extractor, borrowed).await);
        let _ = runner.store.data_mut().table.delete(lent);
        let state = begun?.map_err(|e| anyhow!(e))?;
        Ok(ComponentExtraction { id: shared.id.clone(), runner, state })
    }
}

impl Extraction for ComponentExtraction {
    fn extract(&mut self, path: &str, release: &str, discs: &mut dyn Discs) -> pe2_ext::Result<pe2_ext::Made> {
        // SAFETY: the pointer is used only during this call, and taken back before it returns.
        let discs = unsafe { erase_discs(discs) };
        let lent = self.runner.store.data_mut().table.push(LentDiscs { discs })?;
        let borrowed = Resource::new_borrow(lent.rep());
        let (state, path, release) = (self.state, path.to_string(), release.to_string());
        let made = self
            .runner
            .call(EXTRACTION, async |a, g| g.extraction().call_extract(a, state, path, release, borrowed).await);
        let _ = self.runner.store.data_mut().table.delete(lent);
        made.with_context(|| self.id.clone())?.map_err(|e| anyhow!(e))
    }
}

impl Drop for ComponentExtraction {
    fn drop(&mut self) {
        let Runner { store, runtime, .. } = &mut self.runner;
        let _ = drive(runtime, self.state.resource_drop_async(&mut *store));
    }
}

/// Register a component's extractors, their sessions begun in instances of their own.
fn apply_extractors(registry: &mut Registry, extractors: Vec<g::Extractor>, shared: &Arc<Shared>) -> Result<()> {
    for extractor in extractors {
        let node_type = Name::parse(&extractor.node_type)
            .with_context(|| format!("{}: not a qualified type", extractor.node_type))?;
        let requires: Vec<&str> = extractor.requires.iter().map(String::as_str).collect();
        let (shared, handle) = (shared.clone(), extractor.handle);
        let cap = shared.cap(&extractor.name);
        let _ = registry.extractor(&extractor.name, &node_type, &requires, move |declared| {
            Ok(Box::new(ComponentExtraction::begin(&shared, handle, cap, declared)?) as Box<dyn Extraction>)
        });
    }
    Ok(())
}

/// A tool's or a service's work, as a job of its component with the project lent: its arguments to its answer.
/// Tests run: a service may be one, as long as what it drives takes.
static PATIENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// What a service may take from now on: tests run in this process.
pub fn be_patient() {
    PATIENT.store(true, std::sync::atomic::Ordering::Relaxed);
}

fn patience() -> u64 {
    if PATIENT.load(std::sync::atomic::Ordering::Relaxed) { UNSANDBOXED_LONG } else { LONG }
}

fn called(
    shared: &Arc<Shared>,
    handle: u32,
    name: &str,
    service: bool,
) -> impl Fn(&dyn Project, &Value) -> Result<Value> + Send + Sync + 'static {
    let (shared, name) = (shared.clone(), name.to_string());
    let cap = shared.cap(&name);
    move |project, arguments| {
        let arguments = arguments.to_string();
        let answer = shared.job(cap, |r| {
            let lent = r.lend_project(project)?;
            let borrowed = Resource::new_borrow(lent.rep());
            let out = if service {
                r.call(patience(), async |a, g| g.call_serve(a, handle, borrowed, arguments).await)
            } else {
                r.call(LONG, async |a, g| g.call_run(a, handle, borrowed, arguments).await)
            };
            r.unlend_project(lent);
            out
        })?;
        let answer = answer.map_err(|e| anyhow!(e))?;
        parse_value(&answer).with_context(|| format!("{name}: its answer is not TOML"))
    }
}

/// What an extension declared its jobs may take, by qualified name: only of what it registers, at most `MOST_MEMORY`.
fn caps(id: &str, registration: &g::Registration) -> Result<std::collections::HashMap<String, u64>> {
    let mut out = std::collections::HashMap::new();
    for (name, bytes) in &registration.memory {
        let qualified = Name::parse(name).with_context(|| format!("memory for {name}: not a qualified name"))?;
        anyhow::ensure!(qualified.extension() == id, "memory for {name}: not something {id} registers");
        out.insert(name.clone(), (*bytes).min(MOST_MEMORY));
    }
    Ok(out)
}

/// What the views' instance may take: the most any type or panel with views was given, as they share it.
fn views_cap(id: &str, registration: &g::Registration, caps: &std::collections::HashMap<String, u64>) -> u64 {
    let with_views = registration
        .types
        .iter()
        .filter(|k| {
            !k.views.is_empty() || !k.inspector.is_empty() || !k.editors.is_empty() || !k.contents_views.is_empty()
        })
        .map(|k| k.name.as_str())
        .chain(registration.panels.iter().map(|p| p.name.as_str()));
    with_views.filter_map(|local| caps.get(&format!("{id}@{local}")).copied()).fold(MEMORY, u64::max)
}

/// A field as its component describes it, its readings called back.
fn field(shared: &Arc<Shared>, f: g::Field) -> Field {
    let mut out = Field::new(&f.key);
    out.label = f.label;
    out.read_only = f.read_only;
    out.required = f.required;
    out.custom = f.custom;
    out.choices = f.choices.into_iter().filter_map(|(value, label)| Some((parse_value(&value)?, label))).collect();
    if f.format {
        let shared = shared.clone();
        out = out.format(move |value| shared.format(f.handle, false, value));
    }
    if f.brief {
        let shared = shared.clone();
        out = out.brief(move |value| shared.format(f.handle, true, value));
    }
    out
}

fn icon(icon: types::Icon) -> pe2_ext::Icon {
    match icon {
        types::Icon::Disc => pe2_ext::Icon::Disc,
        types::Icon::Stack => pe2_ext::Icon::Stack,
        types::Icon::Image => pe2_ext::Icon::Image,
        types::Icon::Palette => pe2_ext::Icon::Palette,
        types::Icon::Grid => pe2_ext::Icon::Grid,
        types::Icon::Film => pe2_ext::Icon::Film,
        types::Icon::Waveform => pe2_ext::Icon::Waveform,
        types::Icon::Speaker => pe2_ext::Icon::Speaker,
        types::Icon::Music => pe2_ext::Icon::Music,
        types::Icon::Package => pe2_ext::Icon::Package,
        types::Icon::Cube => pe2_ext::Icon::Cube,
        types::Icon::Bone => pe2_ext::Icon::Bone,
        types::Icon::Polygon => pe2_ext::Icon::Polygon,
        types::Icon::Terminal => pe2_ext::Icon::Terminal,
        types::Icon::Text => pe2_ext::Icon::Text,
        types::Icon::Binary => pe2_ext::Icon::Binary,
        types::Icon::File => pe2_ext::Icon::File,
        types::Icon::Folder => pe2_ext::Icon::Folder,
        types::Icon::CaretUp => pe2_ext::Icon::CaretUp,
        types::Icon::CaretDown => pe2_ext::Icon::CaretDown,
        types::Icon::Cross => pe2_ext::Icon::Cross,
        types::Icon::Plus => pe2_ext::Icon::Plus,
    }
}

fn tint(tint: types::Tint) -> pe2_ext::Tint {
    match tint {
        types::Tint::Structure => pe2_ext::Tint::Structure,
        types::Tint::Picture => pe2_ext::Tint::Picture,
        types::Tint::Motion => pe2_ext::Tint::Motion,
        types::Tint::Sound => pe2_ext::Tint::Sound,
        types::Tint::Code => pe2_ext::Tint::Code,
        types::Tint::Plain => pe2_ext::Tint::Plain,
    }
}

/// Register what a component registered: its callbacks are calls into it.
/// A view of an asset, made in its component.
/// What is inside an asset as a component said.
fn contents_of(contents: g::AssetContents) -> pe2_ext::Contents {
    let action = |a: g::ContentAction| pe2_ext::ContentAction {
        id: a.id,
        label: a.label,
        icon: a.icon.map(icon),
        on_row: a.on_row,
        on: a.on,
    };
    let rows = contents.rows.into_iter().map(|r| pe2_ext::ContentRow {
        id: r.id,
        label: r.label,
        detail: r.detail,
        hint: r.hint,
        depth: r.depth,
        style: match r.style {
            g::ContentStyle::Item => pe2_ext::ContentStyle::Item,
            g::ContentStyle::Dim => pe2_ext::ContentStyle::Dim,
            g::ContentStyle::Heading => pe2_ext::ContentStyle::Heading,
        },
        icon: r.icon.map(|(i, t)| (icon(i), tint(t))),
        picture: r.picture,
        open: r.open,
        look: r.look,
        link: r.link,
        rename: r.rename,
        drag: r.drag.map(|drag| pe2_ext::ContentDrag { what: drag.what, reorders: drag.reorders }),
        takes: r.takes,
        actions: r.actions.into_iter().map(action).collect(),
    });
    let actions = contents.actions.into_iter().map(action).collect();
    pe2_ext::Contents { rows: rows.collect(), actions, stack: contents.stack, problem: None }
}

fn content_request(request: pe2_ext::ContentRequest) -> g::ContentRequest {
    match request {
        pe2_ext::ContentRequest::Action { row, action } => g::ContentRequest::Action((row, action)),
        pe2_ext::ContentRequest::Rename { row, name } => g::ContentRequest::Rename((row, name)),
        pe2_ext::ContentRequest::Dropped { row, asset } => g::ContentRequest::Dropped((row, asset)),
        pe2_ext::ContentRequest::Moved { row, onto, place } => {
            let place = match place {
                pe2_ext::ContentPlace::Before => g::ContentPlace::Before,
                pe2_ext::ContentPlace::Inside => g::ContentPlace::Inside,
                pe2_ext::ContentPlace::After => g::ContentPlace::After,
            };
            g::ContentRequest::Moved((row, onto, place))
        }
    }
}

/// A value a component gave, by what it is the value of: one that does not parse is an error, never a removal or a part of an edit.
fn parsed(text: &str, of: &str) -> Result<Value> {
    parse_value(text).ok_or_else(|| anyhow!("{of}: its value does not parse: {text}"))
}

/// What comes of something done in the contents: all of it, or nothing.
fn content_outcome(outcome: g::ContentOutcome) -> Result<pe2_ext::ContentOutcome> {
    let mut sets = vec![];
    for set in outcome.sets {
        let value = set.value.as_deref().map(|text| parsed(text, &set.path.join("."))).transpose()?;
        sets.push((set.path, value));
    }
    Ok(pe2_ext::ContentOutcome { label: outcome.label, sets, writes: outcome.writes, select: outcome.select })
}

fn part_sheet_of(sheet: g::PartSheet) -> Result<pe2_ext::PartSheet> {
    let mut fields = vec![];
    for f in sheet.fields {
        let value = f.value.as_deref().map(|text| parsed(text, &f.key)).transpose()?;
        let choices =
            f.choices.into_iter().map(|(value, label)| Ok((parsed(&value, &f.key)?, label))).collect::<Result<_>>()?;
        fields.push(pe2_ext::PartField {
            key: f.key,
            label: f.label,
            value,
            absent: f.absent,
            choices,
            read_only: f.read_only,
        });
    }
    Ok(pe2_ext::PartSheet { within: sheet.within, fields, problem: None })
}

/// A view a component declared, with the place it has of its own.
fn view_def(shared: &Arc<Shared>, v: &g::View) -> pe2_ext::ViewDef {
    let tab = v.tab.as_ref().map(|tab| pe2_ext::TabDef { icon: icon(tab.icon), fields: tab.fields.clone() });
    pe2_ext::ViewDef { name: v.name.clone(), make: Arc::new(view(shared, v.handle)), tab }
}

fn view(shared: &Arc<Shared>, handle: u32) -> impl Fn(&Asset) -> Box<dyn View> + Send + Sync + 'static {
    let shared = shared.clone();
    move |asset: &Asset| -> Box<dyn View> { Box::new(ComponentView::new(shared.clone(), handle, Some(asset))) }
}

/// A setting as the guest described it.
fn setting(s: g::Setting) -> Result<pe2_ext::Setting> {
    let parse = |text: &str| parse_value(text).with_context(|| format!("setting {}: not a TOML value: {text}", s.key));
    let mut setting = pe2_ext::Setting::new(&s.key, parse(&s.default)?);
    setting.label = s.label;
    setting.help = s.help;
    if matches!(s.scope, g::Scope::User) {
        setting = setting.user();
    }
    for (value, label) in s.choices {
        setting.choices.push((parse(&value)?, label));
    }
    Ok(setting)
}

fn apply(registry: &mut Registry, registration: g::Registration, shared: &Arc<Shared>) -> Result<()> {
    // Its settings, taken by the host as the guest took them: from the same values kept.
    for s in registration.settings {
        let _ = registry.setting(setting(s)?);
    }
    for (ty, viewer) in &registration.viewers {
        registry.viewer_def(ty, view_def(shared, viewer));
    }
    for (name, label) in &registration.events {
        registry.add_event(name, label);
    }
    for hook in registration.hooks {
        registry.add_hook(pe2_ext::HookDef { event: hook.event, op: hook.op, signals: hook.signals });
    }
    for template in &registration.templates {
        let files: Vec<&str> = template.files.iter().map(String::as_str).collect();
        drop(
            registry
                .template(&template.name, &template.node_type, &template.stack)
                .label(&template.label)
                .files(&files),
        );
    }
    for (label, tool) in &registration.commands {
        registry.command(label, tool);
    }
    apply_types(registry, registration.types, shared);
    for panel in registration.panels {
        let (shared, handle) = (shared.clone(), panel.handle);
        let icon = panel.icon.map_or(pe2_ext::Icon::Grid, icon);
        registry
            .panel(&panel.name, &panel.label, icon, move || Box::new(ComponentView::new(shared.clone(), handle, None)));
    }
    apply_ops(registry, registration.ops, shared);
    apply_extractors(registry, registration.extractors, shared)?;
    for service in registration.services {
        let run = called(shared, service.handle, &service.name, true);
        let _ = registry.service(&service.name, &service.description, &service.schema, run);
    }
    for tool in registration.tools {
        let run = called(shared, tool.handle, &tool.name, false);
        registry.tool(&tool.name, &tool.description, &tool.schema, run);
    }
    Ok(())
}

/// A type's contents, what is done to them and its parts' sheets, where it says it has them (`contents`, `part_sheet`):
/// each a call into the component, by the type's place.
fn with_parts<'a>(
    mut builder: pe2_ext::TypeBuilder<'a>,
    (contents, part_sheet): (bool, bool),
    index: u32,
    shared: &Arc<Shared>,
) -> pe2_ext::TypeBuilder<'a> {
    if contents {
        let lister = shared.clone();
        builder = builder.contents(move |asset, project| {
            let contents = lister.read(|r| {
                r.with_asset_in(asset, project, QUICK, async |a, g, lent, project| {
                    g.call_contents(a, index, lent, project).await
                })
            });
            contents.map_or_else(
                |why| pe2_ext::Contents { problem: Some(format!("{why:#}")), ..pe2_ext::Contents::default() },
                contents_of,
            )
        });
        let shared = shared.clone();
        builder = builder.content_act(move |asset, project, request| {
            let request = content_request(request);
            let done = shared.read(|r| {
                r.with_asset_in(asset, project, QUICK, async |a, g, lent, project| {
                    g.call_content_act(a, index, lent, project, request.clone()).await
                })
            })?;
            content_outcome(done.map_err(|why| anyhow!(why))?)
        });
    }
    if part_sheet {
        let shared = shared.clone();
        builder = builder.part_sheet(move |asset, project, part| {
            let sheet = shared.read(|r| {
                r.with_asset_in(asset, project, QUICK, async |a, g, lent, project| {
                    g.call_part_sheet_of(a, index, lent, project, part.to_string()).await
                })
            });
            let problem = |why: anyhow::Error| pe2_ext::PartSheet {
                problem: Some(format!("{why:#}")),
                ..pe2_ext::PartSheet::default()
            };
            match sheet {
                Ok(sheet) => sheet.map(|sheet| part_sheet_of(sheet).unwrap_or_else(problem)),
                Err(why) => Some(problem(why)),
            }
        });
    }
    builder
}

/// Register a component's types; what they compute calls into it, by each one's place.
fn apply_types(registry: &mut Registry, types: Vec<g::ValueType>, shared: &Arc<Shared>) {
    for node_type in types {
        let index = node_type.handle;
        let builder =
            if node_type.extends { registry.extend(&node_type.name) } else { registry.named_type(&node_type.name) };
        let mut builder = builder.label(&node_type.label);
        if let Some(media_type) = &node_type.media_type {
            builder = builder.media_type(media_type);
        }
        for signature in &node_type.signatures {
            builder = match signature {
                g::Signature::Bytes((offset, bytes)) => builder.bytes(*offset as usize, bytes),
                g::Signature::Name(name) => builder.named(name),
                g::Signature::Text => builder.text(),
            };
        }
        if node_type.sniff {
            let shared = shared.clone();
            builder = builder.sniff(move |name, size, head| {
                let (name, head) = (name.to_string(), head.to_vec());
                let version = shared
                    .read(|r| r.call(QUICK, async |a, g| g.call_sniff(a, index, name, size, head).await))
                    .ok()
                    .flatten()?;
                Some(pe2_ext::Recognized { version: (!version.is_empty()).then_some(version) })
            });
        }
        if node_type.data {
            builder = builder.data();
        }
        for value in &node_type.values {
            builder = builder.of(value);
        }
        if let Some(alias) = &node_type.alias {
            builder = builder.alias(alias);
        }
        if let Some((i, t)) = node_type.icon {
            builder = builder.icon(icon(i), tint(t));
        }
        for f in node_type.fields {
            builder = builder.field(field(shared, f));
        }
        if node_type.extracted {
            builder = builder.extracted();
        }
        let takes: Vec<&str> = node_type.takes.iter().map(String::as_str).collect();
        builder = builder.takes(&takes);
        if let Some(stack) = &node_type.stack {
            builder = builder.stack(stack);
        }
        builder = with_parts(builder, (node_type.contents, node_type.part_sheet), index, shared);
        for v in node_type.views {
            builder = builder.view(&v.name, view(shared, v.handle));
        }
        for v in &node_type.inspector {
            builder = builder.inspector_def(view_def(shared, v));
        }
        for v in &node_type.editors {
            builder = builder.editor_def(view_def(shared, v));
        }
        for v in &node_type.contents_views {
            builder = builder.contents_view_def(view_def(shared, v));
        }
        let _ = builder.register();
    }
}

/// Register a component's types, with what recognizes them in files; their sniffs call into it.
/// Register a component's types and operations, their runs calls into it: each lent its node as an asset whose files
/// are its values' contents, named by their hashes.
fn apply_ops(registry: &mut Registry, ops: Vec<g::Op>, shared: &Arc<Shared>) {
    for op in ops {
        let mut builder = registry.op(&op.name).label(&op.label).priority(op.priority);
        for port in &op.inputs {
            builder = builder.port(&port.name, &port.ty, port.many);
        }
        builder = builder.output_described(&op.output);
        for f in op.fields {
            builder = builder.field(field(shared, f));
        }
        for path in &op.derives {
            builder = builder.derives(path);
        }
        for (name, ty) in &op.also {
            builder = builder.also_described(name, ty);
        }
        if let Some(inverse) = &op.inverse {
            builder = builder.inverse_described(inverse);
        }
        if let Some((name, node_type)) = &op.parameter {
            builder = builder.parameter_described(name, node_type);
        }
        if op.members {
            let (shared, handle) = (shared.clone(), op.handle);
            builder = builder.members_described(move |asset| {
                shared
                    .read(|r| r.with_asset(asset, QUICK, async |a, g, lent| g.call_members(a, handle, lent).await))
                    .unwrap_or_default()
            });
        }
        if let Some(provides) = &op.provides {
            builder = builder.provides(&provides.name).keeps(match provides.retain {
                g::Retain::Cache => pe2_ext::Retain::Cache,
                g::Retain::Build => pe2_ext::Retain::Build,
                g::Retain::Transient => pe2_ext::Retain::Transient,
            });
        }
        let (cap, shared, handle) = (shared.cap(&op.name), shared.clone(), op.handle);
        if op.stepped {
            builder = builder.stepped();
        }
        if op.prepares {
            let shared = shared.clone();
            builder = builder.prepare(move |call| {
                let given = call.inputs.clone();
                let settings = toml::to_string(call.settings).unwrap_or_default();
                let parameter = call.parameter_text.map(str::to_string);
                let node = Node::new(call.host);
                let asset = Asset::new(&node, call.host.node_type());
                let prepared = shared.job(cap, |r| {
                    let inputs = values_to_wit(given, &mut r.store.data_mut().table)?;
                    r.with_asset(&asset, shared.long(), async |a, g, lent| {
                        g.call_prepare_op(a, handle, lent, inputs, settings, parameter).await
                    })
                })?;
                prepared.map(prepared_from_wit).map_err(|e| anyhow!(e))
            });
        }
        if op.output.starts_with("stream<") {
            let _ = builder.streams_described(move |call| {
                let given = std::mem::take(&mut call.inputs);
                let settings = toml::to_string(call.settings).unwrap_or_default();
                let parameter = call.parameter_text.map(str::to_string);
                let node = Node::new(call.host);
                let asset = Asset::new(&node, call.host.node_type());
                // An instance of its own, kept while the stream is read.
                let mut runner = shared.instantiate(cap).with_context(|| shared.id.clone())?;
                let inputs = values_to_wit(given, &mut runner.store.data_mut().table)?;
                let started = runner.with_asset(&asset, LONG, async |a, g, lent| {
                    g.call_start_stream(a, handle, lent, inputs, settings, parameter).await
                })?;
                let resource = started.map_err(|e| anyhow!(e))?;
                let header = runner.call(QUICK, async |a, g| g.generator().call_header(a, resource).await)?;
                let header = toml::from_str(&header).unwrap_or_default();
                Ok(Box::new(ComponentGenerator { shared: shared.clone(), runner, resource: Some(resource), header })
                    as Box<dyn pe2_ext::Generator>)
            });
            continue;
        }
        let _ = builder.run_described(move |call| {
            let given = std::mem::take(&mut call.inputs);
            let settings = toml::to_string(call.settings).unwrap_or_default();
            let parameter = call.parameter_text.map(str::to_string);
            let node = Node::new(call.host);
            let asset = Asset::new(&node, call.host.node_type());
            shared.job(cap, |r| {
                let inputs = values_to_wit(given, &mut r.store.data_mut().table)?;
                let made = r.with_asset(&asset, shared.long(), async |a, g, lent| {
                    g.call_run_op(a, handle, lent, inputs, settings, parameter).await
                })?;
                let mut made = value_from_wit(made.map_err(|e| anyhow!(e))?, &mut r.store.data_mut().table)?;
                // A step ended at a cut, what it carries held by the instance: written out only where the next step will
                // not run on it.
                if pe2_ext::stepped::is_record(&made) && !r.goes_on() {
                    let _span = tracing::info_span!("seal step", of = %shared.id).entered();
                    let sealed = r.call(QUICK, async |a, g| g.call_seal_step(a).await)?.map_err(|e| anyhow!(e))?;
                    if let Some(sealed) = sealed {
                        pe2_ext::stepped::add_sealed(&mut made, sealed);
                    }
                }
                Ok(made)
            })
        });
    }
}

/// A node as an operation's run lends it to a component: its path, type and declared settings; its files the
/// contents its values hold, named by their hashes.
struct Node<'a> {
    host: &'a dyn pe2_ext::CallHost,
    declaration: String,
}

impl<'a> Node<'a> {
    fn new(host: &'a dyn pe2_ext::CallHost) -> Node<'a> {
        Node { host, declaration: String::new() }
    }

    fn content(name: &str) -> pe2_ext::Content {
        pe2_ext::Content::Stored { sha1: name.to_string(), size: 0 }
    }
}

impl Source for Node<'_> {
    fn path(&self) -> &str {
        self.host.path()
    }

    fn uid(&self) -> Option<&str> {
        None
    }

    fn fields(&self) -> &Table {
        static NONE: std::sync::OnceLock<Table> = std::sync::OnceLock::new();
        NONE.get_or_init(Table::new)
    }

    fn declaration(&self) -> &str {
        &self.declaration
    }

    fn has_file(&self, _: &str) -> bool {
        true
    }

    fn read(&self, name: &str) -> Result<Vec<u8>> {
        self.host.read(&Node::content(name))
    }

    fn size(&self, _: &str) -> Option<u64> {
        None
    }

    fn read_range(&self, name: &str, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.host.read_range(&Node::content(name), offset, len)
    }

    fn writer(&self, name: &str) -> Option<Result<Box<dyn pe2_ext::FileWriter>>> {
        Some(self.host.writer(name))
    }

    fn temp(&self) -> Option<Result<String>> {
        Some(self.host.temp())
    }

    fn entries(&self, folder: &str) -> Option<Result<Vec<String>>> {
        Some(self.host.entries(folder))
    }

    fn read_entry(&self, path: &str) -> Option<Result<Vec<u8>>> {
        Some(self.host.read_entry(path))
    }

    fn place(&self, path: &str) -> Option<String> {
        self.host.place(path)
    }

    fn node_named(&self, reference: &str) -> Option<pe2_ext::NodeNamed> {
        self.host.node_named(reference)
    }

    fn output_of(&self, path: &str, address: &str) -> Option<Result<pe2_ext::Data>> {
        Some(self.host.output_of(path, address))
    }

    fn build_config(&self) -> Option<pe2_ext::data::BuildConfig> {
        self.host.build_config()
    }

    fn assets_of(&self, node_type: &str) -> Option<Result<Vec<(String, String)>>> {
        Some(self.host.assets_of(node_type))
    }

    fn keep(&self, path: &str, name: &str) -> Option<Result<pe2_ext::File>> {
        Some(self.host.keep(path, name))
    }

    fn path_of(&self, name: &str) -> Option<String> {
        self.host.path_of(&Node::content(name))
    }
}

/// What an operation said before it runs, as the contract carried it.
fn prepared_from_wit(prepared: g::Prepared) -> pe2_ext::Prepared {
    let input = |from: g::MicroInput| pe2_ext::MicroInput { micro: from.micro, port: from.port, files: from.files };
    let micro = |micro: g::Micro| pe2_ext::Micro {
        op: micro.op,
        label: micro.label,
        settings: toml::from_str(&micro.settings).unwrap_or_default(),
        ports: micro.ports.into_iter().map(|port| port.into_iter().map(input).collect()).collect(),
        keep: micro.keep,
    };
    pe2_ext::Prepared {
        needs: prepared
            .needs
            .into_iter()
            .map(|need| pe2_ext::Need { node: need.node, address: need.address })
            .collect(),
        micros: prepared.micros.into_iter().map(micro).collect(),
    }
}

/// Each port's values as the contract carries them, their media buffers handed to the instance.
fn values_to_wit(ports: Vec<Vec<pe2_ext::Data>>, table: &mut ResourceTable) -> Result<Vec<Vec<types::Value>>> {
    ports.into_iter().map(|port| port.into_iter().map(|v| value_to_wit(v, table)).collect()).collect()
}

/// A media buffer the host holds, handed to an instance.
fn buffer_to_wit(buffer: &pe2_ext::media::MediaBuffer, table: &mut ResourceTable) -> Result<Resource<HostMediaBuffer>> {
    Ok(table.push(HostMediaBuffer(buffer.bytes().clone()))?)
}

/// A value as the contract carries it: a list's items that are lists themselves are dropped (it carries one level).
fn value_to_wit(value: pe2_ext::Data, table: &mut ResourceTable) -> Result<types::Value> {
    use pe2_ext::Data;
    Ok(match value {
        Data::List(items) => {
            let items: Vec<Option<types::Item>> =
                items.into_iter().map(|v| item_to_wit(v, table)).collect::<Result<_>>()?;
            types::Value::Items(items.into_iter().flatten().collect())
        }
        Data::Image(image) => types::Value::Image(to_wit(image)),
        Data::Table(table) => types::Value::Table(toml::to_string(&table).unwrap_or_default()),
        Data::Pcm(pcm) => {
            types::Value::Pcm(types::Pcm { rate: pcm.rate, channels: pcm.channels, samples: pcm.samples })
        }
        Data::Typed(t) => types::Value::Typed(types::Typed {
            ty: t.ty,
            also: t.also,
            files: t.files.into_iter().map(file_to_wit).collect(),
            settings: toml::to_string(&t.settings).unwrap_or_default(),
            notes: t.notes,
        }),
        Data::Video(frame) => types::Value::Video(media::VideoFrame {
            buffer: buffer_to_wit(&frame.buffer, table)?,
            format: pixel_format_to_wit(frame.format),
            width: frame.width,
            height: frame.height,
            timestamp: frame.timestamp,
        }),
        Data::Audio(sound) => types::Value::Audio(media::AudioData {
            buffer: buffer_to_wit(&sound.buffer, table)?,
            format: sample_format_to_wit(sound.format),
            sample_rate: sound.sample_rate,
            channels: sound.channels,
            timestamp: sound.timestamp,
        }),
        Data::Stream(stream) => types::Value::Stream(table.push(HostStreamReader(stream))?),
        Data::Marks(marks) => types::Value::Marks(types::Marks {
            boxes: marks
                .boxes
                .into_iter()
                .map(|m| types::Mark {
                    name: m.name,
                    rect: types::Rect { x: m.rect.x, y: m.rect.y, width: m.rect.width, height: m.rect.height },
                })
                .collect(),
            summary: marks.summary,
            toggle: marks.toggle,
            legend: marks.legend,
        }),
    })
}

fn item_to_wit(value: pe2_ext::Data, table: &mut ResourceTable) -> Result<Option<types::Item>> {
    Ok(Some(match value_to_wit(value, table)? {
        types::Value::Image(i) => types::Item::Image(i),
        types::Value::Table(t) => types::Item::Table(t),
        types::Value::Pcm(p) => types::Item::Pcm(p),
        types::Value::Typed(t) => types::Item::Typed(t),
        types::Value::Video(v) => types::Item::Video(v),
        types::Value::Audio(a) => types::Item::Audio(a),
        types::Value::Marks(m) => types::Item::Marks(m),
        types::Value::Items(_) | types::Value::Stream(_) => return Ok(None),
    }))
}

fn file_to_wit(f: pe2_ext::File) -> types::File {
    let content = match f.content {
        pe2_ext::Content::Inline(bytes) => types::Content::Inline(bytes),
        pe2_ext::Content::Stored { sha1, size } => types::Content::Stored((sha1, size)),
    };
    types::File { name: f.name, content }
}

/// A value as the contract carried it.
fn value_from_wit(value: types::Value, table: &mut ResourceTable) -> Result<pe2_ext::Data> {
    match value {
        types::Value::Stream(reader) => Ok(pe2_ext::Data::Stream(table.delete(reader)?.0)),
        types::Value::Image(i) => item_from_wit(types::Item::Image(i), table),
        types::Value::Table(t) => item_from_wit(types::Item::Table(t), table),
        types::Value::Pcm(p) => item_from_wit(types::Item::Pcm(p), table),
        types::Value::Typed(t) => item_from_wit(types::Item::Typed(t), table),
        types::Value::Video(v) => item_from_wit(types::Item::Video(v), table),
        types::Value::Audio(a) => item_from_wit(types::Item::Audio(a), table),
        types::Value::Marks(m) => item_from_wit(types::Item::Marks(m), table),
        types::Value::Items(items) => {
            Ok(pe2_ext::Data::List(items.into_iter().map(|i| item_from_wit(i, table)).collect::<Result<_>>()?))
        }
    }
}

/// A media buffer an instance hands over: its handle taken back, the bytes it names kept.
fn buffer_from_wit(
    buffer: Resource<HostMediaBuffer>,
    table: &mut ResourceTable,
) -> Result<pe2_ext::media::MediaBuffer> {
    Ok(pe2_ext::media::MediaBuffer::shared(table.delete(buffer)?.0))
}

fn item_from_wit(item: types::Item, table: &mut ResourceTable) -> Result<pe2_ext::Data> {
    use pe2_ext::Data;
    Ok(match item {
        types::Item::Image(i) => {
            Data::Image(Image { width: i.width as usize, height: i.height as usize, rgba: i.rgba })
        }
        types::Item::Table(t) => Data::Table(toml::from_str(&t).unwrap_or_default()),
        types::Item::Pcm(p) => Data::Pcm(pe2_ext::Pcm { rate: p.rate, channels: p.channels, samples: p.samples }),
        types::Item::Typed(t) => Data::Typed(pe2_ext::Typed {
            ty: t.ty,
            also: t.also,
            files: t.files.into_iter().map(file_from_wit).collect(),
            settings: toml::from_str(&t.settings).unwrap_or_default(),
            notes: t.notes,
        }),
        types::Item::Video(v) => Data::Video(pe2_ext::media::VideoFrame {
            buffer: buffer_from_wit(v.buffer, table)?,
            format: pixel_format_from_wit(v.format),
            width: v.width,
            height: v.height,
            timestamp: v.timestamp,
        }),
        types::Item::Audio(a) => Data::Audio(pe2_ext::media::AudioData {
            buffer: buffer_from_wit(a.buffer, table)?,
            format: sample_format_from_wit(a.format),
            sample_rate: a.sample_rate,
            channels: a.channels,
            timestamp: a.timestamp,
        }),
        types::Item::Marks(m) => Data::Marks(pe2_ext::Marks {
            boxes: m.boxes.into_iter().map(|b| pe2_ext::Mark { name: b.name, rect: rect(b.rect) }).collect(),
            summary: m.summary,
            toggle: m.toggle,
            legend: m.legend,
        }),
    })
}

fn file_from_wit(f: types::File) -> pe2_ext::File {
    let content = match f.content {
        types::Content::Inline(bytes) => pe2_ext::Content::Inline(bytes),
        types::Content::Stored((sha1, size)) => pe2_ext::Content::Stored { sha1, size },
    };
    pe2_ext::File { name: f.name, content }
}

/// Where compiled components are kept: the user's cache (`PE2_CACHE`, else the platform's), shared by every project.
fn compiled_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = std::env::var_os("PE2_CACHE") {
        return Some(std::path::PathBuf::from(dir).join("compiled"));
    }
    let base = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join("Library/Caches"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".cache")))
    };
    Some(base?.join("pe2-editor").join("compiled"))
}

/// Remove compiled components not written for 30 days: those of extensions since rebuilt, mostly; one still used is
/// compiled again.
fn sweep(dir: &Path) {
    const KEPT: Duration = Duration::from_hours(30 * 24);
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > KEPT);
        if old && entry.path().extension().is_some_and(|e| e == "cwasm") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A component compiled: read back from the cache, or compiled and kept. A kept file starts with the SHA-1 of the
/// rest, checked before wasmtime is trusted with it.
fn compile(engine: &Engine, bytes: &[u8]) -> Result<Component> {
    use sha1::{Digest, Sha1};
    use std::hash::{Hash, Hasher};
    let mut settings = std::hash::DefaultHasher::new();
    engine.precompile_compatibility_hash().hash(&mut settings);
    let key = format!("{}-{:016x}", crate::extract::sha1_hex(bytes), settings.finish());
    let file = compiled_dir().map(|dir| dir.join(format!("{key}.cwasm")));
    if let Some(kept) = file.as_ref().and_then(|f| std::fs::read(f).ok())
        && let Some((digest, code)) = kept.split_at_checked(20)
        && Sha1::digest(code).as_slice() == digest
    {
        // SAFETY: what `serialize` wrote for this engine's settings (the key) and these bytes, unchanged since (the
        // digest).
        let _span = tracing::info_span!("component read compiled").entered();
        if let Ok(component) = unsafe { Component::deserialize(engine, code) } {
            return Ok(component);
        }
    }
    let _span = tracing::info_span!("component compiled", bytes = bytes.len()).entered();
    let component = Component::new(engine, bytes).map_err(anyhow::Error::from)?;
    if let Some(file) = file
        && let Ok(code) = component.serialize()
    {
        let mut kept = Sha1::digest(&code).to_vec();
        kept.extend_from_slice(&code);
        if let Some(dir) = file.parent() {
            let _ = std::fs::create_dir_all(dir);
            sweep(dir);
        }
        let _ = crate::cache::write_atomic(&file, &kept);
    }
    Ok(component)
}

/// A component in a file, to load.
///
/// # Errors
/// It cannot be read, compiled or instantiated, or its registration fails.
pub fn load(path: &Path) -> Result<Loadable> {
    let bytes = std::fs::read(path).with_context(|| path.display().to_string())?;
    load_bytes(&path.display().to_string(), &bytes, Origin::Component(path.to_path_buf()))
}

/// Who a component is, as `extension!` writes it into the section the host reads without running it
/// (`pe2-extension`, in its core module): its manifest - its id, version, dependencies, what it runs as, and what is said
/// of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct About {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub weak: Vec<String>,
    #[serde(default)]
    pub optional: Vec<String>,
    #[serde(default)]
    pub provides: Vec<String>,
    #[serde(default)]
    pub platforms: Vec<String>,
    #[serde(default)]
    pub unsandboxed: bool,
    /// What it fetches onto a machine before it runs (`setup`): unsandboxed, the programs it starts.
    #[serde(default)]
    pub setup: Vec<super::setup::Setup>,
    /// What it starts by name that is another program with arguments (`unsandboxed::Launch`).
    #[serde(default)]
    pub launch: Vec<super::unsandboxed::Launch>,
    /// What is said of it, which the code does not know.
    #[serde(flatten)]
    pub described: Described,
}

/// What a manifest says of an extension, for people: nothing of it changes what runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Described {
    #[serde(default)]
    pub description: Option<String>,
    /// Its license, as an SPDX expression.
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default)]
    pub repository: Option<String>,
    /// Its guides' first page, in its folder.
    #[serde(default)]
    pub docs: Option<String>,
    /// What to set up on the machine before it runs, in its folder: where there is nothing, none.
    #[serde(default)]
    pub install: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    /// What it ships of others', each with its license.
    #[serde(default)]
    pub bundled: Vec<Bundled>,
}

/// Something an extension ships of others'.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Deserialize)]
pub struct Bundled {
    pub name: String,
    /// Its license, as an SPDX expression.
    pub license: String,
    /// The license's text, in the extension's folder.
    pub text: String,
}

/// What only an extension running unsandboxed may have: setup, which fetches what the programs it starts are, and
/// launches, which name them. One in the sandbox starts none.
///
/// # Errors
/// It runs in the sandbox, and fetches or launches.
pub fn sandboxed_fetches_nothing(about: &About) -> Result<()> {
    anyhow::ensure!(
        about.unsandboxed || about.setup.is_empty(),
        "it fetches onto the machine (setup) and runs in the sandbox, which starts no program: only an unsandboxed \
         extension fetches"
    );
    anyhow::ensure!(
        about.unsandboxed || about.launch.is_empty(),
        "it names programs to launch and runs in the sandbox, which starts none: only an unsandboxed extension launches"
    );
    Ok(())
}

/// What a component says it is, read from its section; none where it has none (built by an older SDK).
#[must_use]
pub fn about(bytes: &[u8]) -> Option<About> {
    toml::from_str(&manifest_text(bytes)?).ok()
}

/// The manifest a component carries, as written; none where it carries none.
#[must_use]
pub fn manifest_text(bytes: &[u8]) -> Option<String> {
    wasmparser::Parser::new(0).parse_all(bytes).find_map(|payload| match payload.ok()? {
        wasmparser::Payload::CustomSection(section) if section.name() == SECTION => {
            String::from_utf8(section.data().to_vec()).ok()
        }
        _ => None,
    })
}

/// The section `extension!` writes.
const SECTION: &str = "pe2-extension";

/// The contract this host runs: the version of the WIT package (`pe2:extension@0.1.0`), the line it is of (`0.1`)
/// running every component built against a version of the line no newer than it.
pub const CONTRACT: &str = "0.1.0";

/// What a component's guest interface is exported as, before its version.
const GUEST: &str = "pe2:extension/guest@";

/// The contract a component was built against: the version it exports its guest interface at; none where it exports
/// none - no extension.
#[must_use]
pub fn contract(bytes: &[u8]) -> Option<String> {
    wasmparser::Parser::new(0).parse_all(bytes).find_map(|payload| match payload.ok()? {
        wasmparser::Payload::ComponentExportSection(exports) => {
            exports.into_iter().find_map(|e| Some(e.ok()?.name.name.strip_prefix(GUEST)?.to_string()))
        }
        _ => None,
    })
}

/// Whether this host runs a component built against `contract`; why not, said for whoever is to act on it.
///
/// # Errors
/// Another line (older: the component is to be rebuilt; newer: the editor updated), or newer within this one.
pub fn runs(contract: &str) -> std::result::Result<(), String> {
    if super::satisfies(CONTRACT, &format!("^{contract}")) {
        return Ok(());
    }
    let (Some(theirs), Some(ours)) = (super::semver(contract), super::semver(CONTRACT)) else {
        return Err(format!("its contract, {contract}, is no version"));
    };
    let line = |v: (u64, u64, u64)| if v.0 == 0 { format!("0.{}", v.1) } else { v.0.to_string() };
    let (their_line, our_line) = (line(theirs), line(ours));
    Err(if their_line == our_line {
        format!("it needs contract {contract}, newer than this editor's {CONTRACT}: update the editor")
    } else if theirs > ours {
        format!("it is built for contract {their_line}, newer than this editor's {our_line}: update the editor")
    } else {
        format!(
            "it is built for contract {their_line}, and this editor runs {our_line}: rebuild it with this editor's SDK"
        )
    })
}

/// A component to load (`name` names it until it says who it is). One that says who it is in its section is compiled
/// and run only when it registers - an extension disabled, or whose requirements are not met, never is - and must then
/// say the same when asked; one without is compiled, instantiated and asked now.
///
/// # Errors
/// One without the section cannot be compiled or instantiated, or its registration fails.
pub fn load_bytes(name: &str, bytes: &[u8], origin: Origin) -> Result<Loadable> {
    load_with(name, bytes, origin, None)
}

/// What an unsandboxed extension is, beyond its component.
#[derive(Clone, Debug)]
pub struct Unsandboxed {
    /// The folder it is in: what it ships.
    pub folder: Option<std::path::PathBuf>,
    /// Its hash: what the user enabled, and what it makes is kept by.
    pub trust: String,
    /// Its installation notes, where it has any: what a program it cannot start points to.
    pub install: Option<std::path::PathBuf>,
    /// Where what its setup fetches is kept on this machine.
    pub store: Option<std::path::PathBuf>,
    /// What its setup fetches for this platform.
    pub setup: Vec<super::setup::Setup>,
    /// Of what is optional, what the user turned off on this computer, by key.
    pub off: std::collections::BTreeSet<String>,
    /// What it starts by name that is another program, for this platform.
    pub launch: Vec<super::unsandboxed::Launch>,
}

impl Unsandboxed {
    /// What its setup fetched onto this machine: the folders, each laid out as its tree is - looked at as it starts a
    /// program, so that what was fetched since is found.
    fn fetched(&self) -> Vec<std::path::PathBuf> {
        let Some(store) = &self.store else { return vec![] };
        self.setup.iter().filter(|setup| setup.fetched(store)).map(|setup| setup.place(store)).collect()
    }

    /// Have what it requires on this machine before anything of it runs - an operation, a service, a reading - as a
    /// step of what asked for it: what is fetched already is there; what another fetch of this process is downloading (the
    /// editor's, beside) is waited for; what is not, is fetched now. Trusting it consented to it. What is optional is
    /// fetched when it asks for it (`set-up`).
    ///
    /// # Errors
    /// What it requires cannot be fetched: downloaded, held to its hashes and kept.
    fn have_required(&self) -> Result<()> {
        let Some(store) = &self.store else { return Ok(()) };
        let cancel = std::sync::atomic::AtomicBool::new(false);
        for setup in self.setup.iter().filter(|setup| !setup.lazy && !setup.fetched(store)) {
            super::setup::fetch(setup, store, self.folder.as_deref(), &mut |_, _| {}, &cancel)
                .with_context(|| format!("fetching {}, which it requires", setup.name))?;
        }
        Ok(())
    }

    /// What it requires and is not fetched yet, by name.
    fn unset(&self) -> Vec<String> {
        let store = self.store.as_deref();
        let missing = |setup: &&super::setup::Setup| !setup.lazy && store.is_none_or(|store| !setup.fetched(store));
        self.setup.iter().filter(missing).map(|setup| setup.name.clone()).collect()
    }
}

/// Load an extension the user enabled to run outside the sandbox.
///
/// # Errors
/// As `load_bytes`.
pub fn load_unsandboxed(name: &str, bytes: &[u8], origin: Origin, unsandboxed: Unsandboxed) -> Result<Loadable> {
    load_with(name, bytes, origin, Some(Arc::new(unsandboxed)))
}

fn load_with(name: &str, bytes: &[u8], origin: Origin, unsandboxed: Option<Arc<Unsandboxed>>) -> Result<Loadable> {
    let _span = tracing::info_span!("component loaded", name = %name).entered();
    let trust = unsandboxed.as_ref().map(|u| u.trust.clone());
    let build = trust.clone().unwrap_or_else(|| crate::extract::sha1_hex(bytes));
    let (size, build) = (bytes.len(), Some(build));
    // Its contract first, before anything of it is compiled.
    let contract = contract(bytes).context("it exports no pe2:extension/guest: it is no extension")?;
    runs(&contract).map_err(|why| anyhow!(why))?;
    let contract = Some(contract);
    let cell: Arc<OnceLock<Arc<Shared>>> = Arc::default();
    if let Some(about) = about(bytes) {
        sandboxed_fetches_nothing(&about)?;
        let (name, bytes, said, kept) = (name.to_string(), bytes.to_vec(), about.clone(), cell.clone());
        return Ok(Loadable {
            id: about.id,
            version: about.version,
            requires: about.requires,
            weak: about.weak,
            optional: about.optional,
            provides: about.provides,
            origin,
            register: Some(Box::new(move |registry| {
                let settings = Value::Table(registry.stored().clone()).to_string();
                let (shared, told, registration) = start(&name, &bytes, &settings, unsandboxed.clone())?;
                // What counts as it is for the section alone: it is read to know the extension, before it runs.
                let told = About {
                    setup: said.setup.clone(),
                    launch: said.launch.clone(),
                    described: said.described.clone(),
                    ..told
                };
                anyhow::ensure!(told == said, "{name}: it says it is {told:?}, its section {said:?}");
                let shared = kept.get_or_init(|| shared).clone();
                apply(registry, registration, &shared)
            })),
            component: Some(cell),
            size: Some(size),
            contract: contract.clone(),
            build: build.clone(),
            settings: toml::Table::new(),
            trust,
            described: about.described,
            setup: about.setup,
            reaches: Vec::new(),
        });
    }
    // Without its section it is asked who it is now, before the host knows its settings: it registers with none kept.
    let (shared, about, registration) = start(name, bytes, "{}", unsandboxed)?;
    let shared = cell.get_or_init(|| shared).clone();
    Ok(Loadable {
        id: about.id,
        version: about.version,
        requires: about.requires,
        weak: about.weak,
        optional: about.optional,
        provides: about.provides,
        origin,
        register: Some(Box::new(move |registry| apply(registry, registration, &shared))),
        component: Some(cell),
        size: Some(size),
        contract,
        build,
        settings: toml::Table::new(),
        trust,
        described: about.described,
        setup: about.setup,
        reaches: Vec::new(),
    })
}

/// What links `wasi:webgpu`, where the host draws; the project itself has no GPU.
pub type LinkGpu = fn(&mut Linker<State>) -> wasmtime::Result<()>;

static GPU: std::sync::OnceLock<LinkGpu> = std::sync::OnceLock::new();

/// Say what links the GPU, before extensions are started.
pub fn link_gpu(link: LinkGpu) {
    let _ = GPU.set(link);
}

/// A component compiled in an engine of its own, its first instance asked who it is and what it registers - which
/// then answers the first readings.
fn start(
    name: &str,
    bytes: &[u8],
    settings: &str,
    unsandboxed: Option<Arc<Unsandboxed>>,
) -> Result<(Arc<Shared>, About, g::Registration)> {
    let engine = new_engine()?;
    let component = compile(&engine, bytes).with_context(|| name.to_string())?;
    let mut linker = Linker::<State>::new(&engine);
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;
    bindings::Linked::add_to_linker::<State, HasSelf<State>>(&mut linker, |state| state)?;
    if let Some(link) = GPU.get() {
        link(&mut linker)?;
    }
    // What this host has not linked traps when called: only what draws has a GPU.
    linker.define_unknown_imports_as_traps(&component)?;
    let pre = bindings::LinkedPre::new(linker.instantiate_pre(&component)?)?;
    let mut shared = Shared {
        id: name.to_string(),
        engine,
        pre,
        reading: Mutex::new(None),
        views: Mutex::new(None),
        open_views: AtomicUsize::new(0),
        caps: std::collections::HashMap::new(),
        views_cap: MEMORY,
        meters: Mutex::new(vec![]),
        peak: AtomicUsize::new(0),
        settings: settings.into(),
        unsandboxed,
    };
    let mut runner = shared.instantiate(MEMORY)?;
    let told = runner.call(LONG, async |accessor, guest| guest.call_describe(accessor).await)?;
    let registration = runner
        .call(LONG, async |accessor, guest| guest.call_register(accessor).await)?
        .map_err(|e| anyhow!("{name}: its registration failed: {e}"))?;
    shared.id.clone_from(&told.id);
    shared.caps = caps(&told.id, &registration)?;
    shared.views_cap = views_cap(&told.id, &registration, &shared.caps);
    runner.store.data_mut().id.clone_from(&told.id);
    *shared.reading.get_mut().map_err(|_| anyhow!("{name}: its readings are poisoned"))? =
        Some((runner, READING_FRAMES));
    let about = About {
        id: told.id,
        version: told.version,
        requires: told.requires,
        weak: told.weak,
        optional: told.optional,
        provides: told.provides,
        platforms: told.platforms,
        unsandboxed: told.unsandboxed,
        setup: Vec::new(),
        launch: Vec::new(),
        described: Described::default(),
    };
    Ok((Arc::new(shared), about, registration))
}
