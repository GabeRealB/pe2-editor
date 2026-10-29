//! Extensions built as WebAssembly components, run by wasmtime: the host side of the contract in
//! `crates/ext/wit/extension.wit`.
//!
//! Loading compiles a component, in an engine of its own, instantiates it once and asks who it is (`describe`) and
//! what it registers (`register`); what it registers becomes the same kinds, roles, fields, pictures, thumbnails and
//! views a linked extension's would, each callback a call into the component by its handle.
//!
//! A guest's memory only grows, so an instance holds the most it ever held until it is dropped: instances live by the
//! lifetime of their job, never a timer. A job - a picture, a thumbnail, an import, an export, a tool's run - gets an
//! instance made for it and dropped when it returns. Readings - whether an asset has a picture, its looks, a field's
//! reading, facts, parts, a sniff, a check - share one instance, reused while it is used and reclaimed after
//! `READING_FRAMES` frames unused (the host counts frames: `frame`). Long-running jobs keep theirs: the views live in
//! one instance, which goes when its last view closes and it answers that it may be reclaimed; an extraction session in
//! one of its own, which goes with the session. Each engine pools its instances' slots, so making one is cheap and
//! dropping one returns its memory. An instance that traps is dropped, not reused.
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
use pe2_ext::{Asset, Declared, Discs, Extraction, Field, Image, Name, Picture, Project, Registry, Source, Table, UiHost};
use pe2_ext::{Value, View};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use wasmtime::component::{Accessor, Component, HasSelf, Linker, Resource, ResourceAny, ResourceTable};
use std::sync::Weak;
use std::sync::atomic::{AtomicUsize, Ordering};
use wasmtime::{Config, Engine, ResourceLimiter, Store};
use wasmtime_wasi::p2::pipe::MemoryOutputPipe;
use wasmtime_wasi::{HostMonotonicClock, HostWallClock, WasiCtx, WasiCtxView, WasiView};

use super::{Loadable, Origin};

mod bindings {
    wasmtime::component::bindgen!({
        world: "extension",
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
        },
    });
}

use bindings::exports::pe2::extension::guest as g;
use bindings::pe2::extension::{host, types};

/// The engine's epoch ticks this often: deadlines are counted in its ticks.
const TICK: Duration = Duration::from_millis(10);
/// A call that answers a question (has a picture, its looks, a field's reading).
const QUICK: u64 = 100;
/// A frame of a view (whose first frame may decode its picture).
const FRAME: u64 = 200;
/// A picture or a thumbnail, decoded.
const LONG: u64 = 1000;
/// An extraction session's beginning (reading every declaration it needs) or one asset's extraction.
const EXTRACTION: u64 = 6000;
/// An instance's memory, at most, unless its extension declares more for its job (`Registry::memory`).
const MEMORY: u64 = 256 << 20;
/// The most any job may be given: a 32-bit memory's whole range, which a pooled slot reserves anyway.
const MOST_MEMORY: u64 = 4 << 30;
/// A reading instance grown past this after a call is dropped, not kept: its memory goes back, as a guest's never
/// shrinks.
const RESIDENT: usize = 64 << 20;
/// How many frames a reading instance stays unused before it is reclaimed.
const READING_FRAMES: u32 = 60;
/// How many instances of an extension live at once, at most: its readings', its jobs', its views'.
const SLOTS: u32 = 16;
/// What a slot keeps of its memory when its instance goes, to be reset rather than faulted in again by the next.
const KEEP_RESIDENT: usize = 4 << 20;

/// An extension's engine, deterministic - NaNs canonical, relaxed SIMD deterministic, an epoch for deadlines - with a
/// pooling allocator of its own: its slots reserved up front (address space, not memory), at most `SLOTS` instances
/// of it at once, an instance's memory decommitted to the component's image when it goes and its slot kept warm, so
/// making one is cheap and dropping one gives its memory back.
fn new_engine() -> Result<Engine> {
    let mut pooling = wasmtime::PoolingAllocationConfig::default();
    pooling
        .total_component_instances(SLOTS)
        .total_core_instances(SLOTS * 8)
        .total_memories(SLOTS * 2)
        .total_tables(SLOTS * 8)
        .total_stacks(SLOTS)
        .max_memory_size(usize::try_from(MOST_MEMORY).unwrap_or(usize::MAX))
        .linear_memory_keep_resident(KEEP_RESIDENT)
        .max_unused_warm_slots(4);
    let mut config = Config::new();
    config.allocation_strategy(wasmtime::InstanceAllocationStrategy::Pooling(pooling));
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
    kind: Option<String>,
    role: Option<String>,
}

// SAFETY: a lent asset is read only during the call that lent it, on the thread making the call.
unsafe impl Send for LentAsset {}

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
struct State {
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
        let lent = LentAsset { source, kind: asset.kind().map(str::to_string), role: asset.role().map(str::to_string) };
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

impl types::Host for State {}

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

    fn kind(&mut self, asset: Resource<LentAsset>) -> wasmtime::Result<Option<String>> {
        Ok(self.table.get(&asset)?.kind.clone())
    }

    fn role(&mut self, asset: Resource<LentAsset>) -> wasmtime::Result<Option<String>> {
        Ok(self.table.get(&asset)?.role.clone())
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

    fn drop(&mut self, asset: Resource<LentAsset>) -> wasmtime::Result<()> {
        self.found.retain(|&rep| rep != asset.rep());
        self.table.delete(asset)?;
        Ok(())
    }
}

impl host::HostProject for State {
    fn asset(&mut self, project: Resource<LentProject>, path: String) -> wasmtime::Result<Option<Resource<LentAsset>>> {
        // SAFETY: lent for the call running now, taken back before it returns.
        let project = unsafe { &*self.project(&project)? };
        self.found(project.asset(&path))
    }

    fn resolve(&mut self, project: Resource<LentProject>, reference: String) -> wasmtime::Result<Option<Resource<LentAsset>>> {
        // SAFETY: as `asset`.
        let project = unsafe { &*self.project(&project)? };
        let Some(reference) = parse_value(&reference) else { return Ok(None) };
        self.found(project.resolve(&reference))
    }

    fn find(&mut self, project: Resource<LentProject>, kind: String) -> wasmtime::Result<Vec<Resource<LentAsset>>> {
        // SAFETY: as `asset`.
        let project = unsafe { &*self.project(&project)? };
        let mut out = vec![];
        for asset in project.find(&kind) {
            out.extend(self.found(Some(asset))?);
        }
        Ok(out)
    }

    fn selection(&mut self, project: Resource<LentProject>) -> wasmtime::Result<Option<Resource<LentAsset>>> {
        // SAFETY: as `asset`.
        let project = unsafe { &*self.project(&project)? };
        self.found(project.selection())
    }

    fn look(&mut self, project: Resource<LentProject>, path: String) -> wasmtime::Result<u32> {
        // SAFETY: as `asset`.
        let project = unsafe { &*self.project(&project)? };
        Ok(u32::try_from(project.look(&path)).unwrap_or(0))
    }

    fn picture(
        &mut self,
        project: Resource<LentProject>,
        path: String,
        look: u32,
    ) -> wasmtime::Result<Option<Result<types::Image, String>>> {
        let project = self.project(&project)?;
        Ok(elsewhere(project, move |p| p.picture(&path, look as usize).map(|made| made.map(to_wit).map_err(|e| format!("{e:#}")))))
    }

    fn part(
        &mut self,
        project: Resource<LentProject>,
        path: String,
        part: String,
    ) -> wasmtime::Result<Option<Result<types::Image, String>>> {
        let project = self.project(&project)?;
        Ok(elsewhere(project, move |p| p.part(&path, &part).map(|made| made.map(to_wit).map_err(|e| format!("{e:#}")))))
    }

    fn parts(&mut self, project: Resource<LentProject>, path: String) -> wasmtime::Result<Vec<(String, String, Vec<String>)>> {
        let project = self.project(&project)?;
        Ok(elsewhere(project, move |p| p.parts(&path).into_iter().map(|part| (part.id, part.label, part.made_of)).collect()))
    }

    fn facts(&mut self, project: Resource<LentProject>, path: String) -> wasmtime::Result<String> {
        let project = self.project(&project)?;
        Ok(elsewhere(project, move |p| toml::to_string(&p.facts(&path)).unwrap_or_default()))
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
    fn of_kind(&mut self, declarations: Resource<LentDeclarations>, kind: String) -> wasmtime::Result<Vec<(String, String)>> {
        // SAFETY: lent for the call running now (`ComponentExtraction::begin`), taken back before it returns.
        let declared = unsafe { &*self.table.get(&declarations)?.declared };
        Ok(declared
            .declarations(&kind)
            .into_iter()
            .map(|(path, table)| (path, toml::to_string(&table).unwrap_or_default()))
            .collect())
    }

    fn drop(&mut self, declarations: Resource<LentDeclarations>) -> wasmtime::Result<()> {
        // A borrow the guest drops: the host takes the declarations back itself.
        let _ = declarations;
        Ok(())
    }
}

impl host::HostDiscs for State {
    fn read(&mut self, discs: Resource<LentDiscs>, at: String) -> wasmtime::Result<Result<Vec<u8>, String>> {
        // SAFETY: lent for the call running now (`ComponentExtraction::extract`), taken back before it returns.
        let discs = unsafe { &mut *self.table.get(&discs)?.discs };
        let at: Table = match toml::from_str(&at) {
            Ok(at) => at,
            Err(e) => return Ok(Err(format!("not a location: {e}"))),
        };
        Ok(discs.read(&at).map_err(|e| format!("{e:#}")))
    }

    fn size(&mut self, discs: Resource<LentDiscs>, at: String) -> wasmtime::Result<Result<u64, String>> {
        // SAFETY: as `read`.
        let discs = unsafe { &mut *self.table.get(&discs)?.discs };
        let at: Table = match toml::from_str(&at) {
            Ok(at) => at,
            Err(e) => return Ok(Err(format!("not a location: {e}"))),
        };
        Ok(discs.size(&at).map_err(|e| format!("{e:#}")))
    }

    fn read_range(
        &mut self,
        discs: Resource<LentDiscs>,
        at: String,
        offset: u64,
        len: u32,
    ) -> wasmtime::Result<Result<Vec<u8>, String>> {
        // SAFETY: as `read`.
        let discs = unsafe { &mut *self.table.get(&discs)?.discs };
        let at: Table = match toml::from_str(&at) {
            Ok(at) => at,
            Err(e) => return Ok(Err(format!("not a location: {e}"))),
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

    fn table(&mut self, ui: Resource<Frame>, id: String, rows: Vec<Vec<host::Cell>>) -> wasmtime::Result<Option<(u32, u32)>> {
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

    fn chip(&mut self, ui: Resource<Frame>, text: String) -> wasmtime::Result<bool> {
        Ok(self.frame(&ui)?.chip(&text))
    }

    fn image(&mut self, ui: Resource<Frame>, texture: Resource<HostTexture>, width: f32, height: f32) -> wasmtime::Result<()> {
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

    fn text_edit(&mut self, ui: Resource<Frame>, id: String, text: String) -> wasmtime::Result<(String, bool)> {
        let mut text = text;
        let committed = self.frame(&ui)?.text_edit(&id, &mut text);
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

    fn editable(&mut self, ui: Resource<Frame>) -> wasmtime::Result<bool> {
        Ok(self.frame(&ui)?.editable())
    }

    fn set(&mut self, ui: Resource<Frame>, key: String, value: Option<String>) -> wasmtime::Result<()> {
        // A value that does not parse is an error, never a removal.
        let value = match value {
            Some(text) => Some(parse_value(&text).ok_or_else(|| wasmtime::Error::msg(format!("{key}: not TOML")))?),
            None => None,
        };
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
        };
        self.frame(&ui)?.canvas(&canvas);
        Ok(())
    }

    fn picture(
        &mut self,
        ui: Resource<Frame>,
        path: String,
        look: u32,
    ) -> wasmtime::Result<Option<Result<(Resource<HostTexture>, u32, u32), String>>> {
        let Some(shown) = self.frame(&ui)?.picture(&path, look as usize) else { return Ok(None) };
        Ok(Some(match shown {
            Ok(texture) => {
                let (width, height) = (u32::try_from(texture.width())?, u32::try_from(texture.height())?);
                let held = self.table.push(HostTexture { key: texture.key().to_string(), image: texture.image().clone() })?;
                Ok((held, width, height))
            }
            Err(e) => Err(e),
        }))
    }

    fn navigate(&mut self, ui: Resource<Frame>, path: String) -> wasmtime::Result<()> {
        self.frame(&ui)?.navigate(&path);
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
    instance: bindings::Extension,
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
        let lent = LentAsset { source, kind: asset.kind().map(str::to_string), role: asset.role().map(str::to_string) };
        Ok(self.store.data_mut().table.push(lent)?)
    }

    fn unlend(&mut self, asset: Resource<LentAsset>) {
        let _ = self.store.data_mut().table.delete(asset);
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
    pre: bindings::ExtensionPre<State>,
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
}

impl Shared {
    /// What a job of something its extension registered may take, by its name within the extension.
    fn cap(&self, local: &str) -> u64 {
        self.caps.get(&format!("{}@{local}", self.id)).copied().unwrap_or(MEMORY)
    }

    /// A new instance for a job that may take `cap` bytes; its extension registers when a call first needs it. Where
    /// its engine's slots are all taken, room is made - the readings' instance reclaimed, a long-running one asked - and
    /// it is tried once more.
    fn instantiate(&self, cap: u64) -> Result<Runner> {
        match self.instance(cap) {
            Ok(runner) => Ok(runner),
            Err(_) if self.make_room() => self.instance(cap),
            Err(e) => Err(e),
        }
    }

    /// Room for an instance: the readings' instance reclaimed whatever its frames, and the views' asked whether it may
    /// go where no view lives in it. Whether any went.
    fn make_room(&self) -> bool {
        let mut made = false;
        if let Ok(mut reading) = self.reading.try_lock() {
            made |= reading.take().is_some();
        }
        if self.open_views.load(Ordering::Relaxed) == 0
            && let Ok(mut views) = self.views.try_lock()
            && let Some(runner) = views.as_ref()
        {
            let reclaimable = runner.try_lock().ok().is_none_or(|mut r| {
                r.call(QUICK, async |a, g| g.call_reclaimable(a).await).unwrap_or(true)
            });
            if reclaimable {
                *views = None;
                made = true;
            }
        }
        made
    }

    fn instance(&self, cap: u64) -> Result<Runner> {
        let stderr = MemoryOutputPipe::new(64 << 10);
        let wasi = WasiCtx::builder().stderr(stderr.clone()).wall_clock(Still).monotonic_clock(Still).build();
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
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(LONG);
        let runtime = Runtime(std::mem::ManuallyDrop::new(tokio::runtime::Builder::new_current_thread().enable_all().build()?));
        let instance = drive(&runtime, self.pre.instantiate_async(&mut store))?;
        Ok(Runner { store, instance, runtime, meter, stderr })
    }

    /// A job that may take `cap` bytes: run on an instance made for it, dropped when it returns.
    fn job<R>(&self, cap: u64, call: impl FnOnce(&mut Runner) -> Result<R>) -> Result<R> {
        let mut runner = self.instantiate(cap).with_context(|| self.id.clone())?;
        let out = call(&mut runner).with_context(|| self.id.clone());
        self.peak.fetch_max(runner.bytes(), Ordering::Relaxed);
        out
    }

    /// A reading: run on the readings' instance, made if there is none, its frames reset; a reading while it is busy
    /// on another thread is a job. One that failed, or grew past the budget, goes.
    fn read<R>(&self, call: impl FnOnce(&mut Runner) -> Result<R>) -> Result<R> {
        let Ok(mut reading) = self.reading.try_lock() else { return self.job(MEMORY, call) };
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
            runner.lock().ok().is_none_or(|mut r| {
                r.call(QUICK, async |a, g| g.call_reclaimable(a).await).unwrap_or(true)
            })
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
        self.read(|runner| runner.call(QUICK, async |accessor, guest| guest.call_format(accessor, field, brief, text).await))
            .ok()
            .flatten()
    }
}

/// A kind's picture, drawn by its component.
struct ComponentPicture {
    shared: Arc<Shared>,
    kind: u32,
    /// What drawing one may take.
    cap: u64,
}

fn image(image: types::Image) -> Image {
    Image { width: image.width as usize, height: image.height as usize, rgba: image.rgba }
}

impl Picture for ComponentPicture {
    fn has(&self, asset: &Asset) -> bool {
        let kind = self.kind;
        self.shared
            .read(|r| r.with_asset(asset, QUICK, async |a, g, lent| g.call_picture_has(a, kind, lent).await))
            .unwrap_or(false)
    }

    fn looks(&self, asset: &Asset) -> Vec<String> {
        let kind = self.kind;
        self.shared
            .read(|r| r.with_asset(asset, QUICK, async |a, g, lent| g.call_picture_looks(a, kind, lent).await))
            .unwrap_or_default()
    }

    fn draw(&self, asset: &Asset, look: usize) -> Result<Image> {
        let (kind, look) = (self.kind, u32::try_from(look)?);
        self.shared
            .job(self.cap, |r| r.with_asset(asset, LONG, async |a, g, lent| g.call_picture_draw(a, kind, lent, look).await))?
            .map(image)
            .map_err(|e| anyhow!(e))
    }

    fn part(&self, asset: &Asset, part: &str) -> Option<Result<Image>> {
        let (kind, part) = (self.kind, part.to_string());
        let made = self
            .shared
            .job(self.cap, |r| r.with_asset(asset, LONG, async |a, g, lent| g.call_picture_part(a, kind, lent, part).await));
        match made {
            Ok(made) => made.map(|m| m.map(image).map_err(|e| anyhow!(e))),
            Err(e) => Some(Err(e)),
        }
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
        let look = u32::try_from(ui.look()).unwrap_or(0);
        let lent = ui.asset().map(|asset| {
            // SAFETY: lent for this frame's call alone, and taken back before it returns.
            let source = unsafe { erase_source(asset.source()) };
            LentAsset { source, kind: asset.kind().map(str::to_string), role: asset.role().map(str::to_string) }
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
                g.view_state().call_show(a, state, ui, project, asset, look).await
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
        let kind = Name::parse(&extractor.kind).with_context(|| format!("{}: not a qualified kind", extractor.kind))?;
        let roles = extractor
            .roles
            .iter()
            .map(|r| Name::parse(r).with_context(|| format!("{r}: not a qualified role")))
            .collect::<Result<Vec<_>>>()?;
        let (shared, handle) = (shared.clone(), extractor.handle);
        let cap = shared.cap(&extractor.name);
        let _ = registry.extractor(&extractor.name, &kind, &roles, move |declared| {
            Ok(Box::new(ComponentExtraction::begin(&shared, handle, cap, declared)?) as Box<dyn Extraction>)
        });
    }
    Ok(())
}

/// A tool's or a service's work, as a job of its component with the project lent: its arguments to its answer.
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
                r.call(LONG, async |a, g| g.call_serve(a, handle, borrowed, arguments).await)
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

/// What the views' instance may take: the most any kind, role or panel with views was given, as they share it.
fn views_cap(id: &str, registration: &g::Registration, caps: &std::collections::HashMap<String, u64>) -> u64 {
    let with_views = registration
        .kinds
        .iter()
        .filter(|k| !k.views.is_empty() || !k.inspector.is_empty())
        .map(|k| k.name.as_str())
        .chain(registration.roles.iter().filter(|r| !r.views.is_empty() || !r.inspector.is_empty()).map(|r| r.name.as_str()))
        .chain(registration.panels.iter().map(|p| p.name.as_str()));
    with_views.filter_map(|local| caps.get(&format!("{id}@{local}")).copied()).fold(MEMORY, u64::max)
}

/// A field as its component describes it, its readings called back.
fn field(shared: &Arc<Shared>, f: g::Field) -> Field {
    let mut out = Field::new(&f.key);
    out.label = f.label;
    out.read_only = f.read_only;
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
    for (index, kind) in registration.kinds.into_iter().enumerate() {
        let index = u32::try_from(index)?;
        let mut builder = registry.kind(&kind.name).label(&kind.label);
        if let Some(alias) = &kind.alias {
            builder = builder.alias(alias);
        }
        if let Some((i, t)) = kind.icon {
            builder = builder.icon(icon(i), tint(t));
        }
        for f in kind.fields {
            builder = builder.field(field(shared, f));
        }
        let cap = shared.cap(&kind.name);
        if kind.picture {
            builder = builder.picture(ComponentPicture { shared: shared.clone(), kind: index, cap });
        }
        if kind.thumbnail {
            let shared = shared.clone();
            builder = builder.thumbnail(move |asset, side| {
                let side = u32::try_from(side).ok()?;
                let made = shared.job(cap, |r| {
                    r.with_asset(asset, LONG, async |a, g, lent| g.call_thumbnail(a, index, lent, side).await)
                });
                match made {
                    Ok(made) => made.map(|m| m.map(image).map_err(|e| anyhow!(e))),
                    Err(e) => Some(Err(e)),
                }
            });
        }
        if kind.facts {
            let shared = shared.clone();
            builder = builder.facts(move |asset| {
                let text = shared
                    .read(|r| r.with_asset(asset, QUICK, async |a, g, lent| g.call_facts(a, index, lent).await))
                    .unwrap_or_default();
                toml::from_str(&text).unwrap_or_default()
            });
        }
        if kind.parts {
            let shared = shared.clone();
            builder = builder.parts(move |asset| {
                let parts = shared
                    .read(|r| r.with_asset(asset, QUICK, async |a, g, lent| g.call_parts(a, index, lent).await))
                    .unwrap_or_default();
                parts.into_iter().map(|p| pe2_ext::Part { id: p.id, label: p.label, made_of: p.made_of }).collect()
            });
        }
        for v in kind.views {
            builder = builder.view(&v.name, view(shared, v.handle));
        }
        for v in kind.inspector {
            builder = builder.inspector(&v.name, view(shared, v.handle));
        }
        let _ = builder.register();
    }
    for role in registration.roles {
        let kind = Name::parse(&role.kind).with_context(|| format!("{}: not a qualified kind", role.kind))?;
        let mut builder = registry.role(&kind, &role.name).label(&role.label);
        if let Some(alias) = &role.alias {
            builder = builder.alias(alias);
        }
        for f in role.fields {
            builder = builder.field(field(shared, f));
        }
        for v in role.views {
            builder = builder.view(&v.name, view(shared, v.handle));
        }
        for v in role.inspector {
            builder = builder.inspector(&v.name, view(shared, v.handle));
        }
        let _ = builder.register();
    }
    for panel in registration.panels {
        let (shared, handle) = (shared.clone(), panel.handle);
        let icon = panel.icon.map_or(pe2_ext::Icon::Grid, icon);
        registry.panel(&panel.name, &panel.label, icon, move || Box::new(ComponentView::new(shared.clone(), handle, None)));
    }
    apply_formats(registry, registration.formats, shared);
    apply_importers(registry, registration.importers, shared)?;
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

/// Register what a component's formats are, their sniffs calls into it.
fn apply_formats(registry: &mut Registry, formats: Vec<g::FileFormat>, shared: &Arc<Shared>) {
    for format in formats {
        let mut builder = registry.format(&format.name).label(&format.label);
        if let Some(media_type) = &format.media_type {
            builder = builder.media_type(media_type);
        }
        for signature in &format.signatures {
            builder = match signature {
                g::Signature::Bytes((offset, bytes)) => builder.bytes(*offset as usize, bytes),
                g::Signature::Name(name) => builder.named(name),
                g::Signature::Text => builder.text(),
            };
        }
        if format.sniff {
            let (shared, handle) = (shared.clone(), format.handle);
            builder = builder.sniff(move |name, size, head| {
                let (name, head) = (name.to_string(), head.to_vec());
                let version = shared
                    .read(|r| r.call(QUICK, async |a, g| g.call_sniff(a, handle, name, size, head).await))
                    .ok()
                    .flatten()?;
                Some(pe2_ext::Recognized { version: (!version.is_empty()).then_some(version) })
            });
        }
        let _ = builder.register();
    }
}

/// Register a component's importers, their checks calls into it.
fn apply_importers(registry: &mut Registry, importers: Vec<g::Importer>, shared: &Arc<Shared>) -> Result<()> {
    for importer in importers {
        let makes = Name::parse(&importer.makes).with_context(|| format!("{}: not a qualified kind", importer.makes))?;
        let mut builder = registry.importer(&importer.name, &makes).label(&importer.label).priority(importer.priority);
        for read in &importer.reads {
            builder = match Name::parse(read) {
                Some(format) => builder.reads(&format),
                None => builder.reads_media(read),
            };
        }
        if importer.native {
            builder = builder.native();
        }
        for f in importer.fields {
            builder = builder.field(field(shared, f));
        }
        for v in importer.inspector {
            builder = builder.inspector(&v.name, view(shared, v.handle));
        }
        if importer.check {
            let (shared, handle) = (shared.clone(), importer.handle);
            builder = builder.check(move |asset| {
                let support =
                    shared.read(|r| r.with_asset(asset, QUICK, async |a, g, lent| g.call_check(a, handle, lent).await));
                match support {
                    Ok(g::Support::Full) => pe2_ext::Support::Full,
                    Ok(g::Support::Partial((reason, offer))) => pe2_ext::Support::Partial { reason, offer },
                    Err(e) => pe2_ext::Support::Partial { reason: format!("{e:#}"), offer: false },
                }
            });
        }
        let cap = shared.cap(&importer.name);
        if importer.imports {
            let (shared, handle) = (shared.clone(), importer.handle);
            builder = builder.import(move |asset| {
                let made = shared
                    .job(cap, |r| r.with_asset(asset, LONG, async |a, g, lent| g.call_make_native(a, handle, lent).await))?;
                let made = made.map_err(|e| pe2_ext::anyhow!("{e}"))?;
                let settings = toml::from_str(&made.settings).context("the settings an import derived")?;
                Ok(pe2_ext::Imported { files: made.files, settings, notes: made.notes })
            });
        }
        for path in &importer.derives {
            builder = builder.derives(path);
        }
        if importer.exports {
            let (shared, handle) = (shared.clone(), importer.handle);
            builder = builder.export(move |asset| {
                let made = shared
                    .job(cap, |r| r.with_asset(asset, LONG, async |a, g, lent| g.call_make_source(a, handle, lent).await))?;
                made.map_err(|e| pe2_ext::anyhow!("{e}"))
            });
        }
        let _ = builder.register();
    }
    Ok(())
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
        let old = entry.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age > KEPT);
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
        if let Ok(component) = unsafe { Component::deserialize(engine, code) } {
            return Ok(component);
        }
    }
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
/// (`pe2-extension`, in its core module): its id, version and requirements.
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize)]
pub struct About {
    pub id: String,
    pub version: String,
    #[serde(default)]
    pub requires: Vec<String>,
}

/// What a component says it is, read from its section; none where it has none (built by an older SDK).
#[must_use]
pub fn about(bytes: &[u8]) -> Option<About> {
    wasmparser::Parser::new(0).parse_all(bytes).find_map(|payload| match payload.ok()? {
        wasmparser::Payload::CustomSection(section) if section.name() == SECTION => {
            toml::from_str(std::str::from_utf8(section.data()).ok()?).ok()
        }
        _ => None,
    })
}

/// The section `extension!` writes.
const SECTION: &str = "pe2-extension";

/// The contract this host runs: the version of the WIT package (`pe2:extension@0.2.0`), the line it is of (`0.2`)
/// running every component built against a version of the line no newer than it.
pub const CONTRACT: &str = "0.3.3";

/// What a component's guest interface is exported as, before its version.
const GUEST: &str = "pe2:extension/guest@";

/// The contract a component was built against: the version it exports its guest interface at; none where it exports
/// none - no extension.
#[must_use]
pub fn contract(bytes: &[u8]) -> Option<String> {
    wasmparser::Parser::new(0).parse_all(bytes).find_map(|payload| match payload.ok()? {
        wasmparser::Payload::ComponentExportSection(exports) => exports
            .into_iter()
            .find_map(|e| Some(e.ok()?.name.name.strip_prefix(GUEST)?.to_string())),
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
        format!("it is built for contract {their_line}, and this editor runs {our_line}: rebuild it with this editor's SDK")
    })
}

/// A component to load (`name` names it until it says who it is). One that says who it is in its section is compiled
/// and run only when it registers - an extension disabled, or whose requirements are not met, never is - and must then
/// say the same when asked; one without is compiled, instantiated and asked now.
///
/// # Errors
/// One without the section cannot be compiled or instantiated, or its registration fails.
pub fn load_bytes(name: &str, bytes: &[u8], origin: Origin) -> Result<Loadable> {
    let size = bytes.len();
    // Its contract first, before anything of it is compiled.
    let contract = contract(bytes).context("it exports no pe2:extension/guest: it is no extension")?;
    runs(&contract).map_err(|why| anyhow!(why))?;
    let contract = Some(contract);
    let cell: Arc<OnceLock<Arc<Shared>>> = Arc::default();
    if let Some(about) = about(bytes) {
        let (name, bytes, said, kept) = (name.to_string(), bytes.to_vec(), about.clone(), cell.clone());
        return Ok(Loadable {
            id: about.id,
            version: about.version,
            requires: about.requires,
            origin,
            register: Some(Box::new(move |registry| {
                let settings = Value::Table(registry.stored().clone()).to_string();
                let (shared, told, registration) = start(&name, &bytes, &settings)?;
                anyhow::ensure!(told == said, "{name}: it says it is {told:?}, its section {said:?}");
                let shared = kept.get_or_init(|| shared).clone();
                apply(registry, registration, &shared)
            })),
            component: Some(cell),
            size: Some(size),
            contract: contract.clone(),
            settings: toml::Table::new(),
        });
    }
    // Without its section it is asked who it is now, before the host knows its settings: it registers with none kept.
    let (shared, about, registration) = start(name, bytes, "{}")?;
    let shared = cell.get_or_init(|| shared).clone();
    Ok(Loadable {
        id: about.id,
        version: about.version,
        requires: about.requires,
        origin,
        register: Some(Box::new(move |registry| apply(registry, registration, &shared))),
        component: Some(cell),
        size: Some(size),
        contract,
        settings: toml::Table::new(),
    })
}

/// A component compiled in an engine of its own, its first instance asked who it is and what it registers - which
/// then answers the first readings.
fn start(name: &str, bytes: &[u8], settings: &str) -> Result<(Arc<Shared>, About, g::Registration)> {
    let engine = new_engine()?;
    let component = compile(&engine, bytes).with_context(|| name.to_string())?;
    let mut linker = Linker::<State>::new(&engine);
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;
    bindings::Extension::add_to_linker::<State, HasSelf<State>>(&mut linker, |state| state)?;
    let pre = bindings::ExtensionPre::new(linker.instantiate_pre(&component)?)?;
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
    *shared.reading.get_mut().map_err(|_| anyhow!("{name}: its readings are poisoned"))? = Some((runner, READING_FRAMES));
    let about = About { id: told.id, version: told.version, requires: told.requires };
    Ok((Arc::new(shared), about, registration))
}
