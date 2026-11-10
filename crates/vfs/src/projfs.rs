//! A tree served on Windows through the Projected File System, where Windows has it turned on (`Client-ProjFS`): a
//! folder virtualized while it is served, what is in it asked of the tree as programs look - a path opened, a folder
//! listed, a file read - and kept on disk once it was. No sync root is registered with Windows, nothing is shown in
//! Explorer, and a path opened is found without its folder listed first. Its library is loaded when it is first used,
//! so that a Windows without it runs all the same.
//!
//! It serves read-only, and is a backend for that alone (`BACKEND`): Windows refuses to rename or move a folder it
//! projects, whatever its provider does, so a tree written through - the project's folder - is never served by it.
//! What it serves no program changes: its files are read-only, and removing, renaming, linking or writing one is
//! refused to every process but this one, which removes what it no longer serves.

use std::collections::HashMap;
use std::ffi::c_void;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use windows::Win32::Storage::ProjectedFileSystem::{
    PRJ_CALLBACK_DATA, PRJ_CALLBACKS, PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN, PRJ_DIR_ENTRY_BUFFER_HANDLE,
    PRJ_FILE_BASIC_INFO, PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, PRJ_NOTIFICATION, PRJ_NOTIFICATION_FILE_PRE_CONVERT_TO_FULL,
    PRJ_NOTIFICATION_MAPPING, PRJ_NOTIFICATION_PARAMETERS, PRJ_NOTIFICATION_PRE_DELETE, PRJ_NOTIFICATION_PRE_RENAME,
    PRJ_NOTIFICATION_PRE_SET_HARDLINK, PRJ_NOTIFY_FILE_PRE_CONVERT_TO_FULL, PRJ_NOTIFY_PRE_DELETE, PRJ_NOTIFY_PRE_RENAME,
    PRJ_NOTIFY_PRE_SET_HARDLINK, PRJ_PLACEHOLDER_INFO, PRJ_PLACEHOLDER_VERSION_INFO, PRJ_STARTVIRTUALIZING_OPTIONS,
};
use windows_core::{GUID, HRESULT, PCWSTR};

use crate::{Node, Served, Tree, What};

const S_OK: HRESULT = HRESULT(0);
/// `HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)`.
#[expect(clippy::cast_possible_wrap, reason = "an HRESULT's bits")]
const NOT_FOUND: HRESULT = HRESULT(0x8007_0002_u32 as i32);
/// `HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER)`.
#[expect(clippy::cast_possible_wrap, reason = "an HRESULT's bits")]
const FULL: HRESULT = HRESULT(0x8007_007A_u32 as i32);
/// `HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED)`.
#[expect(clippy::cast_possible_wrap, reason = "an HRESULT's bits")]
const DENIED: HRESULT = HRESULT(0x8007_0005_u32 as i32);
/// `E_OUTOFMEMORY`.
#[expect(clippy::cast_possible_wrap, reason = "an HRESULT's bits")]
const NO_MEMORY: HRESULT = HRESULT(0x8007_000E_u32 as i32);

type Mark = unsafe extern "system" fn(PCWSTR, PCWSTR, *const PRJ_PLACEHOLDER_VERSION_INFO, *const GUID) -> HRESULT;
type Start = unsafe extern "system" fn(
    PCWSTR,
    *const PRJ_CALLBACKS,
    *const c_void,
    *const PRJ_STARTVIRTUALIZING_OPTIONS,
    *mut PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
) -> HRESULT;
type Stop = unsafe extern "system" fn(PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT);
type WritePlaceholder =
    unsafe extern "system" fn(PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, PCWSTR, *const PRJ_PLACEHOLDER_INFO, u32) -> HRESULT;
type Fill = unsafe extern "system" fn(PCWSTR, *const PRJ_FILE_BASIC_INFO, PRJ_DIR_ENTRY_BUFFER_HANDLE) -> HRESULT;
type WriteData =
    unsafe extern "system" fn(PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, *const GUID, *const c_void, u64, u32) -> HRESULT;
type Allocate = unsafe extern "system" fn(PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT, usize) -> *mut c_void;
type Free = unsafe extern "system" fn(*const c_void);
type Matches = unsafe extern "system" fn(PCWSTR, PCWSTR) -> u8;
type Compare = unsafe extern "system" fn(PCWSTR, PCWSTR) -> i32;

/// The Projected File System's library, as far as it is used: loaded once, where Windows has it.
struct Api {
    _library: libloading::Library,
    mark: Mark,
    start: Start,
    stop: Stop,
    write_placeholder: WritePlaceholder,
    fill: Fill,
    write_data: WriteData,
    allocate: Allocate,
    free: Free,
    matches: Matches,
    compare: Compare,
}

/// The library, where this Windows has the Projected File System turned on.
fn api() -> Option<&'static Api> {
    static API: OnceLock<Option<Api>> = OnceLock::new();
    API.get_or_init(|| {
        // SAFETY: a system library, loaded for its documented functions, each taken by its documented signature.
        unsafe {
            let library = libloading::Library::new("ProjectedFSLib.dll").ok()?;
            let mark = *library.get::<Mark>(b"PrjMarkDirectoryAsPlaceholder\0").ok()?;
            let start = *library.get::<Start>(b"PrjStartVirtualizing\0").ok()?;
            let stop = *library.get::<Stop>(b"PrjStopVirtualizing\0").ok()?;
            let write_placeholder = *library.get::<WritePlaceholder>(b"PrjWritePlaceholderInfo\0").ok()?;
            let fill = *library.get::<Fill>(b"PrjFillDirEntryBuffer\0").ok()?;
            let write_data = *library.get::<WriteData>(b"PrjWriteFileData\0").ok()?;
            let allocate = *library.get::<Allocate>(b"PrjAllocateAlignedBuffer\0").ok()?;
            let free = *library.get::<Free>(b"PrjFreeAlignedBuffer\0").ok()?;
            let matches = *library.get::<Matches>(b"PrjFileNameMatch\0").ok()?;
            let compare = *library.get::<Compare>(b"PrjFileNameCompare\0").ok()?;
            Some(Api {
                _library: library,
                mark,
                start,
                stop,
                write_placeholder,
                fill,
                write_data,
                allocate,
                free,
                matches,
                compare,
            })
        }
    })
    .as_ref()
}

/// A folder listed, as Windows asks for it in turns: its entries in Windows' order, how far it was told, and the
/// pattern it is filtered by.
struct Listing {
    entries: Vec<(Vec<u16>, PRJ_FILE_BASIC_INFO)>,
    next: usize,
    pattern: Option<Vec<u16>>,
}

/// What a virtualized folder is served from: the tree, and the listings under way.
struct Provider {
    served: Served,
    listings: Mutex<HashMap<u128, Listing>>,
}

/// A folder served through the Projected File System: no longer once dropped, what it kept on disk left to whoever
/// removes the folder.
pub struct Projected {
    context: PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT,
    provider: *const Provider,
    at: PathBuf,
}

// SAFETY: the context is the library's handle for any thread; the provider is shared, read through its locks.
unsafe impl Send for Projected {}
// SAFETY: as above.
unsafe impl Sync for Projected {}

impl Drop for Projected {
    fn drop(&mut self) {
        if let Some(api) = api() {
            // SAFETY: the context the library gave, stopped once; no callback runs after it returns.
            unsafe { (api.stop)(self.context) };
        }
        // SAFETY: the provider made by `serve`, given back once nothing calls into it.
        drop(unsafe { std::sync::Arc::from_raw(self.provider) });
    }
}

/// Serve a tree at a folder, made empty for it here, through the Projected File System; what is not there yet looked
/// up in the tree as it is then.
///
/// # Errors
/// Windows has no Projected File System turned on, or the folder cannot be made or virtualized.
pub fn serve(served: &Served, at: &Path) -> io::Result<Projected> {
    let api = api().ok_or_else(|| io::Error::new(io::ErrorKind::Unsupported, "no Projected File System here"))?;
    std::fs::create_dir_all(at)?;
    let root = wide(&at.to_string_lossy());
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let instance = GUID::from_u128(nanos ^ (u128::from(std::process::id()) << 96));
    // SAFETY: a folder's path, no target, no version, an instance id: as documented.
    let marked = unsafe { (api.mark)(PCWSTR(root.as_ptr()), PCWSTR::null(), std::ptr::null(), &raw const instance) };
    if marked.is_err() {
        return Err(io::Error::other(format!("{}: not virtualized ({marked:?})", at.display())));
    }
    let provider = std::sync::Arc::new(Provider { served: served.clone(), listings: Mutex::default() });
    let callbacks = PRJ_CALLBACKS {
        StartDirectoryEnumerationCallback: Some(start_listing),
        EndDirectoryEnumerationCallback: Some(end_listing),
        GetDirectoryEnumerationCallback: Some(list),
        GetPlaceholderInfoCallback: Some(placeholder),
        GetFileDataCallback: Some(file_data),
        NotificationCallback: Some(notified),
        ..PRJ_CALLBACKS::default()
    };
    // Told, before it happens, of what would change what is served: each refused (`notified`).
    let everywhere = wide("");
    let mut told = [PRJ_NOTIFICATION_MAPPING {
        NotificationBitMask: PRJ_NOTIFY_PRE_DELETE
            | PRJ_NOTIFY_PRE_RENAME
            | PRJ_NOTIFY_PRE_SET_HARDLINK
            | PRJ_NOTIFY_FILE_PRE_CONVERT_TO_FULL,
        NotificationRoot: PCWSTR(everywhere.as_ptr()),
    }];
    let options = PRJ_STARTVIRTUALIZING_OPTIONS {
        NotificationMappings: told.as_mut_ptr(),
        NotificationMappingsCount: 1,
        ..PRJ_STARTVIRTUALIZING_OPTIONS::default()
    };
    let raw = std::sync::Arc::into_raw(provider);
    let mut context = PRJ_NAMESPACE_VIRTUALIZATION_CONTEXT(std::ptr::null_mut());
    // SAFETY: the root marked above, callbacks of the documented signatures, the provider they are given, and options
    // that live through the call.
    let started =
        unsafe { (api.start)(PCWSTR(root.as_ptr()), &raw const callbacks, raw.cast(), &raw const options, &raw mut context) };
    if started.is_err() {
        // SAFETY: the provider made above, which nothing calls into.
        drop(unsafe { std::sync::Arc::from_raw(raw) });
        return Err(io::Error::other(format!("{}: not virtualized ({started:?})", at.display())));
    }
    Ok(Projected { context, provider: raw, at: at.to_path_buf() })
}

/// A string as Windows takes it: UTF-16, ended by a zero.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A string Windows gave, ended by a zero.
fn text_of(given: PCWSTR) -> String {
    if given.is_null() {
        return String::new();
    }
    // SAFETY: a string the library gave for the call, ended by a zero.
    String::from_utf16_lossy(unsafe { given.as_wide() })
}

/// The provider a callback is made for.
fn provider<'a>(data: *const PRJ_CALLBACK_DATA) -> Option<(&'a PRJ_CALLBACK_DATA, &'a Provider)> {
    // SAFETY: the callback data the library gives for the call, its instance context the provider `serve` gave.
    let data = unsafe { data.as_ref()? };
    let provider = unsafe { data.InstanceContext.cast::<Provider>().as_ref()? };
    Some((data, provider))
}

/// A node by the path Windows names it by (relative, `\`-separated), its names matched as Windows matches them where
/// they are not the same.
fn node_at(tree: &Tree, path: &str) -> Option<(u64, Node)> {
    let mut ino = crate::ROOT;
    for step in path.split(['\\', '/']).filter(|s| !s.is_empty()) {
        ino = match tree.lookup(ino, step) {
            Some(found) => found,
            None => match &tree.node(ino)?.what {
                What::Folder(children) => *children.iter().find(|(name, _)| name.eq_ignore_ascii_case(step))?.1,
                What::File(_) => return None,
            },
        };
    }
    Some((ino, tree.node(ino)?.clone()))
}

/// A node as Windows is told it: a file read-only.
fn basic(node: &Node) -> PRJ_FILE_BASIC_INFO {
    const FILE_ATTRIBUTE_READONLY: u32 = 0x1;
    let folder = matches!(node.what, What::Folder(_));
    // As a `FILETIME`: hundreds of nanoseconds since 1601.
    let since = node.modified.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default();
    let time = i64::try_from(since.as_nanos() / 100).unwrap_or(0) + 116_444_736_000_000_000;
    PRJ_FILE_BASIC_INFO {
        IsDirectory: folder,
        FileSize: i64::try_from(node.size).unwrap_or(i64::MAX),
        CreationTime: time,
        LastAccessTime: time,
        LastWriteTime: time,
        ChangeTime: time,
        FileAttributes: if folder { 0 } else { FILE_ATTRIBUTE_READONLY },
    }
}

unsafe extern "system" fn placeholder(data: *const PRJ_CALLBACK_DATA) -> HRESULT {
    let (Some((data, provider)), Some(api)) = (provider(data), api()) else { return NOT_FOUND };
    let tree = provider.served.tree();
    let Some((_, node)) = node_at(&tree, &text_of(data.FilePathName)) else { return NOT_FOUND };
    let info = PRJ_PLACEHOLDER_INFO { FileBasicInfo: basic(&node), ..PRJ_PLACEHOLDER_INFO::default() };
    // Named as the tree names it.
    let name = wide(&node.path.replace('/', "\\"));
    let size = u32::try_from(std::mem::size_of::<PRJ_PLACEHOLDER_INFO>()).unwrap_or(u32::MAX);
    // SAFETY: the context of this instance, a path and its placeholder's information, as documented.
    unsafe { (api.write_placeholder)(data.NamespaceVirtualizationContext, PCWSTR(name.as_ptr()), &raw const info, size) }
}

unsafe extern "system" fn start_listing(data: *const PRJ_CALLBACK_DATA, id: *const GUID) -> HRESULT {
    let (Some((data, provider)), Some(api)) = (provider(data), api()) else { return NOT_FOUND };
    // SAFETY: the listing's id, as the library gives it.
    let Some(id) = (unsafe { id.as_ref() }) else { return NOT_FOUND };
    let tree = provider.served.tree();
    // A folder still on disk that the tree no longer has holds nothing more than what is on disk.
    let children = match node_at(&tree, &text_of(data.FilePathName)) {
        Some((_, Node { what: What::Folder(children), .. })) => children,
        _ => std::collections::BTreeMap::new(),
    };
    let mut entries: Vec<(Vec<u16>, PRJ_FILE_BASIC_INFO)> =
        children.iter().filter_map(|(name, ino)| Some((wide(name), basic(tree.node(*ino)?)))).collect();
    // In Windows' order of names, which is how it is listed.
    // SAFETY: two strings ended by zeros.
    entries.sort_by(|a, b| unsafe { (api.compare)(PCWSTR(a.0.as_ptr()), PCWSTR(b.0.as_ptr())) }.cmp(&0));
    let listing = Listing { entries, next: 0, pattern: None };
    provider.listings.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(id.to_u128(), listing);
    S_OK
}

unsafe extern "system" fn end_listing(data: *const PRJ_CALLBACK_DATA, id: *const GUID) -> HRESULT {
    if let (Some((_, provider)), Some(id)) = (provider(data), unsafe { id.as_ref() }) {
        provider.listings.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id.to_u128());
    }
    S_OK
}

unsafe extern "system" fn list(
    data: *const PRJ_CALLBACK_DATA,
    id: *const GUID,
    pattern: PCWSTR,
    buffer: PRJ_DIR_ENTRY_BUFFER_HANDLE,
) -> HRESULT {
    let (Some((data, provider)), Some(api)) = (provider(data), api()) else { return NOT_FOUND };
    // SAFETY: the listing's id, as the library gives it.
    let Some(id) = (unsafe { id.as_ref() }) else { return NOT_FOUND };
    let mut listings = provider.listings.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(listing) = listings.get_mut(&id.to_u128()) else { return NOT_FOUND };
    // A listing begun again, or begun: its pattern is the one given now.
    if data.Flags.0 & PRJ_CB_DATA_FLAG_ENUM_RESTART_SCAN.0 != 0 {
        listing.next = 0;
        listing.pattern = None;
    }
    if listing.pattern.is_none() {
        let given = text_of(pattern);
        listing.pattern = Some(wide(if given.is_empty() { "*" } else { &given }));
    }
    let pattern = listing.pattern.clone().unwrap_or_else(|| wide("*"));
    while let Some((name, info)) = listing.entries.get(listing.next) {
        // SAFETY: two strings ended by zeros.
        if unsafe { (api.matches)(PCWSTR(name.as_ptr()), PCWSTR(pattern.as_ptr())) } != 0 {
            // SAFETY: a name, its information and the buffer the library gave for this call.
            let filled = unsafe { (api.fill)(PCWSTR(name.as_ptr()), std::ptr::from_ref(info), buffer) };
            if filled == FULL {
                break;
            }
            if filled.is_err() {
                return filled;
            }
        }
        listing.next += 1;
    }
    S_OK
}

unsafe extern "system" fn file_data(data: *const PRJ_CALLBACK_DATA, offset: u64, length: u32) -> HRESULT {
    let (Some((data, provider)), Some(api)) = (provider(data), api()) else { return NOT_FOUND };
    let tree = provider.served.tree();
    let Some((ino, node)) = node_at(&tree, &text_of(data.FilePathName)) else { return NOT_FOUND };
    // Read, and kept whole from now on: a file is filled once, from its start.
    if offset == 0 {
        provider.served.seen(&node.path, crate::Seen::Kept(node.size));
    }
    let context = data.NamespaceVirtualizationContext;
    // SAFETY: a buffer of the library's for this instance, as long as what is asked for, read into where it is, as
    // much of it written as was read, and freed after.
    unsafe {
        let buffer = (api.allocate)(context, (length as usize).max(1));
        if buffer.is_null() {
            return NO_MEMORY;
        }
        let asked = std::slice::from_raw_parts_mut(buffer.cast::<u8>(), length as usize);
        let written = match tree.read_into(ino, offset, asked) {
            Ok(read) => {
                let read = u32::try_from(read).unwrap_or(u32::MAX);
                (api.write_data)(context, &raw const data.DataStreamId, buffer, offset, read)
            }
            Err(_) => NOT_FOUND,
        };
        (api.free)(buffer);
        written
    }
}

/// What is about to change what is served, refused: it is served read-only. This process's own is let through - it
/// removes what it no longer serves.
unsafe extern "system" fn notified(
    data: *const PRJ_CALLBACK_DATA,
    _folder: bool,
    notification: PRJ_NOTIFICATION,
    _destination: PCWSTR,
    _parameters: *mut PRJ_NOTIFICATION_PARAMETERS,
) -> HRESULT {
    // SAFETY: the callback data the library gives for the call.
    let own = unsafe { data.as_ref() }.is_some_and(|data| data.TriggeringProcessId == std::process::id());
    let changes = [
        PRJ_NOTIFICATION_PRE_DELETE,
        PRJ_NOTIFICATION_PRE_RENAME,
        PRJ_NOTIFICATION_PRE_SET_HARDLINK,
        PRJ_NOTIFICATION_FILE_PRE_CONVERT_TO_FULL,
    ];
    if !own && changes.contains(&notification) { DENIED } else { S_OK }
}

/// The Projected File System as a backend: the tree at a folder, read-only.
struct AsBackend;

/// The Projected File System, among Windows' backends.
pub const BACKEND: &dyn crate::backend::Backend = &AsBackend;

impl crate::backend::Backend for AsBackend {
    fn name(&self) -> &'static str {
        "projfs"
    }

    fn label(&self) -> &'static str {
        "Projected File System"
    }

    fn can(&self) -> crate::backend::Can {
        crate::backend::Can { write: false, folder: true }
    }

    fn keeps(&self) -> bool {
        true
    }

    fn available(&self, at: &Path) -> Result<(), String> {
        if api().is_none() {
            return Err("the Projected File System is not turned on (the Windows feature `Client-ProjFS`)".into());
        }
        match crate::backend::volume_filesystem(at) {
            Some(filesystem) if !["NTFS", "ReFS"].iter().any(|known| filesystem.eq_ignore_ascii_case(known)) => {
                Err(format!("the Projected File System serves on NTFS and ReFS volumes, and this one is {filesystem}"))
            }
            _ => Ok(()),
        }
    }

    fn mount(&self, job: &crate::backend::Job) -> io::Result<Box<dyn crate::backend::Mount>> {
        Ok(Box::new(serve(job.served, job.at)?))
    }

    fn clear(&self, at: &Path, _name: &str) {
        // What it kept on disk of what was read: plain files to Windows once nothing virtualizes the folder.
        let _ = std::fs::remove_dir_all(at);
    }
}

impl crate::backend::Mount for Projected {
    fn shown(&self) -> String {
        project::host::shown(&self.at)
    }

    fn released(&self, path: &str) {
        // What was read of it is kept on disk until removed.
        let _ = std::fs::remove_dir_all(self.at.join(path));
    }
}
