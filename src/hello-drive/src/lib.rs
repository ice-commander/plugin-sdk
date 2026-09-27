//! Entries of a plugin's own in the drives list.
//!
//! Two shelves appear beside the real disks, each remembering how often it has
//! been opened, in whatever language the application is showing. This is the
//! shape for a share on another machine, a device, a service.
//!
//! Three things are not obvious.
//!
//! **The rows are pulled, not pushed.** `IcDrivesFn` is asked every time the list
//! is drawn, on the frontend's own thread, so it must answer from what the plugin
//! already holds rather than go to a network. When something does change,
//! `drives_changed` says so and the host asks again.
//!
//! **A key has to survive a restart.** Favourites and the highlight on the open
//! drive hang off it, so it is a name that means the same thing tomorrow, not a
//! number that happens to be handy today.
//!
//! **A kind needs no form.** The shelves are the plugin's, not something the user
//! adds by hand, so the kind is registered without a document: it mounts what the
//! drives list hands it and is never offered in the dialog to create.
//!
//! The pinned entry in the connections list is the one call `kind` guards — that
//! list is the desktop's. Nothing is lost by leaving it out elsewhere: what it
//! opens is the page, and the page is registered everywhere.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, needs_pinned_connections, HostCheck, IcBytes, IcConnectionVTable, IcDirEntry,
    IcDrive, IcDrives, IcFsHandle, IcFsSource, IcFsVTable, IcHost, IcListing, IcPinnedConnection,
    IcPinnedConnections, IcViewVTable, IC_ABI_VERSION, IC_ERR_HOST_TOO_OLD, IC_ERR_HOST_UNKNOWN,
    IC_ERR_INIT_FAILED, IC_HOST_GTK, IC_OK, IC_SETTING_PLAIN,
};
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-drive",
    "Hello Drive",
    sdk_version!(),
    "SDK example: entries of a plugin's own in the drives list"
);

pub const ID: &str = "sdk-hello-drive";
pub const KIND: &str = "sdk.shelf";
pub const VIEW: &str = "sdk.shelves";
pub const PINNED: &str = "shelves";
pub const ICON: &str = include_str!("../assets/shelf.svg");

/// A real plugin would discover its shelves; these are written down.
pub const SHELVES: [(&str, &str); 2] = [("upper", "Upper shelf"), ("lower", "Lower shelf")];

/// A drive has to be mountable, so there is a filesystem behind it.
const HOLDS: [&str; 2] = ["a jar", "a lamp"];

static HOST: AtomicUsize = AtomicUsize::new(0);

fn host() -> *const IcHost {
    HOST.load(Ordering::Relaxed) as *const IcHost
}

pub fn note(said: &str) {
    let host = host();
    if host.is_null() {
        return;
    }
    if let Ok(line) = CString::new(format!("hello-drive: {said}")) {
        unsafe { ((*host).log_warn)(line.as_ptr()) };
    }
}

// ---------------------------------------------------------------- remembering

/// The host keeps each plugin's settings apart, and a value written with
/// `IC_SETTING_SECRET` is encrypted; this is not a secret.
pub fn opened_so_far(shelf: &str) -> u64 {
    let host = host();
    if host.is_null() {
        return 0;
    }
    let (Ok(id), Ok(key)) = (CString::new(ID), CString::new(format!("opened.{shelf}"))) else {
        return 0;
    };
    let held = unsafe { ((*host).settings_read)(id.as_ptr(), key.as_ptr()) };
    String::from_utf8_lossy(held.as_slice())
        .trim()
        .parse()
        .unwrap_or(0)
}

pub fn remember_opening(shelf: &str) {
    let host = host();
    if host.is_null() {
        return;
    }
    let counted = (opened_so_far(shelf) + 1).to_string();
    let (Ok(id), Ok(key)) = (CString::new(ID), CString::new(format!("opened.{shelf}"))) else {
        return;
    };
    unsafe {
        ((*host).settings_write)(
            id.as_ptr(),
            key.as_ptr(),
            counted.as_ptr(),
            counted.len() as u64,
            IC_SETTING_PLAIN,
        )
    };
    said_again();
}

/// A null value is how a key is forgotten.
pub fn forget_openings() {
    let host = host();
    if host.is_null() {
        return;
    }
    let Ok(id) = CString::new(ID) else {
        return;
    };
    for (shelf, _) in SHELVES {
        let Ok(key) = CString::new(format!("opened.{shelf}")) else {
            continue;
        };
        unsafe {
            ((*host).settings_write)(
                id.as_ptr(),
                key.as_ptr(),
                std::ptr::null(),
                0,
                IC_SETTING_PLAIN,
            )
        };
    }
    said_again();
}

fn said_again() {
    let host = host();
    if host.is_null() {
        return;
    }
    unsafe {
        ((*host).drives_changed)();
        ((*host).pinned_connections_changed)();
    }
}

// -------------------------------------------------------------------- words

pub const LOCALES: &[(&str, &str)] = &[
    ("en", include_str!("../locales/en.json")),
    ("ru", include_str!("../locales/ru.json")),
    ("pl", include_str!("../locales/pl.json")),
    ("cs", include_str!("../locales/cs.json")),
    ("sk", include_str!("../locales/sk.json")),
    ("de", include_str!("../locales/de.json")),
    ("es", include_str!("../locales/es.json")),
    ("uk", include_str!("../locales/uk.json")),
    ("it", include_str!("../locales/it.json")),
    ("fr", include_str!("../locales/fr.json")),
    ("ro", include_str!("../locales/ro.json")),
    ("hu", include_str!("../locales/hu.json")),
    ("be", include_str!("../locales/be.json")),
    ("bg", include_str!("../locales/bg.json")),
    ("sr", include_str!("../locales/sr.json")),
];

static SPOKEN: std::sync::OnceLock<String> = std::sync::OnceLock::new();

type Catalogue = std::collections::HashMap<String, std::collections::HashMap<String, String>>;

fn catalogues() -> &'static Catalogue {
    static PARSED: std::sync::OnceLock<Catalogue> = std::sync::OnceLock::new();
    PARSED.get_or_init(|| {
        LOCALES
            .iter()
            .filter_map(|(language, raw)| {
                serde_json::from_str(raw)
                    .ok()
                    .map(|table| ((*language).to_string(), table))
            })
            .collect()
    })
}

/// English behind whatever the language has, and the key itself behind that.
pub fn phrase(language: &str, key: &str) -> String {
    let tables = catalogues();
    tables
        .get(language)
        .and_then(|table| table.get(key))
        .or_else(|| tables.get("en").and_then(|table| table.get(key)))
        .cloned()
        .unwrap_or_else(|| key.to_string())
}

pub fn language() -> &'static str {
    SPOKEN.get().map(String::as_str).unwrap_or("en")
}

/// A column heading or a tooltip may travel as a key and be translated by
/// whichever frontend draws it. This is neither: it goes into a list as text,
/// so the plugin picks the words itself, which is what `language` is for.
pub fn how_often(language: &str, opened: u64) -> String {
    match opened {
        0 => phrase(language, "hello_drive.never_opened"),
        1 => phrase(language, "hello_drive.opened_once"),
        many => phrase(language, "hello_drive.opened_times").replace("{count}", &many.to_string()),
    }
}

// ------------------------------------------------------------------- drives

thread_local! {
    /// What the last answer is made of. `IcDrivesFn` hands out pointers, so
    /// what they point at has to outlive the call.
    static HELD: RefCell<(Vec<CString>, Vec<IcDrive>)> =
        const { RefCell::new((Vec::new(), Vec::new())) };
}

/// Asked every time the lists are drawn, on the frontend's own thread.
extern "C" fn drives(_user_data: *mut c_void) -> IcDrives {
    let spoken = language();
    let mut text: Vec<CString> = Vec::new();
    for (key, name) in SHELVES {
        text.push(CString::new(format!("sdk.shelf.{key}")).unwrap_or_default());
        text.push(CString::new(name).unwrap_or_default());
        text.push(CString::new(how_often(spoken, opened_so_far(key))).unwrap_or_default());
        text.push(
            CString::new(serde_json::json!({ "shelf": key }).to_string()).unwrap_or_default(),
        );
    }
    let rows: Vec<IcDrive> = (0..SHELVES.len())
        .map(|at| IcDrive {
            key: text[at * 4].as_ptr(),
            name: text[at * 4 + 1].as_ptr(),
            subtitle: text[at * 4 + 2].as_ptr(),
            settings: text[at * 4 + 3].as_ptr(),
            // Null falls back to the picture the source was registered with,
            // which is right when every row looks alike.
            svg: std::ptr::null(),
            online: 1,
        })
        .collect();
    HELD.with(|held| {
        *held.borrow_mut() = (text, rows);
        let borrowed = held.borrow();
        IcDrives {
            count: borrowed.1.len() as u32,
            rows: borrowed.1.as_ptr(),
            ..IcDrives::EMPTY
        }
    })
}

// --------------------------------------------------------------- the mount

struct Opened {
    shelf: String,
    names: Vec<CString>,
    view: Vec<IcDirEntry>,
    bytes: Vec<u8>,
    error: CString,
}

fn with_opened<R>(handle: IcFsHandle, f: impl FnOnce(&mut Opened) -> R) -> Option<R> {
    if handle.is_null() {
        return None;
    }
    let cell = unsafe { &*(handle as *const RefCell<Opened>) };
    Some(f(&mut cell.borrow_mut()))
}

/// Which shelf the settings name. They are the same settings the drive row
/// carried, so this is how a pick in the list reaches the right thing.
pub fn shelf_of(settings: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(settings).ok()?;
    let named = parsed["shelf"].as_str()?.to_string();
    SHELVES
        .iter()
        .any(|(key, _)| *key == named)
        .then_some(named)
}

extern "C" fn kind_open(settings: *const u8, len: u64, _user_data: *mut c_void) -> IcFsHandle {
    let raw = if settings.is_null() || len == 0 {
        String::new()
    } else {
        String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(settings, len as usize) })
            .to_string()
    };
    let Some(shelf) = shelf_of(&raw) else {
        note("asked for a shelf that is not here");
        return std::ptr::null_mut();
    };
    remember_opening(&shelf);
    Box::into_raw(Box::new(RefCell::new(Opened {
        shelf,
        names: Vec::new(),
        view: Vec::new(),
        bytes: Vec::new(),
        error: CString::default(),
    }))) as IcFsHandle
}

/// Reached only if something tries to mount a shelf as a file of another
/// filesystem. A shelf is opened through the connection kind above, so there
/// is nothing to open inside.
extern "C" fn never_opened_inside_a_file(
    _source: IcFsSource,
    _path: *const c_char,
    _user_data: *mut c_void,
) -> IcFsHandle {
    std::ptr::null_mut()
}

extern "C" fn fs_close(handle: IcFsHandle) {
    if handle.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(handle as *mut RefCell<Opened>) });
}

fn wanted(path: *const c_char) -> String {
    if path.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(path) }
        .to_string_lossy()
        .trim_matches('/')
        .to_string()
}

extern "C" fn fs_list(handle: IcFsHandle, path: *const c_char) -> IcListing {
    if !wanted(path).is_empty() {
        return IcListing::EMPTY;
    }
    with_opened(handle, |o| {
        o.names = HOLDS
            .iter()
            .map(|held| CString::new(*held).unwrap_or_default())
            .collect();
        o.view = o
            .names
            .iter()
            .map(|name| IcDirEntry {
                name: name.as_ptr(),
                is_dir: 0,
                size: 0,
                modified: 0,
                permissions: 0,
                has_permissions: 0,
            })
            .collect();
        IcListing {
            items: o.view.as_ptr(),
            count: o.view.len() as u32,
        }
    })
    .unwrap_or(IcListing::EMPTY)
}

extern "C" fn fs_read(handle: IcFsHandle, path: *const c_char) -> IcBytes {
    let name = wanted(path);
    with_opened(handle, |o| {
        if !HOLDS.contains(&name.as_str()) {
            return IcBytes::EMPTY;
        }
        o.bytes = format!("{name}, from the {} shelf\n", o.shelf).into_bytes();
        IcBytes {
            data: o.bytes.as_ptr(),
            len: o.bytes.len() as u64,
        }
    })
    .unwrap_or(IcBytes::EMPTY)
}

extern "C" fn fs_read_only(_handle: IcFsHandle) -> c_int {
    1
}

extern "C" fn fs_last_error(handle: IcFsHandle) -> *const c_char {
    with_opened(handle, |o| {
        o.error = CString::new("there is nothing by that name on this shelf").unwrap_or_default();
        o.error.as_ptr()
    })
    .unwrap_or(std::ptr::null())
}

pub fn vtable() -> IcFsVTable {
    IcFsVTable {
        struct_size: std::mem::size_of::<IcFsVTable>() as u32,
        open_in: never_opened_inside_a_file,
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
        columns: None,
        list_rows: None,
        action_state: None,
        cell_clicked: None,
        set_permissions: None,
    }
}

// ------------------------------------------------------ the pinned entry

/// What the pinned entry is called: the shelves, and how often they were
/// opened between them once they were. Plain text, like a subtitle.
pub fn pinned_title(language: &str) -> String {
    let total: u64 = SHELVES.iter().map(|(key, _)| opened_so_far(key)).sum();
    let named = phrase(language, "hello_drive.shelves");
    match total {
        0 => named,
        many => format!("{named} · {many}"),
    }
}

thread_local! {
    static PINNED_HELD: RefCell<(Vec<CString>, Vec<IcPinnedConnection>)> =
        const { RefCell::new((Vec::new(), Vec::new())) };
}

/// Asked every time the connections list is drawn, on the frontend's thread.
extern "C" fn pinned(_user_data: *mut c_void) -> IcPinnedConnections {
    let text = vec![
        CString::new(PINNED).unwrap_or_default(),
        CString::new(pinned_title(language())).unwrap_or_default(),
        CString::new(ICON).unwrap_or_default(),
        CString::new(VIEW).unwrap_or_default(),
    ];
    let rows = vec![IcPinnedConnection {
        id: text[0].as_ptr(),
        title: text[1].as_ptr(),
        svg: text[2].as_ptr(),
        view: text[3].as_ptr(),
    }];
    PINNED_HELD.with(|held| {
        *held.borrow_mut() = (text, rows);
        let borrowed = held.borrow();
        IcPinnedConnections {
            count: borrowed.1.len() as u32,
            rows: borrowed.1.as_ptr(),
            ..IcPinnedConnections::EMPTY
        }
    })
}

/// The page the pinned entry opens: each shelf and how often it was opened.
pub fn page() -> serde_json::Value {
    let spoken = language();
    let rows: Vec<serde_json::Value> = SHELVES
        .iter()
        .map(|(key, name)| {
            serde_json::json!({ "shelf": name, "opened": how_often(spoken, opened_so_far(key)) })
        })
        .collect();
    serde_json::json!({
        "schema": 1,
        "kind": VIEW,
        "data": { "rows": rows },
        "form": { "t": "view", "padding": 16, "spacing": 8, "children": [
            { "t": "table", "id": "shelves", "rows_key": "rows",
              "columns": [
                  { "key": "shelf", "width": 160,
                    "title": { "tr": "hello_drive.title", "en": "Shelf" } },
                  { "key": "opened" } ] },
            { "t": "button", "id": "forget",
              "label": { "tr": "hello_drive.forget", "en": "Forget the counts" },
              "intent": { "do": "emit", "node": "forget" } }
        ] }
    })
}

thread_local! {
    static ANSWER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn answer_with(text: String) -> IcBytes {
    ANSWER.with(|held| {
        *held.borrow_mut() = text.into_bytes();
        let borrowed = held.borrow();
        IcBytes {
            data: borrowed.as_ptr(),
            len: borrowed.len() as u64,
        }
    })
}

extern "C" fn page_describe(_ctx: *const u8, _len: u64, _user_data: *mut c_void) -> IcBytes {
    answer_with(page().to_string())
}

/// What a press on the page does. `redescribe` asks the host for the page
/// again, so the table shows the counts as they are now.
pub fn reply_for(event: &serde_json::Value) -> serde_json::Value {
    if event["type"] == "activate" && event["node"] == "forget" {
        forget_openings();
        return serde_json::json!({ "redescribe": true });
    }
    serde_json::json!({})
}

extern "C" fn page_event(event: *const u8, len: u64, _user_data: *mut c_void) -> IcBytes {
    let raw = if event.is_null() || len == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(event, len as usize) }
    };
    let parsed: serde_json::Value = serde_json::from_slice(raw).unwrap_or_default();
    answer_with(reply_for(&parsed).to_string())
}

/// A frontend with a connections list to pin to. A null kind means the desktop.
fn keeps_a_connections_list(kind: *const c_char) -> bool {
    kind.is_null() || unsafe { CStr::from_ptr(kind) }.to_str() == Ok(IC_HOST_GTK)
}

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_init(host: *const IcHost, kind: *const c_char) -> c_int {
    match check_host(host, IC_ABI_VERSION, needs_pinned_connections()) {
        HostCheck::Ok => {}
        HostCheck::WrongMagic => return IC_ERR_HOST_UNKNOWN,
        HostCheck::TooOld { .. } | HostCheck::Truncated { .. } => return IC_ERR_HOST_TOO_OLD,
    }
    HOST.store(host as usize, Ordering::Relaxed);

    // Which language to put in a subtitle. Headings and tooltips travel as
    // keys and the host translates those; a subtitle is plain text.
    let spoken = unsafe { ((*host).language)() };
    if !spoken.is_null() {
        let _ = SPOKEN.set(
            unsafe { CStr::from_ptr(spoken) }
                .to_string_lossy()
                .to_string(),
        );
    }
    for (language, catalogue) in LOCALES {
        let Ok(tag) = CString::new(*language) else {
            continue;
        };
        unsafe {
            ((*host).register_locales)(tag.as_ptr(), catalogue.as_ptr(), catalogue.len() as u64)
        };
    }

    let (Ok(mounted_by), Ok(svg)) = (CString::new(KIND), CString::new(ICON)) else {
        return IC_ERR_INIT_FAILED;
    };
    // The host copies what these tables hold, so a local is enough — except
    // for the filesystem, whose address the connection table carries. That
    // one has to outlive the call, hence the leak of exactly one.
    let filesystem: &'static IcFsVTable = Box::leak(Box::new(vtable()));
    let connection = IcConnectionVTable {
        struct_size: std::mem::size_of::<IcConnectionVTable>() as u32,
        open: kind_open,
        fs: filesystem,
        describe: None,
        on_event: None,
    };
    // No document: nobody creates a shelf by hand, so the kind is never
    // offered in the connections dialog. It only mounts what the drives list
    // hands it.
    let declared = unsafe {
        ((*host).register_connection_kind)(
            mounted_by.as_ptr(),
            std::ptr::null(),
            0,
            &connection,
            std::ptr::null_mut(),
        )
    };
    if declared != IC_OK {
        return declared;
    }

    // The entries themselves, mounted through the kind just declared.
    let offered = unsafe {
        ((*host).register_drive_source)(
            mounted_by.as_ptr(),
            svg.as_ptr(),
            drives,
            std::ptr::null_mut(),
        )
    };
    if offered != IC_OK {
        return offered;
    }

    let (Ok(view), Ok(title), Ok(source)) = (
        CString::new(VIEW),
        CString::new("Hello Drive"),
        CString::new(ID),
    ) else {
        return IC_ERR_INIT_FAILED;
    };
    let page_table = IcViewVTable {
        struct_size: std::mem::size_of::<IcViewVTable>() as u32,
        describe: page_describe,
        on_event: Some(page_event),
        closed: None,
    };
    let registered = unsafe {
        ((*host).register_view)(
            view.as_ptr(),
            title.as_ptr(),
            &page_table,
            std::ptr::null_mut(),
        )
    };
    if registered != IC_OK {
        return registered;
    }

    // Everything above is drawn by every frontend. The connections list is
    // not: where there is none, the entry is left unregistered rather than
    // handed to nobody, and the page it would have opened stays reachable.
    if !keeps_a_connections_list(kind) {
        return IC_OK;
    }

    // The view has to be registered first: an entry whose view the host does
    // not know is left out of the list.
    unsafe { ((*host).register_pinned_connections)(source.as_ptr(), pinned, std::ptr::null_mut()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pick_in_the_list_names_the_shelf_it_stands_for() {
        assert_eq!(shelf_of(r#"{"shelf":"upper"}"#).as_deref(), Some("upper"));
        assert_eq!(shelf_of(r#"{"shelf":"lower"}"#).as_deref(), Some("lower"));
    }

    #[test]
    fn settings_that_name_nothing_here_are_refused() {
        // A kind is handed whatever was stored for it, which may be a record
        // from another version, or nothing at all.
        assert_eq!(shelf_of(r#"{"shelf":"attic"}"#), None);
        assert_eq!(shelf_of("{}"), None);
        assert_eq!(shelf_of("not json"), None);
        assert_eq!(shelf_of(""), None);
    }

    #[test]
    fn a_shelf_nobody_opened_says_so_rather_than_counting_to_nothing() {
        assert_eq!(how_often("en", 0), "never opened");
        assert_eq!(how_often("en", 1), "opened once");
        assert_eq!(how_often("en", 4), "opened 4 times");
    }

    #[test]
    fn the_count_is_put_where_the_language_puts_it() {
        assert_eq!(how_often("ru", 0), "ещё не открывали");
        assert_eq!(how_often("ru", 1), "открывали один раз");
        assert_eq!(how_often("ru", 7), "открывали 7 раз");
        // A language nobody shipped falls back to english rather than to the
        // key, which would be unreadable in a list.
        assert_eq!(how_often("xx", 2), "opened 2 times");
    }

    #[test]
    fn every_language_carries_every_word() {
        let english: std::collections::BTreeSet<&String> =
            catalogues().get("en").expect("english").keys().collect();
        assert_eq!(LOCALES.len(), 15);
        assert_eq!(catalogues().len(), 15);
        for (language, table) in catalogues() {
            assert_eq!(
                table.keys().collect::<std::collections::BTreeSet<_>>(),
                english,
                "`{language}` does not carry the same words"
            );
            assert!(table.values().all(|word| !word.trim().is_empty()));
        }
    }

    #[test]
    fn every_word_asked_for_is_one_the_plugin_ships() {
        // Nothing is looked up that was never written down; a missing one
        // would show as its own key in the list of drives.
        for key in [
            "hello_drive.never_opened",
            "hello_drive.opened_once",
            "hello_drive.opened_times",
            "hello_drive.title",
            "hello_drive.shelves",
            "hello_drive.forget",
        ] {
            assert_ne!(phrase("en", key), key, "`{key}` was never written down");
        }
    }

    #[test]
    fn the_pinned_entry_is_the_shelves_and_counts_their_openings() {
        assert_eq!(
            pinned_title("en"),
            "Shelves",
            "never opened, so nothing to count"
        );
        assert_eq!(pinned_title("ru"), "Полки");
    }

    #[test]
    fn forgetting_asks_for_the_page_again_and_nothing_else_does() {
        let forgot = reply_for(&serde_json::json!({ "type": "activate", "node": "forget" }));
        assert_eq!(forgot["redescribe"], serde_json::json!(true));
        assert_eq!(
            reply_for(&serde_json::json!({ "type": "activate", "node": "other" })),
            serde_json::json!({})
        );
    }

    #[test]
    fn the_plugin_says_what_it_is_before_it_is_initialised() {
        let name = unsafe { CStr::from_ptr(ic_plugin_name()) }.to_string_lossy();
        assert_eq!(name, "Hello Drive");
        assert!(!ic_plugin_about().is_null());
    }

    #[test]
    fn a_shelf_holds_what_it_says_it_holds() {
        let settings = serde_json::json!({ "shelf": "upper" }).to_string();
        let handle = kind_open(
            settings.as_ptr(),
            settings.len() as u64,
            std::ptr::null_mut(),
        );
        assert!(!handle.is_null());
        let root = CString::new("").expect("a path");
        let listing = fs_list(handle, root.as_ptr());
        let names: Vec<String> = listing
            .as_slice()
            .iter()
            .map(|entry| entry.name_string())
            .collect();
        assert_eq!(names, vec!["a jar".to_string(), "a lamp".to_string()]);

        let jar = CString::new("a jar").expect("a name");
        let read = fs_read(handle, jar.as_ptr());
        assert_eq!(
            String::from_utf8_lossy(read.as_slice()),
            "a jar, from the upper shelf\n"
        );
        fs_close(handle);
    }

    #[test]
    fn opening_a_shelf_that_is_not_here_hands_back_nothing() {
        let settings = serde_json::json!({ "shelf": "attic" }).to_string();
        let handle = kind_open(
            settings.as_ptr(),
            settings.len() as u64,
            std::ptr::null_mut(),
        );
        assert!(handle.is_null(), "there is no such shelf to open");
    }
}
