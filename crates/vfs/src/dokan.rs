//! The tree served on Windows through Dokany, where it is installed: a real filesystem mounted at a folder, each request
//! answered from the tree served at that moment and, where it can be written, what programs wrote over it
//! (`writes::Writes`) - as FUSE serves it on Linux and macOS, so a project is written through its folder the same way
//! on each. Nothing is kept on disk: no placeholders, nothing to bring in step, and of a process that ended only the
//! folder, a junction to a volume that is gone, which the next to serve or sweep there makes a plain folder again
//! (`unjoin`).
//!
//! Dokany is a kernel driver and its library, which the user installs; its library is loaded when first used, so that
//! a Windows without it runs all the same, and serves by another backend.
//!
//! # Attribution
//!
//! The declarations here - the layouts of `Options`, `FileInfo` and `Operations`, the option flags, the status codes
//! of `DokanCreateFileSystem` and the signatures of the library's functions - are transcribed from Dokany's public
//! header, `dokan/dokan.h` of Dokan Library 2.3.1 (<https://github.com/dokan-dev/dokany>):
//!
//! > Dokan : user-mode file system library for Windows
//! > Copyright (C) 2020 - 2025 Google, Inc.
//! > Copyright (C) 2015 - 2019 Adrien J. <liryna.stark@gmail.com> and Maxime C. <maxime@islog.com>
//! > Copyright (C) 2007 - 2011 Hiroki Asakawa <info@dokan-dev.net>
//!
//! which is distributed under the GNU Lesser General Public License, version 3 or later. Nothing of Dokany is built
//! into or distributed with this program: the library called is the one installed on the user's system
//! (`dokan2.dll`), found and loaded at run time.

use std::collections::{BTreeMap, HashMap};
use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use windows::Win32::Foundation::FILETIME;
use windows::Win32::Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetDiskFreeSpaceExW, WIN32_FIND_DATAW};
use windows_core::PCWSTR;

use crate::backend::{Backend, Can, Job, Mount};
use crate::writes::{Shown, Writes};
use crate::{Served, Tree, What};

/// An `NTSTATUS`.
type Status = i32;

const fn status(code: u32) -> Status {
    code.cast_signed()
}

const SUCCESS: Status = 0;
const UNSUCCESSFUL: Status = status(0xC000_0001);
const ACCESS_DENIED: Status = status(0xC000_0022);
const NOT_FOUND: Status = status(0xC000_0034);
const COLLISION: Status = status(0xC000_0035);
const WRITE_PROTECTED: Status = status(0xC000_00A2);
const IS_A_DIRECTORY: Status = status(0xC000_00BA);
const NOT_EMPTY: Status = status(0xC000_0101);
const NOT_A_DIRECTORY: Status = status(0xC000_0103);

/// `DOKAN_MINIMUM_COMPATIBLE_VERSION`: the oldest library these declarations are for, which every 2.x takes.
const VERSION: u16 = 200;
/// `DOKAN_OPTION_WRITE_PROTECT`.
const OPTION_WRITE_PROTECT: u32 = 1 << 3;
/// `VOLUME_SECURITY_DESCRIPTOR_MAX_SIZE`.
const SECURITY_DESCRIPTOR_MAX: usize = 1024 * 16;

// What a file is opened or made by, as the kernel says it (`CreateDisposition`, `CreateOptions`, `DesiredAccess`).
const FILE_SUPERSEDE: u32 = 0;
const FILE_CREATE: u32 = 2;
const FILE_OPEN_IF: u32 = 3;
const FILE_OVERWRITE: u32 = 4;
const FILE_OVERWRITE_IF: u32 = 5;
const FILE_DIRECTORY_FILE: u32 = 0x1;
const FILE_NON_DIRECTORY_FILE: u32 = 0x40;
/// `FILE_WRITE_DATA | FILE_APPEND_DATA | GENERIC_WRITE | GENERIC_ALL`: opened to change what a file holds.
const WRITES: u32 = 0x2 | 0x4 | 0x4000_0000 | 0x1000_0000;

const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
const FILE_ATTRIBUTE_NORMAL: u32 = 0x80;

/// `DOKAN_OPTIONS`.
#[repr(C)]
struct Options {
    version: u16,
    single_thread: u8,
    flags: u32,
    global_context: u64,
    mount_point: *const u16,
    unc_name: *const u16,
    timeout: u32,
    allocation_unit_size: u32,
    sector_size: u32,
    volume_security_descriptor_length: u32,
    volume_security_descriptor: [u8; SECURITY_DESCRIPTOR_MAX],
}

/// `DOKAN_FILE_INFO`: what is known of an open file, a call at a time. `context` is this module's: the handle a file
/// opened for writing has in `Writes`, or none.
#[repr(C)]
struct FileInfo {
    context: u64,
    dokan_context: u64,
    dokan_options: *mut Options,
    processing_context: *mut c_void,
    process_id: u32,
    is_directory: u8,
    delete_pending: u8,
    paging_io: u8,
    synchronous_io: u8,
    nocache: u8,
    write_to_end_of_file: u8,
}

type Name = PCWSTR;
type Info = *mut FileInfo;
type Fill = unsafe extern "system" fn(*mut WIN32_FIND_DATAW, Info) -> i32;

/// `DOKAN_OPERATIONS`: what the library calls for each request, in its order. What is left out the library answers
/// by itself.
#[repr(C)]
struct Operations {
    create_file: Option<unsafe extern "system" fn(Name, *mut c_void, u32, u32, u32, u32, u32, Info) -> Status>,
    cleanup: Option<unsafe extern "system" fn(Name, Info)>,
    close_file: Option<unsafe extern "system" fn(Name, Info)>,
    read_file: Option<unsafe extern "system" fn(Name, *mut u8, u32, *mut u32, i64, Info) -> Status>,
    write_file: Option<unsafe extern "system" fn(Name, *const u8, u32, *mut u32, i64, Info) -> Status>,
    flush_file_buffers: Option<unsafe extern "system" fn(Name, Info) -> Status>,
    get_file_information: Option<unsafe extern "system" fn(Name, *mut BY_HANDLE_FILE_INFORMATION, Info) -> Status>,
    find_files: Option<unsafe extern "system" fn(Name, Fill, Info) -> Status>,
    find_files_with_pattern: Option<unsafe extern "system" fn(Name, Name, Fill, Info) -> Status>,
    set_file_attributes: Option<unsafe extern "system" fn(Name, u32, Info) -> Status>,
    set_file_time:
        Option<unsafe extern "system" fn(Name, *const FILETIME, *const FILETIME, *const FILETIME, Info) -> Status>,
    delete_file: Option<unsafe extern "system" fn(Name, Info) -> Status>,
    delete_directory: Option<unsafe extern "system" fn(Name, Info) -> Status>,
    move_file: Option<unsafe extern "system" fn(Name, Name, i32, Info) -> Status>,
    set_end_of_file: Option<unsafe extern "system" fn(Name, i64, Info) -> Status>,
    set_allocation_size: Option<unsafe extern "system" fn(Name, i64, Info) -> Status>,
    lock_file: Option<unsafe extern "system" fn(Name, i64, i64, Info) -> Status>,
    unlock_file: Option<unsafe extern "system" fn(Name, i64, i64, Info) -> Status>,
    get_disk_free_space: Option<unsafe extern "system" fn(*mut u64, *mut u64, *mut u64, Info) -> Status>,
    get_volume_information:
        Option<unsafe extern "system" fn(*mut u16, u32, *mut u32, *mut u32, *mut u32, *mut u16, u32, Info) -> Status>,
    mounted: Option<unsafe extern "system" fn(Name, Info) -> Status>,
    unmounted: Option<unsafe extern "system" fn(Info) -> Status>,
    get_file_security: Option<unsafe extern "system" fn(Name, *mut u32, *mut c_void, u32, *mut u32, Info) -> Status>,
    set_file_security: Option<unsafe extern "system" fn(Name, *mut u32, *mut c_void, u32, Info) -> Status>,
    find_streams: Option<unsafe extern "system" fn(Name, *mut c_void, *mut c_void, Info) -> Status>,
}

/// A filesystem the library runs (`DOKAN_HANDLE`).
type Instance = *mut c_void;

/// Dokany's library, as far as it is used: loaded once, where it is installed, and set up for this process.
struct Api {
    _library: libloading::Library,
    create: unsafe extern "system" fn(*mut Options, *mut Operations, *mut Instance) -> i32,
    wait_closed: unsafe extern "system" fn(Instance, u32) -> u32,
    close: unsafe extern "system" fn(Instance),
    remove_mount_point: unsafe extern "system" fn(PCWSTR) -> i32,
    driver_version: unsafe extern "system" fn() -> u32,
    notify_create: unsafe extern "system" fn(Instance, PCWSTR, i32) -> i32,
    notify_delete: unsafe extern "system" fn(Instance, PCWSTR, i32) -> i32,
    notify_update: unsafe extern "system" fn(Instance, PCWSTR) -> i32,
    /// `DokanIsNameInExpression`: whether a name matches a pattern, as Windows matches them.
    matches: unsafe extern "system" fn(PCWSTR, PCWSTR, i32) -> i32,
}

/// The library, where Dokany is installed.
fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: Dokany's library, loaded for its documented functions, each taken by its documented signature, and
        // set up once before any other of them is called.
        unsafe {
            let library = libloading::Library::new("dokan2.dll").ok()?;
            let init = *library.get::<unsafe extern "system" fn()>(b"DokanInit\0").ok()?;
            let api = Api {
                create: *library.get(b"DokanCreateFileSystem\0").ok()?,
                wait_closed: *library.get(b"DokanWaitForFileSystemClosed\0").ok()?,
                close: *library.get(b"DokanCloseHandle\0").ok()?,
                remove_mount_point: *library.get(b"DokanRemoveMountPoint\0").ok()?,
                driver_version: *library.get(b"DokanDriverVersion\0").ok()?,
                notify_create: *library.get(b"DokanNotifyCreate\0").ok()?,
                notify_delete: *library.get(b"DokanNotifyDelete\0").ok()?,
                notify_update: *library.get(b"DokanNotifyUpdate\0").ok()?,
                matches: *library.get(b"DokanIsNameInExpression\0").ok()?,
                _library: library,
            };
            init();
            Some(api)
        }
    })
    .as_ref()
}

/// What a mount is served from: the tree, what programs wrote over it where it can be written, and what it tells of
/// itself as a volume.
struct Provider {
    served: Served,
    writes: Option<Writes>,
    name: String,
    /// The room there is where it is mounted: free, and in all.
    room: (u64, u64),
    folded: Mutex<Folded>,
}

/// The names in the tree's folders by what they are in lower case, for as long as that tree is served: a name asked
/// for in another case, or one that is not there - what a compiler looking for a header along its paths mostly asks
/// for - is answered by it, where listing the folder each time took longer the more it holds.
#[derive(Default)]
struct Folded {
    of: Option<Arc<Tree>>,
    folders: HashMap<String, Arc<HashMap<String, String>>>,
}

/// A string the library gave a callback, as text.
fn text(given: PCWSTR) -> String {
    // SAFETY: a string the library gave for the call, ended by a zero.
    if given.is_null() { String::new() } else { String::from_utf16_lossy(unsafe { given.as_wide() }) }
}

fn join(parent: &str, name: &str) -> String {
    if parent.is_empty() { name.to_string() } else { format!("{parent}/{name}") }
}

impl Provider {
    /// What a path is: written over the tree where it can be written, else the tree's.
    fn shown(&self, path: &str) -> Option<Shown> {
        if let Some(writes) = &self.writes {
            return writes.shown(path);
        }
        let tree = self.served.tree();
        let (_, node) = tree.at(path)?;
        Some(Shown { folder: matches!(node.what, What::Folder(_)), size: node.size, modified: node.modified })
    }

    /// A folder's children, each whether it is a folder; none where it is no folder.
    fn list(&self, path: &str) -> Option<BTreeMap<String, bool>> {
        if !self.shown(path)?.folder {
            return None;
        }
        if let Some(writes) = &self.writes {
            return Some(writes.list(path));
        }
        let tree = self.served.tree();
        let (_, node) = tree.at(path)?;
        let What::Folder(children) = &node.what else { return None };
        let folder = |ino: &u64| matches!(tree.node(*ino).map(|n| &n.what), Some(What::Folder(_)));
        Some(children.iter().map(|(name, ino)| (name.clone(), folder(ino))).collect())
    }

    /// A path as the tree names it, for one as Windows gives it (`\a\b`): its names matched as Windows matches them
    /// where they are not the same - whatever their case. What is not there keeps the name given.
    fn path(&self, given: PCWSTR) -> String {
        self.resolved(&text(given))
    }

    /// A path as the tree names it, for one as Windows writes it (`path`).
    fn resolved(&self, given: &str) -> String {
        let mut walked = String::new();
        for step in given.split(['\\', '/']).filter(|step| !step.is_empty()) {
            let exact = join(&walked, step);
            if self.shown(&exact).is_some() {
                walked = exact;
                continue;
            }
            let found = self.by_case(&walked, step);
            walked = join(&walked, found.as_deref().unwrap_or(step));
        }
        walked
    }

    /// A name of a folder as it is written there, for one written in whatever case; none where nothing there is
    /// named so.
    fn by_case(&self, folder: &str, name: &str) -> Option<String> {
        let lower = name.to_lowercase();
        let tree = self.served.tree();
        let names = {
            let mut kept = self.folded.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !kept.of.as_ref().is_some_and(|of| Arc::ptr_eq(of, &tree)) {
                *kept = Folded { of: Some(tree.clone()), folders: HashMap::new() };
            }
            if let Some(names) = kept.folders.get(folder) {
                names.clone()
            } else {
                let mut names = HashMap::new();
                if let Some((_, node)) = tree.at(folder)
                    && let What::Folder(children) = &node.what
                {
                    // Of two that differ in case alone, the first as the folder lists them.
                    for name in children.keys() {
                        names.entry(name.to_lowercase()).or_insert_with(|| name.clone());
                    }
                }
                let names = Arc::new(names);
                kept.folders.insert(folder.to_string(), names.clone());
                names
            }
        };
        if let Some(found) = names.get(&lower).filter(|found| self.shown(&join(folder, found)).is_some()) {
            return Some(found.clone());
        }
        // What was written there since, which the tree does not hold yet.
        let staged = self.writes.as_ref()?.staged_in(folder);
        staged.into_iter().find(|staged| staged.to_lowercase() == lower)
    }

    /// A path as it is given, for what is about to bear its last name: the folder it is in as the tree names it
    /// (`path`), the name as written.
    fn named(&self, given: PCWSTR) -> String {
        let given = text(given);
        let given = given.trim_matches(['\\', '/']);
        let (folder, name) = given.rsplit_once(['\\', '/']).unwrap_or(("", given));
        join(&self.resolved(folder), name)
    }

    fn attributes(&self, shown: &Shown) -> u32 {
        match (shown.folder, self.writes.is_some()) {
            (true, _) => FILE_ATTRIBUTE_DIRECTORY,
            (false, true) => FILE_ATTRIBUTE_NORMAL,
            (false, false) => FILE_ATTRIBUTE_READONLY,
        }
    }
}

/// A time as Windows keeps it: hundreds of nanoseconds since 1601.
fn file_time(at: SystemTime) -> FILETIME {
    let since = at.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let ticks = u64::try_from(since.as_nanos() / 100).unwrap_or(0) + 116_444_736_000_000_000;
    #[expect(clippy::cast_possible_truncation, reason = "its two halves")]
    FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 }
}

/// A number's two halves, high then low, as Windows keeps a length.
#[expect(clippy::cast_possible_truncation, reason = "its two halves")]
fn halves(of: u64) -> (u32, u32) {
    ((of >> 32) as u32, of as u32)
}

fn failed(e: &io::Error) -> Status {
    match e.kind() {
        io::ErrorKind::NotFound => NOT_FOUND,
        io::ErrorKind::AlreadyExists => COLLISION,
        io::ErrorKind::PermissionDenied | io::ErrorKind::InvalidInput => ACCESS_DENIED,
        _ => UNSUCCESSFUL,
    }
}

/// What a call is about: the file's information, and the provider of the mount it is in.
///
/// # Safety
/// `info` is what the library gave a callback, for the length of the call.
unsafe fn called<'a>(info: Info) -> Option<(&'a mut FileInfo, &'a Provider)> {
    // SAFETY: the library's for the call; its options are those `mount` gave, their context the mount's provider,
    // which lives until the library has closed the filesystem.
    unsafe {
        let info = info.as_mut()?;
        let provider = (info.dokan_options.as_ref()?.global_context as *const Provider).as_ref()?;
        Some((info, provider))
    }
}

unsafe extern "system" fn create_file(
    name: Name,
    _security: *mut c_void,
    access: u32,
    _attributes: u32,
    _share: u32,
    disposition: u32,
    options: u32,
    info: Info,
) -> Status {
    // SAFETY: a callback's information.
    let Some((info, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    let path = provider.path(name);
    info.context = 0;
    let (wants_folder, wants_file) = (options & FILE_DIRECTORY_FILE != 0, options & FILE_NON_DIRECTORY_FILE != 0);
    // One there already, opened by what would also have made it, is said so.
    let there = if matches!(disposition, FILE_OPEN_IF | FILE_OVERWRITE_IF | FILE_SUPERSEDE) { COLLISION } else { SUCCESS };
    match provider.shown(&path) {
        Some(Shown { folder: true, .. }) => {
            if wants_file {
                return IS_A_DIRECTORY;
            }
            if disposition == FILE_CREATE {
                return COLLISION;
            }
            info.is_directory = 1;
            there
        }
        Some(_) => {
            if wants_folder {
                return NOT_A_DIRECTORY;
            }
            if disposition == FILE_CREATE {
                return COLLISION;
            }
            let truncate = matches!(disposition, FILE_SUPERSEDE | FILE_OVERWRITE | FILE_OVERWRITE_IF);
            if truncate || access & WRITES != 0 {
                let Some(writes) = &provider.writes else { return WRITE_PROTECTED };
                match writes.open(&path, truncate) {
                    Ok(handle) => info.context = handle,
                    Err(e) => return failed(&e),
                }
            }
            // Open until closed: what serving waits for before it stops.
            provider.served.opened();
            provider.served.seen(&path, crate::Seen::Read);
            there
        }
        None => {
            if !matches!(disposition, FILE_CREATE | FILE_OPEN_IF | FILE_OVERWRITE_IF | FILE_SUPERSEDE) {
                return NOT_FOUND;
            }
            let Some(writes) = &provider.writes else { return WRITE_PROTECTED };
            if wants_folder {
                return match writes.make_folder(&path) {
                    Ok(()) => {
                        info.is_directory = 1;
                        SUCCESS
                    }
                    Err(e) => failed(&e),
                };
            }
            match writes.create(&path) {
                Ok(handle) => {
                    info.context = handle;
                    provider.served.opened();
                    SUCCESS
                }
                Err(e) => failed(&e),
            }
        }
    }
}

/// The last handle of an open file let go of: what it wrote applied, and the file or folder removed where that was
/// asked of it.
unsafe extern "system" fn cleanup(name: Name, info: Info) {
    // SAFETY: a callback's information.
    let Some((info, provider)) = (unsafe { called(info) }) else { return };
    let Some(writes) = &provider.writes else { return };
    if info.context != 0 {
        let _ = writes.close(info.context, true);
        info.context = 0;
    }
    if info.delete_pending != 0 {
        let _ = writes.remove(&provider.path(name));
    }
}

unsafe extern "system" fn close_file(_name: Name, info: Info) {
    // SAFETY: a callback's information.
    if let Some((info, provider)) = unsafe { called(info) }
        && info.is_directory == 0
    {
        provider.served.released();
    }
}

unsafe extern "system" fn read_file(
    name: Name,
    buffer: *mut u8,
    length: u32,
    read: *mut u32,
    offset: i64,
    info: Info,
) -> Status {
    // SAFETY: a callback's information.
    let Some((_, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    let path = provider.path(name);
    let offset = u64::try_from(offset).unwrap_or(0);
    // SAFETY: the library's buffer, `length` long, which this call is given to fill.
    let asked = unsafe { std::slice::from_raw_parts_mut(buffer, length as usize) };
    let filled = if let Some(writes) = &provider.writes {
        writes.read_into(&path, offset, asked)
    } else {
        let tree = provider.served.tree();
        match tree.at(&path) {
            Some((node, _)) => tree.read_into(node, offset, asked),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    };
    match filled {
        Ok(filled) => {
            // SAFETY: where the library takes how much of its buffer was filled.
            unsafe { *read = u32::try_from(filled).unwrap_or(u32::MAX) };
            SUCCESS
        }
        Err(e) => failed(&e),
    }
}

unsafe extern "system" fn write_file(
    name: Name,
    buffer: *const u8,
    length: u32,
    written: *mut u32,
    offset: i64,
    info: Info,
) -> Status {
    // SAFETY: a callback's information.
    let Some((info, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    let Some(writes) = &provider.writes else { return WRITE_PROTECTED };
    let path = provider.path(name);
    let size = provider.shown(&path).map_or(0, |shown| shown.size);
    let offset = if info.write_to_end_of_file != 0 { size } else { u64::try_from(offset).unwrap_or(0) };
    // SAFETY: the library's buffer, `length` long.
    let mut data = unsafe { std::slice::from_raw_parts(buffer, length as usize) };
    // What the system writes back of a mapped file never makes it longer.
    if info.paging_io != 0 {
        let room = usize::try_from(size.saturating_sub(offset)).unwrap_or(usize::MAX);
        data = &data[..data.len().min(room)];
    }
    // Written after its last handle was let go of (a mapped file's pages): through a handle of its own.
    let (handle, own) = match info.context {
        0 => match writes.open(&path, false) {
            Ok(handle) => (handle, true),
            Err(e) => return failed(&e),
        },
        handle => (handle, false),
    };
    let wrote = writes.write(handle, offset, data);
    if own {
        let _ = writes.close(handle, true);
    }
    match wrote {
        Ok(count) => {
            // SAFETY: where the library takes how much was written.
            unsafe { *written = u32::try_from(count).unwrap_or(u32::MAX) };
            SUCCESS
        }
        Err(e) => failed(&e),
    }
}

unsafe extern "system" fn flush_file_buffers(_name: Name, info: Info) -> Status {
    // SAFETY: a callback's information.
    let Some((info, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    match provider.writes.as_ref().filter(|_| info.context != 0).map(|writes| writes.close(info.context, false)) {
        Some(Err(e)) => failed(&e),
        _ => SUCCESS,
    }
}

unsafe extern "system" fn get_file_information(
    name: Name,
    told: *mut BY_HANDLE_FILE_INFORMATION,
    info: Info,
) -> Status {
    // SAFETY: a callback's information.
    let Some((_, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    let path = provider.path(name);
    let Some(shown) = provider.shown(&path) else { return NOT_FOUND };
    let (time, (size_high, size_low)) = (file_time(shown.modified), halves(shown.size));
    let (index_high, index_low) = halves(provider.served.ino(&path));
    let information = BY_HANDLE_FILE_INFORMATION {
        dwFileAttributes: provider.attributes(&shown),
        ftCreationTime: time,
        ftLastAccessTime: time,
        ftLastWriteTime: time,
        dwVolumeSerialNumber: SERIAL,
        nFileSizeHigh: size_high,
        nFileSizeLow: size_low,
        nNumberOfLinks: 1,
        nFileIndexHigh: index_high,
        nFileIndexLow: index_low,
    };
    // SAFETY: where the library takes the file's information.
    unsafe { *told = information };
    SUCCESS
}

/// An entry of a folder as a listing gives it, told to the library: nothing, where its name is longer than Windows
/// lists (opened by its path, it is still there).
///
/// # Safety
/// `fill` and `info` are what the library gave the callback telling it.
unsafe fn tell_entry(provider: &Provider, name: &str, shown: &Shown, fill: Fill, info: Info) {
    let (time, (size_high, size_low)) = (file_time(shown.modified), halves(shown.size));
    let mut found = WIN32_FIND_DATAW {
        dwFileAttributes: provider.attributes(shown),
        ftCreationTime: time,
        ftLastAccessTime: time,
        ftLastWriteTime: time,
        nFileSizeHigh: size_high,
        nFileSizeLow: size_low,
        ..WIN32_FIND_DATAW::default()
    };
    let wide: Vec<u16> = name.encode_utf16().collect();
    if wide.len() >= found.cFileName.len() {
        return;
    }
    found.cFileName[..wide.len()].copy_from_slice(&wide);
    // SAFETY: an entry, and the information of this call, as the library's own function takes them.
    unsafe { fill(&raw mut found, info) };
}

unsafe extern "system" fn find_files(name: Name, fill: Fill, info: Info) -> Status {
    // SAFETY: a callback's information.
    let Some((_, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    let path = provider.path(name);
    let Some(children) = provider.list(&path) else { return NOT_FOUND };
    for (child, _) in children {
        if let Some(shown) = provider.shown(&join(&path, &child)) {
            // SAFETY: what the library gave this callback.
            unsafe { tell_entry(provider, &child, &shown, fill, info) };
        }
    }
    SUCCESS
}

/// A folder's entries matching a pattern. What Windows asks of a name - whether it is there, what it is - it asks
/// so, the pattern the name: answered by looking that one entry up, where listing the whole folder to find it would
/// answer with thousands for one.
unsafe extern "system" fn find_files_with_pattern(name: Name, pattern: Name, fill: Fill, info: Info) -> Status {
    // SAFETY: a callback's information.
    let (Some((_, provider)), Some(api)) = (unsafe { called(info) }, api()) else { return UNSUCCESSFUL };
    let (path, wanted) = (provider.path(name), text(pattern));
    if !wanted.contains(['*', '?', '<', '>', '"']) {
        let found = provider.resolved(&join(&path, &wanted));
        if let Some(shown) = provider.shown(&found) {
            let named = found.rsplit('/').next().unwrap_or(&found);
            // SAFETY: what the library gave this callback.
            unsafe { tell_entry(provider, named, &shown, fill, info) };
        }
        return SUCCESS;
    }
    let Some(children) = provider.list(&path) else { return NOT_FOUND };
    for (child, _) in children {
        let candidate = wide(&child);
        // SAFETY: the pattern as the library gave it and a name ended by a zero, matched as Windows matches them.
        let matches = unsafe { (api.matches)(pattern, PCWSTR(candidate.as_ptr()), 1) } != 0;
        if matches && let Some(shown) = provider.shown(&join(&path, &child)) {
            // SAFETY: what the library gave this callback.
            unsafe { tell_entry(provider, &child, &shown, fill, info) };
        }
    }
    SUCCESS
}

/// Attributes and times are the project's to tell, not a program's to set: taken, and left as they are.
unsafe extern "system" fn set_file_attributes(_name: Name, _attributes: u32, info: Info) -> Status {
    // SAFETY: a callback's information.
    match unsafe { called(info) } {
        Some((_, Provider { writes: Some(_), .. })) => SUCCESS,
        Some(_) => WRITE_PROTECTED,
        None => UNSUCCESSFUL,
    }
}

unsafe extern "system" fn set_file_time(
    _name: Name,
    _created: *const FILETIME,
    _accessed: *const FILETIME,
    _written: *const FILETIME,
    info: Info,
) -> Status {
    // SAFETY: a callback's information.
    match unsafe { called(info) } {
        Some((_, Provider { writes: Some(_), .. })) => SUCCESS,
        Some(_) => WRITE_PROTECTED,
        None => UNSUCCESSFUL,
    }
}

/// Whether a file may be removed: it is, once its last handle is let go of (`cleanup`).
unsafe extern "system" fn delete_file(name: Name, info: Info) -> Status {
    // SAFETY: a callback's information.
    let Some((_, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    if provider.writes.is_none() {
        return WRITE_PROTECTED;
    }
    match provider.shown(&provider.path(name)) {
        Some(Shown { folder: false, .. }) => SUCCESS,
        Some(_) => ACCESS_DENIED,
        None => NOT_FOUND,
    }
}

/// Whether a folder may be removed: an empty one, once its last handle is let go of (`cleanup`).
unsafe extern "system" fn delete_directory(name: Name, info: Info) -> Status {
    // SAFETY: a callback's information.
    let Some((_, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    if provider.writes.is_none() {
        return WRITE_PROTECTED;
    }
    match provider.list(&provider.path(name)) {
        Some(children) if children.is_empty() => SUCCESS,
        Some(_) => NOT_EMPTY,
        None => NOT_FOUND,
    }
}

unsafe extern "system" fn move_file(name: Name, to: Name, replace: i32, info: Info) -> Status {
    // SAFETY: a callback's information.
    let Some((_, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    let Some(writes) = &provider.writes else { return WRITE_PROTECTED };
    let from = provider.path(name);
    // What is there under the new name, whatever its case, is what it is renamed over - but itself: renamed to its own
    // name in another case, it takes the name as it is given.
    let there = provider.path(to);
    let to = if there == from { provider.named(to) } else { there };
    if replace == 0 && to != from && provider.shown(&to).is_some() {
        return COLLISION;
    }
    match writes.rename(&from, &to) {
        Ok(()) => SUCCESS,
        Err(e) => failed(&e),
    }
}

/// A file's length set: applied with what its handle writes, or now where it has none.
fn set_length(provider: &Provider, path: &str, length: u64, open: bool) -> Status {
    let Some(writes) = &provider.writes else { return WRITE_PROTECTED };
    if let Err(e) = writes.truncate(path, length) {
        return failed(&e);
    }
    if !open
        && let Ok(handle) = writes.open(path, false)
        && let Err(e) = writes.close(handle, true)
    {
        return failed(&e);
    }
    SUCCESS
}

unsafe extern "system" fn set_end_of_file(name: Name, length: i64, info: Info) -> Status {
    // SAFETY: a callback's information.
    let Some((info, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    set_length(provider, &provider.path(name), u64::try_from(length).unwrap_or(0), info.context != 0)
}

/// The room a file is given: only less than it holds changes it.
unsafe extern "system" fn set_allocation_size(name: Name, size: i64, info: Info) -> Status {
    // SAFETY: a callback's information.
    let Some((info, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    let (path, size) = (provider.path(name), u64::try_from(size).unwrap_or(0));
    match provider.shown(&path) {
        Some(shown) if size < shown.size => set_length(provider, &path, size, info.context != 0),
        Some(_) => SUCCESS,
        None => NOT_FOUND,
    }
}

unsafe extern "system" fn get_disk_free_space(
    available: *mut u64,
    total: *mut u64,
    free: *mut u64,
    info: Info,
) -> Status {
    // SAFETY: a callback's information.
    let Some((_, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    // SAFETY: where the library takes each number, where it asks for it.
    unsafe {
        for (asked, room) in [(available, provider.room.0), (total, provider.room.1), (free, provider.room.0)] {
            if let Some(asked) = asked.as_mut() {
                *asked = room;
            }
        }
    }
    SUCCESS
}

/// The serial number every mount tells: a volume's, of no disk.
const SERIAL: u32 = 0x7065_3200;

/// Write a string into a buffer of the library's, `size` characters long, ended by a zero.
///
/// # Safety
/// `buffer` is `size` characters long, or null.
unsafe fn tell(buffer: *mut u16, size: u32, text: &str) {
    if buffer.is_null() || size == 0 {
        return;
    }
    let wide: Vec<u16> = text.encode_utf16().take(size as usize - 1).chain(std::iter::once(0)).collect();
    // SAFETY: no longer than the buffer.
    unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), buffer, wide.len()) };
}

unsafe extern "system" fn get_volume_information(
    name: *mut u16,
    name_size: u32,
    serial: *mut u32,
    longest_name: *mut u32,
    flags: *mut u32,
    filesystem: *mut u16,
    filesystem_size: u32,
    info: Info,
) -> Status {
    const FILE_CASE_PRESERVED_NAMES: u32 = 0x2;
    const FILE_UNICODE_ON_DISK: u32 = 0x4;
    const FILE_READ_ONLY_VOLUME: u32 = 0x8_0000;
    // SAFETY: a callback's information.
    let Some((_, provider)) = (unsafe { called(info) }) else { return UNSUCCESSFUL };
    let read_only = if provider.writes.is_none() { FILE_READ_ONLY_VOLUME } else { 0 };
    // SAFETY: the library's buffers, of the lengths given with them, and where it takes each number it asks for.
    unsafe {
        tell(name, name_size, &provider.name);
        // What Windows offers of a volume it decides by this name: as the volumes it knows.
        tell(filesystem, filesystem_size, "NTFS");
        for (asked, value) in [
            (serial, SERIAL),
            (longest_name, 255),
            (flags, FILE_CASE_PRESERVED_NAMES | FILE_UNICODE_ON_DISK | read_only),
        ] {
            if let Some(asked) = asked.as_mut() {
                *asked = value;
            }
        }
    }
    SUCCESS
}

/// A string as Windows takes it: UTF-16, ended by a zero.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A tree mounted through Dokany: unmounted when dropped. What the library was given to serve by - the options, the
/// mount point they name, the operations - it reads where they are for as long as it serves, so they are kept here.
pub struct Mounted {
    instance: Instance,
    provider: *const Provider,
    at: PathBuf,
    mount_point: Vec<u16>,
    _options: Box<Options>,
    _operations: Box<Operations>,
    told: Arc<AtomicBool>,
    telling: Option<std::thread::JoinHandle<()>>,
}

// SAFETY: the instance is the library's handle for any thread; the provider is shared, read through its own locks.
unsafe impl Send for Mounted {}

impl Drop for Mounted {
    fn drop(&mut self) {
        self.told.store(true, Ordering::Relaxed);
        if let Some(telling) = self.telling.take() {
            let _ = telling.join();
        }
        if let Some(api) = api() {
            // SAFETY: the mount point and the instance `mount` made, let go of once: unmounted, waited for until no
            // callback runs, then closed.
            unsafe {
                (api.remove_mount_point)(PCWSTR(self.mount_point.as_ptr()));
                (api.wait_closed)(self.instance, u32::MAX);
                (api.close)(self.instance);
            }
        }
        crate::backend::unjoin(&self.at);
        // SAFETY: the provider `mount` made, given back once nothing calls into it.
        drop(unsafe { Arc::from_raw(self.provider) });
    }
}

/// How often a folder just mounted on is asked whether it answers, and how many times: for 20 seconds.
const READY_STEP: std::time::Duration = std::time::Duration::from_millis(20);
const READY_STEPS: u32 = 1000;

/// Why the library would not serve, by what `DokanCreateFileSystem` said.
fn refused(code: i32) -> &'static str {
    match code {
        -2 => "its drive letter is no good",
        -3 => "its driver could not be installed",
        -4 => "its driver is not as it should be",
        -5 => "it could not be mounted there",
        -6 => "the folder is no place to mount on",
        -7 => "its library is not of a version it takes",
        _ => "it failed",
    }
}

/// The room there is on the volume a folder is on: free, and in all. Asked of the folder it is in, never of the folder
/// itself once it is mounted on - that would be asking this.
fn room(at: &Path) -> (u64, u64) {
    let around = wide(&at.parent().unwrap_or(at).to_string_lossy());
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: a path ended by a zero, and where each number is written.
    let asked =
        unsafe { GetDiskFreeSpaceExW(PCWSTR(around.as_ptr()), Some(&raw mut free), Some(&raw mut total), None) };
    if asked.is_ok() { (free, total) } else { (1 << 40, 1 << 40) }
}

/// Mount the tree served at a folder, an empty one made where it is not there: read-only, or written through `writes`.
///
/// # Errors
/// Dokany is not installed, or would not mount there.
pub fn mount(served: &Served, writes: Option<Writes>, at: &Path, name: &str) -> io::Result<Mounted> {
    let api = api().ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "Dokany is not installed"))?;
    let at = crate::backend::plain(at);
    // What a process that ended while it served there left of it, first.
    crate::backend::unjoin(&at);
    std::fs::create_dir_all(&at)?;
    let read_only = writes.is_none();
    let provider = Arc::new(Provider {
        served: served.clone(),
        writes,
        name: name.to_string(),
        room: room(&at),
        folded: Mutex::default(),
    });
    let mount_point = wide(&at.to_string_lossy());
    let raw = Arc::into_raw(provider);
    let mut options = Box::new(Options {
        version: VERSION,
        // One thread pulls a read-only mount's requests, where the library would park one a processor (16 at most) in
        // the pool all its mounts share - enough of them, on a larger machine, that a later mount of this process waits
        // some two seconds for the pool to grow before its folder answers. What is asked of a read-only mount is
        // answered from memory in microseconds, so one thread serves it as well: measured, a build's tools reading
        // their sources through it take no longer. A mount written through keeps them all: applying what a program
        // wrote takes long enough that the rest must not wait for it.
        single_thread: u8::from(read_only),
        flags: if read_only { OPTION_WRITE_PROTECT } else { 0 },
        global_context: raw as u64,
        mount_point: mount_point.as_ptr(),
        unc_name: std::ptr::null(),
        timeout: 0,
        allocation_unit_size: 4096,
        sector_size: 512,
        volume_security_descriptor_length: 0,
        volume_security_descriptor: [0; SECURITY_DESCRIPTOR_MAX],
    });
    let mut operations = Box::new(Operations {
        create_file: Some(create_file),
        cleanup: Some(cleanup),
        close_file: Some(close_file),
        read_file: Some(read_file),
        write_file: Some(write_file),
        flush_file_buffers: Some(flush_file_buffers),
        get_file_information: Some(get_file_information),
        find_files: Some(find_files),
        find_files_with_pattern: Some(find_files_with_pattern),
        set_file_attributes: Some(set_file_attributes),
        set_file_time: Some(set_file_time),
        delete_file: Some(delete_file),
        delete_directory: Some(delete_directory),
        move_file: Some(move_file),
        set_end_of_file: Some(set_end_of_file),
        set_allocation_size: Some(set_allocation_size),
        lock_file: None,
        unlock_file: None,
        get_disk_free_space: Some(get_disk_free_space),
        get_volume_information: Some(get_volume_information),
        mounted: None,
        unmounted: None,
        get_file_security: None,
        set_file_security: None,
        find_streams: None,
    });
    let mut instance: Instance = std::ptr::null_mut();
    // SAFETY: options and operations as the library declares them, kept where they are for as long as it serves, and
    // where it writes the filesystem it made.
    let made = unsafe { (api.create)(&raw mut *options, &raw mut *operations, &raw mut instance) };
    if made != 0 {
        // SAFETY: the provider made above, which nothing calls into.
        drop(unsafe { Arc::from_raw(raw) });
        return Err(io::Error::other(format!("{}: not mounted by Dokany: {}", at.display(), refused(made))));
    }
    let told = Arc::new(AtomicBool::new(false));
    let telling = {
        let (served, stop, at, instance) = (served.clone(), told.clone(), at.clone(), instance as usize);
        std::thread::Builder::new().name("dokan-notify".into()).spawn(move || tell_changes(&served, instance, &at, &stop))?
    };
    let mounted = Mounted {
        instance,
        provider: raw,
        at,
        mount_point,
        _options: options,
        _operations: operations,
        told,
        telling: Some(telling),
    };
    // Mounted, as the library says at once, the folder does not answer yet where the pool the library's mounts share
    // has no thread free for this one - where this process serves another, written through, already: not for a second
    // or two. Waited for, so that what is given its path can open it.
    let answers = |_| {
        std::fs::metadata(&mounted.at).is_ok() || {
            std::thread::sleep(READY_STEP);
            false
        }
    };
    if !(0..READY_STEPS).any(answers) {
        let at = mounted.at.display().to_string();
        drop(mounted);
        return Err(io::Error::other(format!("{at}: mounted by Dokany, and does not answer")));
    }
    Ok(mounted)
}

/// Tell what watches the mount of what changes in the tree served by other means than the mount - the project changed
/// beneath it: each file changed, each node gone or new. Until `stop`.
fn tell_changes(served: &Served, instance: usize, at: &Path, stop: &AtomicBool) {
    let Some(api) = api() else { return };
    let instance = instance as Instance;
    let full = |path: &str| wide(&format!("{}\\{}", at.display(), path.replace('/', "\\")));
    let folder = |tree: &Tree, ino: u64| i32::from(matches!(tree.node(ino).map(|n| &n.what), Some(What::Folder(_))));
    let mut last = served.tree();
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let now = served.tree();
        if Arc::ptr_eq(&last, &now) {
            continue;
        }
        let changes = Tree::changes(&last, &now);
        // SAFETY: the filesystem `mount` made, which lives until this thread is joined, and paths ended by zeros.
        unsafe {
            for node in changes.changed.iter().filter_map(|ino| now.node(*ino)) {
                (api.notify_update)(instance, PCWSTR(full(&node.path).as_ptr()));
            }
            for (parent, ino, name) in &changes.gone {
                if let Some(parent) = last.node(*parent) {
                    let path = full(&join(&parent.path, name));
                    (api.notify_delete)(instance, PCWSTR(path.as_ptr()), folder(&last, *ino));
                }
            }
            for (parent, name) in &changes.new {
                if let Some(parent) = now.node(*parent) {
                    let path = join(&parent.path, name);
                    let is_folder = now.at(&path).map_or(0, |(ino, _)| folder(&now, ino));
                    (api.notify_create)(instance, PCWSTR(full(&path).as_ptr()), is_folder);
                }
            }
        }
        last = now;
    }
}

/// Dokany as a backend: the tree mounted at a folder, written through where it has an owner.
struct AsBackend;

/// Dokany, among Windows' backends.
pub const BACKEND: &dyn Backend = &AsBackend;

impl Backend for AsBackend {
    fn name(&self) -> &'static str {
        "dokan"
    }

    fn label(&self) -> &'static str {
        "Dokany"
    }

    fn can(&self) -> Can {
        Can { write: true, folder: true }
    }

    fn available(&self, _at: &Path) -> Result<(), String> {
        let Some(api) = api() else { return Err("Dokany is not installed (dokan2.dll)".into()) };
        // SAFETY: the library's own function, which takes nothing.
        if unsafe { (api.driver_version)() } == 0 {
            return Err("Dokany's driver does not run".into());
        }
        Ok(())
    }

    fn mount(&self, job: &Job) -> io::Result<Box<dyn Mount>> {
        let writes = match &job.owner {
            Some(owner) => Some(Writes::new(job.served, owner.clone(), job.staging)?),
            None => None,
        };
        Ok(crate::backend::Finishing::of(mount(job.served, writes.clone(), job.at, job.name)?, writes))
    }

    fn clear(&self, at: &Path, _name: &str) {
        // A folder still mounted on by a process that ended: no longer, and a plain folder again.
        let at = crate::backend::plain(at);
        if let Some(api) = api() {
            let mount_point = wide(&at.to_string_lossy());
            // SAFETY: a path ended by a zero.
            unsafe { (api.remove_mount_point)(PCWSTR(mount_point.as_ptr())) };
        }
        crate::backend::unjoin(&at);
    }

    fn retire(&self, at: &Path, name: &str) {
        // Nothing of it is on disk but the folder itself, mounted on: a junction to a volume that is gone. One that
        // leads to a folder is another backend's, kept apart, and stays.
        if crate::backend::joined_to(at).is_none_or(|to| !to.exists()) {
            self.clear(at, name);
        }
    }
}

impl Mount for Mounted {
    fn shown(&self) -> String {
        project::host::shown(&self.at)
    }
}
