//! A filesystem that dresses its own panel.
//!
//! Open a `.checklist` file and each line becomes an entry, with two columns the
//! plugin put there: a tick box you can click, and how long the line is. The
//! panel's usual buttons give way to four of its own, and while anything is left
//! a count appears in the window header.
//!
//! The dressing is declared in two different places, and the split matters. The
//! columns and the tick are slots on the filesystem's own vtable — `columns`,
//! `list_rows`, `cell_clicked` — because they are about what it holds. The
//! toolbar is not: it is declared against the extension, and the filesystem is
//! never asked. What a toolbar looks like is no business of a thing that stores
//! files, and the same filesystem answers frontends that have no toolbar at all.
//!
//! Those frontends are why `init` looks at `kind`. A terminal and a browser draw
//! neither toolbar nor header, and the row a browser draws carries a name, a size
//! and a date and nothing a plugin added — so the dressing is registered in the
//! desktop application and nowhere else. `columns` and `list_rows` are still
//! called there, since every frontend lists a plugin filesystem through the same
//! code, but what they describe is dropped before the panel sees it.
//!
//! Refusing outright — `IC_ERR_NOT_THIS_HOST` — is for a plugin whose whole
//! offering is dressing. This one has a filesystem to give, and a checklist opens
//! in all three.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, needs_up_to, HostCheck, IcBytes, IcColumns, IcDirEntry, IcFsColumn, IcFsHandle,
    IcFsSource, IcFsVTable, IcHost, IcListing, IcRow, IcRows, IC_ABI_VERSION, IC_ACTION_BUTTON,
    IC_ACTION_ENABLED, IC_ACTION_ON, IC_ACTION_SHOWN, IC_ACTION_TOGGLE, IC_CELL_TICKED,
    IC_COLUMN_CHECK, IC_COLUMN_TEXT, IC_ENABLE_ALWAYS, IC_ERR_HOST_TOO_OLD, IC_ERR_HOST_UNKNOWN,
    IC_ERR_INIT_FAILED, IC_HOST_GTK, IC_OK, IC_OPEN_READ, IC_SEEK_END, IC_SEEK_SET, IC_SIDE_RIGHT,
};
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-panel",
    "Hello Panel",
    sdk_version!(),
    "SDK example: a filesystem with columns and a toolbar of its own"
);

pub const EXTENSIONS: &str = ".checklist";

/// The column a tick box lives in. The key is how a click names it coming
/// back, so it has to be something the plugin recognises.
pub const DONE_COLUMN: &str = "checklist_done";
pub const LENGTH_COLUMN: &str = "checklist_length";

pub const TICK_ALL: &str = "checklist.tick_all";
pub const UNTICK_ALL: &str = "checklist.untick_all";
pub const ONLY_REMAINING: &str = "checklist.only_remaining";
pub const TICK_SELECTED: &str = "checklist.tick_selected";

pub const INDICATOR: &str = "checklist.left";

static HOST: AtomicUsize = AtomicUsize::new(0);

/// Saying what an indicator reads where it was never registered talks to nothing.
static INDICATOR_IS_UP: AtomicBool = AtomicBool::new(false);

/// Several mounts can be open at once and one indicator speaks for all of them,
/// so the counts are kept per mount and added up.
fn outstanding() -> &'static std::sync::Mutex<std::collections::BTreeMap<usize, usize>> {
    static HELD: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<usize, usize>>> =
        std::sync::OnceLock::new();
    HELD.get_or_init(|| std::sync::Mutex::new(std::collections::BTreeMap::new()))
}

fn counted() -> std::sync::MutexGuard<'static, std::collections::BTreeMap<usize, usize>> {
    outstanding()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub fn what_is_left(counts: &std::collections::BTreeMap<usize, usize>) -> Option<String> {
    let left: usize = counts.values().sum();
    (left > 0).then(|| format!("{left} left"))
}

/// An indicator with nothing to indicate is better absent than empty, which is
/// what `set_header_visible` is for; `set_header_label` is the text beside it.
fn show_what_is_left() {
    if !INDICATOR_IS_UP.load(Ordering::Relaxed) {
        return;
    }
    let host = host();
    if host.is_null() {
        return;
    }
    let Ok(id) = CString::new(INDICATOR) else {
        return;
    };
    match what_is_left(&counted()) {
        Some(said) => {
            if let Ok(said) = CString::new(said) {
                unsafe { ((*host).set_header_label)(id.as_ptr(), said.as_ptr()) };
            }
            unsafe { ((*host).set_header_visible)(id.as_ptr(), 1) };
        }
        None => unsafe {
            ((*host).set_header_visible)(id.as_ptr(), 0);
        },
    }
}

fn note_what_is_left(handle: IcFsHandle, opened: &Opened) {
    let left = opened.tasks.iter().filter(|task| !task.done).count();
    counted().insert(handle as usize, left);
    show_what_is_left();
}

fn host() -> *const IcHost {
    HOST.load(Ordering::Relaxed) as *const IcHost
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub name: String,
    pub done: bool,
}

/// Nothing is written back: a `.checklist` is read once when it opens. A mount
/// that kept its ticks would write the file itself and keep the path to do it.
pub struct Opened {
    pub tasks: Vec<Task>,
    pub only_remaining: bool,
    // Kept alive for exactly as long as the host is reading what was answered.
    names: Vec<CString>,
    cells: Vec<CString>,
    cell_ptrs: Vec<*const c_char>,
    entries: Vec<IcDirEntry>,
    rows: Vec<IcRow>,
    columns: Vec<IcFsColumn>,
    bytes: Vec<u8>,
    error: CString,
}

/// A task per non-empty line. A line ending in `[x]` starts out done, so a
/// file can be opened already half finished.
pub fn tasks_of(text: &str) -> Vec<Task> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| match line.strip_suffix("[x]") {
            Some(rest) => Task {
                name: rest.trim_end().to_string(),
                done: true,
            },
            None => Task {
                name: line.to_string(),
                done: false,
            },
        })
        .filter(|task| !task.name.is_empty())
        .collect()
}

impl Opened {
    fn new(text: &str) -> Self {
        Self {
            tasks: tasks_of(text),
            only_remaining: false,
            names: Vec::new(),
            cells: Vec::new(),
            cell_ptrs: Vec::new(),
            entries: Vec::new(),
            rows: Vec::new(),
            columns: Vec::new(),
            bytes: Vec::new(),
            error: CString::default(),
        }
    }

    pub fn shown(&self) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|task| !self.only_remaining || !task.done)
            .collect()
    }

    pub fn anything_done(&self) -> bool {
        self.tasks.iter().any(|task| task.done)
    }
}

fn with_opened<R>(handle: IcFsHandle, f: impl FnOnce(&mut Opened) -> R) -> Option<R> {
    if handle.is_null() {
        return None;
    }
    let cell = unsafe { &*(handle as *const RefCell<Opened>) };
    Some(f(&mut cell.borrow_mut()))
}

fn text_of(path: *const c_char) -> String {
    if path.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(path) }
        .to_string_lossy()
        .trim_matches('/')
        .to_string()
}

/// Says the listing has moved on, so the panel asks for it again. Without
/// this a tick would be recorded and nothing would change on screen.
fn listing_moved_on() {
    let host = host();
    if host.is_null() {
        return;
    }
    let Ok(extensions) = CString::new(EXTENSIONS) else {
        return;
    };
    unsafe { ((*host).fs_invalidate)(extensions.as_ptr()) };
}

fn read_whole(source: IcFsSource, path: *const c_char) -> Option<Vec<u8>> {
    let host = host();
    if host.is_null() || path.is_null() {
        return None;
    }
    let stream = unsafe { ((*host).fs_open)(source, path, IC_OPEN_READ) };
    if stream.is_null() {
        return None;
    }
    let end = unsafe { ((*host).fs_seek)(stream, 0, IC_SEEK_END) };
    unsafe { ((*host).fs_seek)(stream, 0, IC_SEEK_SET) };
    let mut held = Vec::with_capacity(end.max(0) as usize);
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = unsafe { ((*host).fs_read)(stream, buffer.as_mut_ptr(), buffer.len() as u64) };
        // A short answer is not the end; only zero is.
        if read <= 0 {
            break;
        }
        held.extend_from_slice(&buffer[..read as usize]);
    }
    unsafe { ((*host).fs_close)(stream) };
    Some(held)
}

extern "C" fn fs_open_in(
    source: IcFsSource,
    path: *const c_char,
    _user_data: *mut c_void,
) -> IcFsHandle {
    // A file with nothing readable in it opens as an empty checklist rather
    // than refusing: the host may have just made it.
    let read = read_whole(source, path).unwrap_or_default();
    let text = String::from_utf8_lossy(&read).to_string();
    Box::into_raw(Box::new(RefCell::new(Opened::new(&text)))) as IcFsHandle
}

extern "C" fn fs_close(handle: IcFsHandle) {
    if handle.is_null() {
        return;
    }
    counted().remove(&(handle as usize));
    show_what_is_left();
    drop(unsafe { Box::from_raw(handle as *mut RefCell<Opened>) });
}

/// `list` is required whether or not a filesystem declares columns; `list_rows`
/// is the optional one that fills them.
extern "C" fn fs_list(handle: IcFsHandle, path: *const c_char) -> IcListing {
    if !text_of(path).is_empty() {
        return IcListing::EMPTY;
    }
    with_opened(handle, |opened| {
        fill_entries(opened);
        IcListing {
            items: opened.entries.as_ptr(),
            count: opened.entries.len() as u32,
        }
    })
    .unwrap_or(IcListing::EMPTY)
}

fn fill_entries(opened: &mut Opened) {
    let shown: Vec<Task> = opened.shown().into_iter().cloned().collect();
    opened.names = shown
        .iter()
        .map(|task| CString::new(task.name.clone()).unwrap_or_default())
        .collect();
    opened.entries = shown
        .iter()
        .zip(opened.names.iter())
        .map(|(task, name)| IcDirEntry {
            name: name.as_ptr(),
            is_dir: 0,
            size: task.name.len() as u64,
            modified: 0,
            permissions: 0,
            has_permissions: 0,
        })
        .collect();
}

extern "C" fn fs_list_rows(handle: IcFsHandle, path: *const c_char) -> IcRows {
    if !text_of(path).is_empty() {
        return IcRows::EMPTY;
    }
    with_opened(handle, |opened| {
        fill_entries(opened);
        note_what_is_left(handle, opened);
        let shown: Vec<Task> = opened.shown().into_iter().cloned().collect();

        // Two cells per row, in the order the columns were declared. One flat
        // store of strings and one of pointers into it; each row points at
        // its own pair.
        opened.cells = shown
            .iter()
            .flat_map(|task| {
                [
                    CString::new(if task.done { IC_CELL_TICKED } else { "" }).unwrap_or_default(),
                    CString::new(task.name.chars().count().to_string()).unwrap_or_default(),
                ]
            })
            .collect();
        opened.cell_ptrs = opened.cells.iter().map(|cell| cell.as_ptr()).collect();

        opened.rows = opened
            .entries
            .iter()
            .enumerate()
            .map(|(at, entry)| IcRow {
                entry: *entry,
                extra: unsafe { opened.cell_ptrs.as_ptr().add(at * 2) },
                extra_count: 2,
            })
            .collect();
        IcRows {
            count: opened.rows.len() as u32,
            items: opened.rows.as_ptr(),
            ..IcRows::EMPTY
        }
    })
    .unwrap_or(IcRows::EMPTY)
}

/// The columns this filesystem adds to the panel's own. They are added, not
/// a replacement: name, size and date stay where they were.
extern "C" fn fs_columns(handle: IcFsHandle) -> IcColumns {
    with_opened(handle, |opened| {
        opened.columns = vec![
            IcFsColumn {
                key: c"checklist_done".as_ptr(),
                title: c"Done".as_ptr(),
                width: 60,
                kind: IC_COLUMN_CHECK,
            },
            IcFsColumn {
                key: c"checklist_length".as_ptr(),
                title: c"Letters".as_ptr(),
                width: 80,
                kind: IC_COLUMN_TEXT,
            },
        ];
        IcColumns {
            count: opened.columns.len() as u32,
            items: opened.columns.as_ptr(),
            ..IcColumns::EMPTY
        }
    })
    .unwrap_or(IcColumns::EMPTY)
}

/// A tick was clicked. The plugin decides what that means and the panel then
/// asks for the listing again, rather than assuming the box changed.
extern "C" fn fs_cell_clicked(
    handle: IcFsHandle,
    _dir: *const c_char,
    name: *const c_char,
    column_key: *const c_char,
    ticked: c_int,
) -> c_int {
    if text_of(column_key) != DONE_COLUMN {
        return IC_ERR_INIT_FAILED;
    }
    let wanted = text_of(name);
    let found = with_opened(handle, |opened| {
        match opened.tasks.iter_mut().find(|task| task.name == wanted) {
            Some(task) => {
                task.done = ticked != 0;
                note_what_is_left(handle, opened);
                true
            }
            None => false,
        }
    })
    .unwrap_or(false);
    if !found {
        return IC_ERR_INIT_FAILED;
    }
    // A tick the user clicked does not change by itself: the plugin is told,
    // decides what it means, and then says the listing has moved on. Without
    // this the box snaps back to whatever the last listing said.
    listing_moved_on();
    IC_OK
}

/// What each of this filesystem's buttons looks like now. Asked every time
/// the toolbar is drawn, so it answers from what the mount already holds.
extern "C" fn fs_action_state(handle: IcFsHandle, action_id: *const c_char) -> u32 {
    let which = text_of(action_id);
    with_opened(handle, |opened| match which.as_str() {
        // Nothing ticked, nothing to untick.
        UNTICK_ALL if !opened.anything_done() => IC_ACTION_SHOWN,
        ONLY_REMAINING if opened.only_remaining => IC_ACTION_SHOWN | IC_ACTION_ON,
        _ => IC_ACTION_SHOWN | IC_ACTION_ENABLED,
    })
    .unwrap_or(ic_plugin_api::IC_ACTION_DEFAULT)
}

///
/// The host answers on the thread that asked, and what it hands over is
/// valid until the next call on that thread — so it is read into strings of
/// this plugin's own here and not held on to.
pub fn what_is_selected() -> Vec<String> {
    let host = host();
    if host.is_null() {
        return Vec::new();
    }
    unsafe { ((*host).selection)() }
        .as_slice()
        .iter()
        .filter_map(|item| item.path_string())
        .collect()
}

pub fn line_of(path: &str) -> Option<String> {
    path.replace('\\', "/")
        .rsplit('/')
        .next()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
}

pub fn tick_what_is_selected(opened: &mut Opened, picked: &[String]) -> usize {
    let wanted: Vec<String> = picked.iter().filter_map(|one| line_of(one)).collect();
    let mut touched = 0;
    for task in opened.tasks.iter_mut() {
        if wanted.contains(&task.name) && !task.done {
            task.done = true;
            touched += 1;
        }
    }
    touched
}

extern "C" fn on_action(mount: IcFsHandle, user_data: *mut c_void, _parent: *mut c_void) {
    if user_data.is_null() {
        return;
    }
    let which = text_of(user_data as *const c_char);
    let picked = what_is_selected();
    let changed = with_opened(mount, |opened| match which.as_str() {
        TICK_SELECTED => tick_what_is_selected(opened, &picked) > 0,
        TICK_ALL => {
            opened.tasks.iter_mut().for_each(|task| task.done = true);
            true
        }
        UNTICK_ALL => {
            opened.tasks.iter_mut().for_each(|task| task.done = false);
            true
        }
        ONLY_REMAINING => {
            opened.only_remaining = !opened.only_remaining;
            true
        }
        _ => false,
    })
    .unwrap_or(false);
    with_opened(mount, |opened| note_what_is_left(mount, opened));
    if changed {
        listing_moved_on();
    }
}

/// The indicator is a button like any other and has to do something when it
/// is pressed. This one only says again what it already shows.
extern "C" fn on_indicator(_user_data: *mut c_void, _parent: *mut c_void) {
    show_what_is_left();
}

extern "C" fn fs_read(handle: IcFsHandle, path: *const c_char) -> IcBytes {
    let wanted = text_of(path);
    with_opened(handle, |opened| {
        let Some(task) = opened.tasks.iter().find(|task| task.name == wanted) else {
            return IcBytes::EMPTY;
        };
        opened.bytes = format!(
            "{}\n{}\n",
            task.name,
            if task.done { "done" } else { "still to do" }
        )
        .into_bytes();
        IcBytes {
            data: opened.bytes.as_ptr(),
            len: opened.bytes.len() as u64,
        }
    })
    .unwrap_or(IcBytes::EMPTY)
}

extern "C" fn fs_read_only(_handle: IcFsHandle) -> c_int {
    1
}

extern "C" fn fs_last_error(handle: IcFsHandle) -> *const c_char {
    with_opened(handle, |opened| {
        opened.error = CString::new("no such line in this checklist").unwrap_or_default();
        opened.error.as_ptr()
    })
    .unwrap_or(std::ptr::null())
}

pub fn vtable() -> IcFsVTable {
    IcFsVTable {
        struct_size: std::mem::size_of::<IcFsVTable>() as u32,
        open_in: fs_open_in,
        close: fs_close,
        list: fs_list,
        read: fs_read,
        is_read_only: fs_read_only,
        last_error: fs_last_error,
        write: None,
        create_dir: None,
        remove: None,
        rename: None,
        shell_open: None,
        shell_read: None,
        shell_write: None,
        shell_resize: None,
        shell_close: None,
        shell_available: None,
        columns: Some(fs_columns),
        list_rows: Some(fs_list_rows),
        action_state: Some(fs_action_state),
        cell_clicked: Some(fs_cell_clicked),
        set_permissions: None,
    }
}

const ACTIONS: &[(&str, &str, &str, u32)] = &[
    (
        TICK_SELECTED,
        include_str!("../assets/tick-selected.svg"),
        "Tick what is selected",
        IC_ACTION_BUTTON,
    ),
    (
        TICK_ALL,
        include_str!("../assets/tick.svg"),
        "Tick everything",
        IC_ACTION_BUTTON,
    ),
    (
        UNTICK_ALL,
        include_str!("../assets/untick.svg"),
        "Untick everything",
        IC_ACTION_BUTTON,
    ),
    (
        ONLY_REMAINING,
        include_str!("../assets/remaining.svg"),
        "Show only what is left",
        IC_ACTION_TOGGLE,
    ),
];

/// The one frontend with somewhere to put a button of a plugin's own, and an
/// indicator to go beside it. A null kind means a host too old to say, and
/// before there was anything to say there was only the desktop.
fn dresses_a_panel(kind: *const c_char) -> bool {
    kind.is_null() || unsafe { CStr::from_ptr(kind) }.to_str() == Ok(IC_HOST_GTK)
}

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_init(host: *const IcHost, kind: *const c_char) -> c_int {
    // The furthest slot this plugin calls. The checklist is read through the
    // host, and those calls sit past every one the panel dressing uses, so
    // asking for `set_default_toolbar_visible` alone would not cover them.
    let needs = needs_up_to(std::mem::offset_of!(IcHost, fs_close));
    match check_host(host, IC_ABI_VERSION, needs) {
        HostCheck::Ok => {}
        HostCheck::WrongMagic => return IC_ERR_HOST_UNKNOWN,
        HostCheck::TooOld { .. } | HostCheck::Truncated { .. } => return IC_ERR_HOST_TOO_OLD,
    }
    HOST.store(host as usize, Ordering::Relaxed);

    let Ok(extensions) = CString::new(EXTENSIONS) else {
        return IC_ERR_INIT_FAILED;
    };
    static TABLE: std::sync::OnceLock<IcFsVTable> = std::sync::OnceLock::new();
    let registered = unsafe {
        ((*host).register_filesystem)(
            extensions.as_ptr(),
            TABLE.get_or_init(vtable),
            std::ptr::null_mut(),
        )
    };
    if registered != IC_OK {
        return registered;
    }

    // Everything below is the dressing, and only the desktop draws any of it.
    INDICATOR_IS_UP.store(false, Ordering::Relaxed);
    if !dresses_a_panel(kind) {
        return IC_OK;
    }

    // The panel's own buttons are about writing into a folder. A checklist is
    // not one, so the toolbar here is what this plugin puts on it.
    unsafe { ((*host).set_default_toolbar_visible)(extensions.as_ptr(), 0) };

    // The indicator: nothing to say until a checklist is open, so it starts
    // out absent rather than empty.
    if let (Ok(id), Ok(svg)) = (
        CString::new(INDICATOR),
        CString::new(include_str!("../assets/left.svg")),
    ) {
        unsafe {
            ((*host).add_header_button)(
                id.as_ptr(),
                svg.as_ptr(),
                c"".as_ptr(),
                c"What is left on the open checklists".as_ptr(),
                IC_SIDE_RIGHT,
                0,
                on_indicator,
                std::ptr::null_mut(),
            );
            ((*host).set_header_visible)(id.as_ptr(), 0);
        }
        INDICATOR_IS_UP.store(true, Ordering::Relaxed);
        std::mem::forget(id);
    }

    for (id, svg, tooltip, behaviour) in ACTIONS {
        let (Ok(id), Ok(svg), Ok(tooltip)) = (
            CString::new(*id),
            CString::new(*svg),
            CString::new(*tooltip),
        ) else {
            continue;
        };
        unsafe {
            ((*host).register_fs_action)(
                extensions.as_ptr(),
                id.as_ptr(),
                svg.as_ptr(),
                tooltip.as_ptr(),
                IC_ENABLE_ALWAYS,
                *behaviour,
                on_action,
                id.as_ptr() as *mut c_void,
            )
        };
        // The host holds the id for as long as the button is on the toolbar,
        // which is for as long as the plugin is loaded.
        std::mem::forget(id);
    }
    IC_OK
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A host of the tests' own: one file, and the four calls this plugin
    /// makes to read it. The plugin is never handed bytes, so neither is it
    /// here — what a test hands over is a filesystem that holds the file.
    mod as_if_hosted {
        use super::*;
        use ic_plugin_api::IcStream;

        /// The one file this filesystem holds, whatever it is asked for.
        pub struct Held(pub Vec<u8>);

        struct Reading {
            of: *const Held,
            at: usize,
        }

        pub extern "C" fn fs_open(source: IcFsSource, _: *const c_char, mode: u32) -> IcStream {
            if source.is_null() || mode != IC_OPEN_READ {
                return std::ptr::null_mut();
            }
            Box::into_raw(Box::new(Reading {
                of: source as *const Held,
                at: 0,
            })) as IcStream
        }

        pub extern "C" fn fs_read(stream: IcStream, into: *mut u8, len: u64) -> i64 {
            let reading = unsafe { &mut *(stream as *mut Reading) };
            let all = unsafe { &(*reading.of).0 };
            let taken = all.len().saturating_sub(reading.at).min(len as usize);
            unsafe { std::ptr::copy_nonoverlapping(all[reading.at..].as_ptr(), into, taken) };
            reading.at += taken;
            taken as i64
        }

        pub extern "C" fn fs_seek(stream: IcStream, offset: i64, whence: u32) -> i64 {
            let reading = unsafe { &mut *(stream as *mut Reading) };
            let all = unsafe { &(*reading.of).0 };
            let from = match whence {
                IC_SEEK_SET => 0,
                IC_SEEK_END => all.len() as i64,
                _ => reading.at as i64,
            };
            reading.at = (from + offset).clamp(0, all.len() as i64) as usize;
            reading.at as i64
        }

        pub extern "C" fn fs_close(stream: IcStream) {
            if !stream.is_null() {
                drop(unsafe { Box::from_raw(stream as *mut Reading) });
            }
        }
    }

    /// The host table the plugin calls back into, installed once: the silent
    /// one the contract brings for tests, with the reads this plugin actually
    /// makes put in over the top. The rest of it answers and does nothing,
    /// which is what the header and the invalidations want here.
    fn hosted() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            let mut table = ic_plugin_api::testing::silent_host();
            table.fs_open = as_if_hosted::fs_open;
            table.fs_read = as_if_hosted::fs_read;
            table.fs_seek = as_if_hosted::fs_seek;
            table.fs_close = as_if_hosted::fs_close;
            let table = Box::leak(Box::new(table));
            HOST.store(table as *const IcHost as usize, Ordering::Relaxed);
            // As a desktop `init` leaves it: the indicator is on the header,
            // so the counting below is told to it rather than dropped.
            INDICATOR_IS_UP.store(true, Ordering::Relaxed);
        });
    }

    /// A mount on a filesystem whose one `.checklist` holds this. The
    /// filesystem outlives the mount, as the host's does; here that is a leak
    /// the test process carries to the end.
    fn open(text: &str) -> IcFsHandle {
        hosted();
        let source = Box::into_raw(Box::new(as_if_hosted::Held(text.as_bytes().to_vec())));
        let name = CString::new("chores.checklist").expect("a name");
        fs_open_in(source as IcFsSource, name.as_ptr(), std::ptr::null_mut())
    }

    fn listed(handle: IcFsHandle) -> Vec<(String, bool, String)> {
        let root = CString::new("").expect("a path");
        let rows = fs_list_rows(handle, root.as_ptr());
        rows.as_slice()
            .iter()
            .map(|row| {
                (
                    row.entry.name_string(),
                    ic_plugin_api::cell_is_ticked(&row.extra_at(0)),
                    row.extra_at(1),
                )
            })
            .collect()
    }

    fn press(handle: IcFsHandle, action: &str) {
        let id = CString::new(action).expect("an id");
        on_action(handle, id.as_ptr() as *mut c_void, std::ptr::null_mut());
    }

    fn state(handle: IcFsHandle, action: &str) -> u32 {
        let id = CString::new(action).expect("an id");
        fs_action_state(handle, id.as_ptr())
    }

    #[test]
    fn a_line_becomes_a_task_and_a_trailing_mark_means_it_is_done() {
        let tasks = tasks_of("buy milk\nwrite the thing [x]\n\n   \nfeed the cat\n");
        assert_eq!(
            tasks,
            vec![
                Task {
                    name: "buy milk".to_string(),
                    done: false
                },
                Task {
                    name: "write the thing".to_string(),
                    done: true
                },
                Task {
                    name: "feed the cat".to_string(),
                    done: false
                },
            ],
            "blank lines are not tasks"
        );
    }

    #[test]
    fn a_file_of_nothing_opens_as_an_empty_list_rather_than_failing() {
        let handle = open("");
        assert!(listed(handle).is_empty());
        fs_close(handle);
    }

    #[test]
    fn each_row_carries_a_tick_and_a_length() {
        let handle = open("buy milk\nwrite the thing [x]\n");
        assert_eq!(
            listed(handle),
            vec![
                ("buy milk".to_string(), false, "8".to_string()),
                ("write the thing".to_string(), true, "15".to_string()),
            ]
        );
        fs_close(handle);
    }

    #[test]
    fn the_columns_are_a_tick_box_and_a_piece_of_text() {
        let handle = open("buy milk\n");
        let columns = fs_columns(handle);
        let declared: Vec<(String, bool)> = columns
            .as_slice()
            .iter()
            .map(|column| (column.key_string(), column.is_check()))
            .collect();
        assert_eq!(
            declared,
            vec![
                (DONE_COLUMN.to_string(), true),
                (LENGTH_COLUMN.to_string(), false),
            ]
        );
        fs_close(handle);
    }

    #[test]
    fn clicking_a_tick_changes_that_one_line() {
        let handle = open("buy milk\nfeed the cat\n");
        let name = CString::new("feed the cat").expect("a name");
        let column = CString::new(DONE_COLUMN).expect("a column");
        let root = CString::new("").expect("a path");
        assert_eq!(
            fs_cell_clicked(handle, root.as_ptr(), name.as_ptr(), column.as_ptr(), 1),
            IC_OK
        );
        assert_eq!(
            listed(handle),
            vec![
                ("buy milk".to_string(), false, "8".to_string()),
                ("feed the cat".to_string(), true, "12".to_string()),
            ]
        );
        fs_close(handle);
    }

    #[test]
    fn a_click_on_a_column_or_a_line_that_is_not_ours_is_refused() {
        let handle = open("buy milk\n");
        let root = CString::new("").expect("a path");
        let known = CString::new("buy milk").expect("a name");
        let missing = CString::new("nothing like this").expect("a name");
        let ours = CString::new(DONE_COLUMN).expect("a column");
        let theirs = CString::new("size").expect("a column");
        assert_eq!(
            fs_cell_clicked(handle, root.as_ptr(), known.as_ptr(), theirs.as_ptr(), 1),
            IC_ERR_INIT_FAILED,
            "the panel's own columns have no boxes to click"
        );
        assert_eq!(
            fs_cell_clicked(handle, root.as_ptr(), missing.as_ptr(), ours.as_ptr(), 1),
            IC_ERR_INIT_FAILED
        );
        fs_close(handle);
    }

    #[test]
    fn a_path_the_panel_shows_is_cut_down_to_the_line_it_stands_for() {
        assert_eq!(
            line_of("/home/u/chores.checklist/buy milk").as_deref(),
            Some("buy milk")
        );
        assert_eq!(line_of("buy milk").as_deref(), Some("buy milk"));
        assert_eq!(
            line_of("C:\\u\\chores.checklist\\buy milk").as_deref(),
            Some("buy milk")
        );
        assert_eq!(line_of("/"), None);
        assert_eq!(line_of(""), None);
    }

    #[test]
    fn ticking_what_is_selected_leaves_the_rest_alone() {
        let handle = open("buy milk\nfeed the cat\nwrite the thing\n");
        let touched = with_opened(handle, |opened| {
            tick_what_is_selected(
                opened,
                &["/home/u/chores.checklist/feed the cat".to_string()],
            )
        })
        .expect("opened");
        assert_eq!(touched, 1);
        assert_eq!(
            listed(handle)
                .into_iter()
                .map(|(name, done, _)| (name, done))
                .collect::<Vec<_>>(),
            vec![
                ("buy milk".to_string(), false),
                ("feed the cat".to_string(), true),
                ("write the thing".to_string(), false),
            ]
        );
        fs_close(handle);
    }

    #[test]
    fn a_selection_that_is_already_ticked_or_from_elsewhere_changes_nothing() {
        let handle = open("buy milk [x]\nfeed the cat\n");
        let touched = with_opened(handle, |opened| {
            tick_what_is_selected(
                opened,
                &[
                    // Already done.
                    "/home/u/chores.checklist/buy milk".to_string(),
                    // Not a line of this checklist at all.
                    "/etc/passwd".to_string(),
                ],
            )
        })
        .expect("opened");
        assert_eq!(touched, 0);
        fs_close(handle);
    }

    #[test]
    fn the_buttons_tick_and_untick_everything() {
        let handle = open("buy milk\nfeed the cat\n");
        press(handle, TICK_ALL);
        assert!(listed(handle).iter().all(|(_, done, _)| *done));
        press(handle, UNTICK_ALL);
        assert!(listed(handle).iter().all(|(_, done, _)| !*done));
        fs_close(handle);
    }

    #[test]
    fn untick_everything_is_dim_while_there_is_nothing_to_untick() {
        let handle = open("buy milk\n");
        assert_eq!(
            state(handle, UNTICK_ALL) & IC_ACTION_ENABLED,
            0,
            "nothing is ticked yet"
        );
        press(handle, TICK_ALL);
        assert_ne!(state(handle, UNTICK_ALL) & IC_ACTION_ENABLED, 0);
        // Shown either way: a toolbar that changes shape under the hand is
        // worse than one with a button that waits.
        assert_ne!(state(handle, UNTICK_ALL) & IC_ACTION_SHOWN, 0);
        fs_close(handle);
    }

    #[test]
    fn the_toggle_stays_pressed_and_hides_what_is_finished() {
        let handle = open("buy milk\nwrite the thing [x]\n");
        assert_eq!(state(handle, ONLY_REMAINING) & IC_ACTION_ON, 0);
        assert_eq!(listed(handle).len(), 2);

        press(handle, ONLY_REMAINING);
        assert_ne!(state(handle, ONLY_REMAINING) & IC_ACTION_ON, 0);
        assert_eq!(
            listed(handle)
                .into_iter()
                .map(|(name, _, _)| name)
                .collect::<Vec<_>>(),
            vec!["buy milk".to_string()]
        );

        press(handle, ONLY_REMAINING);
        assert_eq!(
            listed(handle).len(),
            2,
            "pressing it again brings them back"
        );
        fs_close(handle);
    }

    #[test]
    fn a_host_that_predates_the_columns_still_gets_a_listing() {
        let handle = open("buy milk\nfeed the cat\n");
        let root = CString::new("").expect("a path");
        let listing = fs_list(handle, root.as_ptr());
        let names: Vec<String> = listing
            .as_slice()
            .iter()
            .map(|entry| entry.name_string())
            .collect();
        assert_eq!(
            names,
            vec!["buy milk".to_string(), "feed the cat".to_string()]
        );
        fs_close(handle);
    }

    #[test]
    fn reading_a_line_says_what_it_is_and_whether_it_is_done() {
        let handle = open("buy milk [x]\n");
        let name = CString::new("buy milk").expect("a name");
        let bytes = fs_read(handle, name.as_ptr());
        assert_eq!(
            String::from_utf8_lossy(bytes.as_slice()),
            "buy milk\ndone\n"
        );
        fs_close(handle);
    }

    #[test]
    fn the_indicator_counts_what_is_left_across_every_open_checklist() {
        let mut counts = std::collections::BTreeMap::new();
        assert_eq!(what_is_left(&counts), None, "nothing is open");
        counts.insert(1usize, 0usize);
        assert_eq!(
            what_is_left(&counts),
            None,
            "a checklist with nothing left has nothing to say"
        );
        counts.insert(1usize, 2usize);
        assert_eq!(what_is_left(&counts).as_deref(), Some("2 left"));
        // A plugin can have several of its filesystems open at once, and one
        // indicator speaks for all of them.
        counts.insert(2usize, 3usize);
        assert_eq!(what_is_left(&counts).as_deref(), Some("5 left"));
        counts.remove(&2);
        assert_eq!(what_is_left(&counts).as_deref(), Some("2 left"));
    }

    #[test]
    fn ticking_a_line_brings_the_count_down_and_closing_forgets_it() {
        let handle = open("buy milk\nfeed the cat\n");
        // Listing is what first tells the indicator anything.
        let _ = listed(handle);
        assert_eq!(
            counted().get(&(handle as usize)).copied(),
            Some(2),
            "both are still to do"
        );

        press(handle, TICK_ALL);
        // Only this checklist is looked at: the sum across all of them is
        // what `what_is_left` is for, and it is tested on its own above.
        // Reading it here would depend on what other tests have open.
        assert_eq!(counted().get(&(handle as usize)).copied(), Some(0));

        fs_close(handle);
        assert!(
            !counted().contains_key(&(handle as usize)),
            "a closed checklist is not still counted"
        );
    }

    #[test]
    fn a_row_carries_one_cell_per_declared_column_and_says_how_many() {
        let handle = open("buy milk\n");
        let columns = fs_columns(handle);
        let root = CString::new("").expect("a path");
        let rows = fs_list_rows(handle, root.as_ptr());
        for row in rows.as_slice() {
            // `extra_count` is what bounds the read, not the number of
            // columns: a row that claimed more than it holds would be read
            // past the end of its own cells.
            assert_eq!(row.extra_count as usize, columns.as_slice().len());
        }
        fs_close(handle);
    }

    #[test]
    fn whether_a_button_stays_pressed_is_settled_at_registration() {
        // The widget is built with the panel, long before any checklist is
        // open to be asked, so this is fixed here and only what it looks like
        // moment to moment is asked of the mount.
        let toggles: Vec<&str> = ACTIONS
            .iter()
            .filter(|(_, _, _, kind)| *kind == IC_ACTION_TOGGLE)
            .map(|(id, _, _, _)| *id)
            .collect();
        assert_eq!(toggles, vec![ONLY_REMAINING]);
    }

    /// A host that only counts. Its filesystem reads are the ones above, so a
    /// plugin left pointing at this table by an `init` reads a checklist as
    /// well as it did before.
    mod counting {
        use super::*;

        pub static FILESYSTEMS: AtomicUsize = AtomicUsize::new(0);
        pub static DRESSING: AtomicUsize = AtomicUsize::new(0);

        extern "C" fn filesystem(_: *const c_char, _: *const IcFsVTable, _: *mut c_void) -> c_int {
            FILESYSTEMS.fetch_add(1, Ordering::Relaxed);
            IC_OK
        }

        extern "C" fn default_toolbar(_: *const c_char, _: c_int) -> c_int {
            DRESSING.fetch_add(1, Ordering::Relaxed);
            IC_OK
        }

        extern "C" fn action(
            _: *const c_char,
            _: *const c_char,
            _: *const c_char,
            _: *const c_char,
            _: u32,
            _: u32,
            _: ic_plugin_api::IcFsClickFn,
            _: *mut c_void,
        ) -> c_int {
            DRESSING.fetch_add(1, Ordering::Relaxed);
            IC_OK
        }

        extern "C" fn header(
            _: *const c_char,
            _: *const c_char,
            _: *const c_char,
            _: *const c_char,
            _: u32,
            _: i32,
            _: ic_plugin_api::IcClickFn,
            _: *mut c_void,
        ) -> c_int {
            DRESSING.fetch_add(1, Ordering::Relaxed);
            IC_OK
        }

        pub fn table() -> &'static IcHost {
            let mut built = ic_plugin_api::testing::silent_host();
            built.fs_open = as_if_hosted::fs_open;
            built.fs_read = as_if_hosted::fs_read;
            built.fs_seek = as_if_hosted::fs_seek;
            built.fs_close = as_if_hosted::fs_close;
            built.register_filesystem = filesystem;
            built.set_default_toolbar_visible = default_toolbar;
            built.register_fs_action = action;
            built.add_header_button = header;
            Box::leak(Box::new(built))
        }

        pub fn asked_of(frontend: &str) -> (usize, usize) {
            FILESYSTEMS.store(0, Ordering::Relaxed);
            DRESSING.store(0, Ordering::Relaxed);
            let kind = CString::new(frontend).expect("a plain word");
            assert_eq!(ic_plugin_init(table(), kind.as_ptr()), IC_OK);
            (
                FILESYSTEMS.load(Ordering::Relaxed),
                DRESSING.load(Ordering::Relaxed),
            )
        }
    }

    #[test]
    fn only_the_desktop_is_given_the_dressing_and_every_frontend_the_filesystem() {
        let (mounted, dressed) = counting::asked_of(IC_HOST_GTK);
        assert_eq!(mounted, 1);
        assert_eq!(dressed, ACTIONS.len() + 2, "the toolbar, the indicator");

        // Both of these run the panel through the same code the desktop does,
        // so the checklist opens and lists; neither draws a thing a plugin
        // hung on the panel, so neither is offered one. The two calls share
        // one registry, which is why `asked_of` starts each from nothing.
        for frontend in [ic_plugin_api::IC_HOST_WEB, ic_plugin_api::IC_HOST_CONSOLE] {
            let (mounted, dressed) = counting::asked_of(frontend);
            assert_eq!(mounted, 1, "a checklist still opens in {frontend}");
            assert_eq!(dressed, 0, "and nothing else is asked of {frontend}");
        }
    }

    #[test]
    fn a_host_that_did_not_say_which_one_it_is_is_taken_for_the_desktop() {
        let web = CString::new(ic_plugin_api::IC_HOST_WEB).expect("a plain word");
        let console = CString::new(ic_plugin_api::IC_HOST_CONSOLE).expect("a plain word");
        let desktop = CString::new(IC_HOST_GTK).expect("a plain word");
        // A host from before `init` was told which frontend it is: the desktop
        // was the only one there was, and it is what such a host still gets.
        assert!(dresses_a_panel(std::ptr::null()));
        assert!(dresses_a_panel(desktop.as_ptr()));
        assert!(!dresses_a_panel(web.as_ptr()));
        assert!(!dresses_a_panel(console.as_ptr()));
    }

    #[test]
    fn nothing_reaches_a_handle_that_was_never_opened() {
        let nowhere = std::ptr::null_mut();
        let root = CString::new("").expect("a path");
        assert!(fs_list_rows(nowhere, root.as_ptr()).as_slice().is_empty());
        assert!(fs_columns(nowhere).as_slice().is_empty());
        assert!(fs_read(nowhere, root.as_ptr()).as_slice().is_empty());
        assert_eq!(
            fs_action_state(nowhere, root.as_ptr()),
            ic_plugin_api::IC_ACTION_DEFAULT
        );
    }
}
