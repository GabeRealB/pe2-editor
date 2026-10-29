//! Extensions built as WebAssembly components, run by wasmtime: the host side of the contract in
//! `crates/ext/wit/extension.wit`.
//!
//! Loading compiles a component, instantiates it once and asks who it is (`describe`) and what it registers
//! (`register`); what it registers becomes the same kinds, roles, fields, pictures, thumbnails and views a linked
//! extension's would, each callback a call into the component by its handle. A call runs on an instance taken from the
//! extension's pool (another is instantiated when all are busy, so the thumbnails' thread and the frames do not wait on
//! each other); the views share one instance of their own, as a view lives in the instance that made it. An instance
//! that traps is dropped, not reused.
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
use pe2_ext::{Asset, Field, Image, Name, Picture, Project, Registry, Source, Table, UiHost, Value, View};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use wasmtime::component::{Accessor, Component, HasSelf, Linker, Resource, ResourceAny, ResourceTable};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};
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
/// An instance's memory, at most.
const MEMORY: usize = 512 << 20;

/// The engine every component runs in, deterministic: NaNs canonical, relaxed SIMD deterministic, and an epoch for
/// deadlines.
fn engine() -> &'static Engine {
    static ENGINE: OnceLock<Engine> = OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut config = Config::new();
        config.wasm_component_model_async(true);
        config.epoch_interruption(true);
        config.cranelift_nan_canonicalization(true);
        config.relaxed_simd_deterministic(true);
        let engine = Engine::new(&config).expect("the engine's configuration is valid");
        let ticking = engine.weak();
        std::thread::spawn(move || {
            while let Some(engine) = ticking.upgrade() {
                engine.increment_epoch();
                drop(engine);
                std::thread::sleep(TICK);
            }
        });
        engine
    })
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

/// An instance's state: its resources, WASI with nothing behind it, its limits.
struct State {
    id: String,
    wasi: WasiCtx,
    table: ResourceTable,
    limits: StoreLimits,
    /// The frames open, the lent one first and its innermost row last: only the last is drawn on.
    frames: Vec<u32>,
    /// The assets the project's queries lent during the call, taken back when it returns.
    found: Vec<u32>,
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

    fn drop(&mut self, project: Resource<LentProject>) -> wasmtime::Result<()> {
        self.table.delete(project)?;
        Ok(())
    }
}

/// A value as it crosses: TOML.
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

impl host::HostUi for State {
    fn label(&mut self, ui: Resource<Frame>, text: String, style: types::Style) -> wasmtime::Result<()> {
        let style = match style {
            types::Style::Normal => pe2_ext::Style::Normal,
            types::Style::Weak => pe2_ext::Style::Weak,
            types::Style::Note => pe2_ext::Style::Note,
            types::Style::Strong => pe2_ext::Style::Strong,
            types::Style::Error => pe2_ext::Style::Error,
        };
        self.frame(&ui)?.label(&text, style);
        Ok(())
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
    runtime: tokio::runtime::Runtime,
}

impl Runner {
    /// Run `call` on the instance, within `ticks` of the epoch.
    fn call<R>(
        &mut self,
        ticks: u64,
        call: impl AsyncFnOnce(&Accessor<State>, &g::Guest) -> wasmtime::Result<R>,
    ) -> Result<R> {
        self.store.set_epoch_deadline(ticks);
        let Runner { store, instance, runtime } = self;
        let guest = instance.pe2_extension_guest();
        let out = runtime.block_on(store.run_concurrent(async |accessor| call(accessor, guest).await));
        // What the project's queries lent goes with the call.
        let data = self.store.data_mut();
        for rep in std::mem::take(&mut data.found) {
            let _ = data.table.delete(Resource::<LentAsset>::new_own(rep));
        }
        Ok(out??)
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

    /// What standard error the instance wrote: a panic's message.
    fn stderr(&self, pipe: &MemoryOutputPipe) -> String {
        let _ = self;
        String::from_utf8_lossy(&pipe.contents()).trim().to_string()
    }
}

/// A component: compiled once, instantiated as often as calls need.
pub struct Shared {
    id: String,
    pre: bindings::ExtensionPre<State>,
    pool: Mutex<Vec<Runner>>,
    /// The instance the views live in, made with the first view.
    views: Mutex<Option<Arc<Mutex<Runner>>>>,
}

impl Shared {
    /// A new instance, its registration run (each instance keeps what its extension registered).
    fn instantiate(&self, register: bool) -> Result<Runner> {
        let stderr = MemoryOutputPipe::new(64 << 10);
        let wasi = WasiCtx::builder().stderr(stderr.clone()).wall_clock(Still).monotonic_clock(Still).build();
        let limits = StoreLimitsBuilder::new().memory_size(MEMORY).build();
        let table = ResourceTable::new();
        let state = State { id: self.id.clone(), wasi, table, limits, frames: vec![], found: vec![] };
        let mut store = Store::new(engine(), state);
        store.limiter(|state| &mut state.limits);
        store.set_epoch_deadline(LONG);
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        let instance = runtime.block_on(self.pre.instantiate_async(&mut store))?;
        let mut runner = Runner { store, instance, runtime };
        if register {
            runner
                .call(LONG, async |accessor, guest| guest.call_register(accessor).await)
                .map_err(|e| anyhow!("{e:#}: {}", runner.stderr(&stderr)))?
                .map_err(|e| anyhow!(e))?;
        }
        Ok(runner)
    }

    /// Run `call` on an idle instance of the pool, or a new one; an instance that fails is dropped.
    fn with<R>(&self, call: impl FnOnce(&mut Runner) -> Result<R>) -> Result<R> {
        let idle = self.pool.lock().map_err(|_| anyhow!("{}: its pool is poisoned", self.id))?.pop();
        let mut runner = match idle {
            Some(runner) => runner,
            None => self.instantiate(true)?,
        };
        let out = call(&mut runner).with_context(|| self.id.clone());
        if out.is_ok()
            && let Ok(mut pool) = self.pool.lock()
        {
            pool.push(runner);
        }
        out
    }

    /// The views' instance.
    fn views(&self) -> Result<Arc<Mutex<Runner>>> {
        let mut views = self.views.lock().map_err(|_| anyhow!("{}: its views are poisoned", self.id))?;
        if let Some(runner) = views.as_ref() {
            return Ok(runner.clone());
        }
        let runner = Arc::new(Mutex::new(self.instantiate(true)?));
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
        self.with(|runner| runner.call(QUICK, async |accessor, guest| guest.call_format(accessor, field, brief, text).await))
            .ok()
            .flatten()
    }
}

/// A kind's picture, drawn by its component.
struct ComponentPicture {
    shared: Arc<Shared>,
    kind: u32,
}

fn image(image: types::Image) -> Image {
    Image { width: image.width as usize, height: image.height as usize, rgba: image.rgba }
}

impl Picture for ComponentPicture {
    fn has(&self, asset: &Asset) -> bool {
        let kind = self.kind;
        self.shared
            .with(|r| r.with_asset(asset, QUICK, async |a, g, lent| g.call_picture_has(a, kind, lent).await))
            .unwrap_or(false)
    }

    fn looks(&self, asset: &Asset) -> Vec<String> {
        let kind = self.kind;
        self.shared
            .with(|r| r.with_asset(asset, QUICK, async |a, g, lent| g.call_picture_looks(a, kind, lent).await))
            .unwrap_or_default()
    }

    fn draw(&self, asset: &Asset, look: usize) -> Result<Image> {
        let (kind, look) = (self.kind, u32::try_from(look)?);
        self.shared
            .with(|r| r.with_asset(asset, LONG, async |a, g, lent| g.call_picture_draw(a, kind, lent, look).await))?
            .map(image)
            .map_err(|e| anyhow!(e))
    }

    fn part(&self, asset: &Asset, part: &str) -> Option<Result<Image>> {
        let (kind, part) = (self.kind, part.to_string());
        let made = self
            .shared
            .with(|r| r.with_asset(asset, LONG, async |a, g, lent| g.call_picture_part(a, kind, lent, part).await));
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
        if let (Some(runner), Some(state)) = (&self.runner, self.state.take())
            && let Ok(mut r) = runner.lock()
        {
            let Runner { store, runtime, .. } = &mut *r;
            let _ = runtime.block_on(state.resource_drop_async(&mut *store));
        }
    }
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

fn apply(registry: &mut Registry, registration: g::Registration, shared: &Arc<Shared>) -> Result<()> {
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
        if kind.picture {
            builder = builder.picture(ComponentPicture { shared: shared.clone(), kind: index });
        }
        if kind.thumbnail {
            let shared = shared.clone();
            builder = builder.thumbnail(move |asset, side| {
                let side = u32::try_from(side).ok()?;
                let made = shared.with(|r| {
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
                    .with(|r| r.with_asset(asset, QUICK, async |a, g, lent| g.call_facts(a, index, lent).await))
                    .unwrap_or_default();
                toml::from_str(&text).unwrap_or_default()
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
    for tool in registration.tools {
        let (shared, handle, name) = (shared.clone(), tool.handle, tool.name.clone());
        registry.tool(&tool.name, &tool.description, &tool.schema, move |project, arguments| {
            let arguments = arguments.to_string();
            let answer = shared.with(|r| {
                let lent = r.lend_project(project)?;
                let borrowed = Resource::new_borrow(lent.rep());
                let out = r.call(LONG, async |a, g| g.call_run(a, handle, borrowed, arguments).await);
                r.unlend_project(lent);
                out
            })?;
            let answer = answer.map_err(|e| anyhow!(e))?;
            parse_value(&answer).with_context(|| format!("{name}: its answer is not TOML"))
        });
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
                    .with(|r| r.call(QUICK, async |a, g| g.call_sniff(a, handle, name, size, head).await))
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
                    shared.with(|r| r.with_asset(asset, QUICK, async |a, g, lent| g.call_check(a, handle, lent).await));
                match support {
                    Ok(g::Support::Full) => pe2_ext::Support::Full,
                    Ok(g::Support::Partial((reason, offer))) => pe2_ext::Support::Partial { reason, offer },
                    Err(e) => pe2_ext::Support::Partial { reason: format!("{e:#}"), offer: false },
                }
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
fn compile(bytes: &[u8]) -> Result<Component> {
    use sha1::{Digest, Sha1};
    use std::hash::{Hash, Hasher};
    let mut settings = std::hash::DefaultHasher::new();
    engine().precompile_compatibility_hash().hash(&mut settings);
    let key = format!("{}-{:016x}", crate::extract::sha1_hex(bytes), settings.finish());
    let file = compiled_dir().map(|dir| dir.join(format!("{key}.cwasm")));
    if let Some(kept) = file.as_ref().and_then(|f| std::fs::read(f).ok())
        && let Some((digest, code)) = kept.split_at_checked(20)
        && Sha1::digest(code).as_slice() == digest
    {
        // SAFETY: what `serialize` wrote for this engine's settings (the key) and these bytes, unchanged since (the
        // digest).
        if let Ok(component) = unsafe { Component::deserialize(engine(), code) } {
            return Ok(component);
        }
    }
    let component = Component::new(engine(), bytes).map_err(anyhow::Error::from)?;
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

/// A component to load (`name` names it until it says who it is): compiled, instantiated, asked who it is and what it
/// registers.
///
/// # Errors
/// It cannot be compiled or instantiated, or its registration fails.
pub fn load_bytes(name: &str, bytes: &[u8], origin: Origin) -> Result<Loadable> {
    let component = compile(bytes).with_context(|| name.to_string())?;
    let mut linker = Linker::<State>::new(engine());
    wasmtime_wasi::p3::add_to_linker(&mut linker)?;
    bindings::Extension::add_to_linker::<State, HasSelf<State>>(&mut linker, |state| state)?;
    let pre = bindings::ExtensionPre::new(linker.instantiate_pre(&component)?)?;
    let mut shared = Shared { id: name.to_string(), pre, pool: Mutex::new(vec![]), views: Mutex::new(None) };
    let mut runner = shared.instantiate(false)?;
    let about = runner.call(LONG, async |accessor, guest| guest.call_describe(accessor).await)?;
    let registration = runner
        .call(LONG, async |accessor, guest| guest.call_register(accessor).await)?
        .map_err(|e| anyhow!("{name}: its registration failed: {e}"))?;
    shared.id.clone_from(&about.id);
    runner.store.data_mut().id.clone_from(&about.id);
    shared.pool.get_mut().map_err(|_| anyhow!("{name}: its pool is poisoned"))?.push(runner);
    let shared = Arc::new(shared);
    Ok(Loadable {
        id: about.id,
        version: about.version,
        requires: about.requires,
        origin,
        register: Some(Box::new(move |registry| apply(registry, registration, &shared))),
    })
}
