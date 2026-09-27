//! A panel of the plugin's own: rows that are not files, and buttons that act
//! on the ones the user picked.
//!
//! The panel source is a function the host calls for a table each time it
//! lists the panel. The first column is the row's name: a row without one is
//! dropped, and no two rows may share one, because the host finds a picked row
//! by its name. `key_column` names the column whose text comes back in
//! `selection` when an action is pressed, so it holds what the plugin finds a
//! row by — here the task's number.
//!
//! Actions are registered against the source and shown only while a panel
//! stands in it. Nothing is listed again after one is pressed: an action that
//! changed the rows opens the source once more, and that is what makes the
//! panel ask. `close_panel_source` takes the panel back to where it was before
//! the source was first opened.
//!
//! Only the desktop application has panels a plugin can fill, so anywhere else
//! `init` declines with `IC_ERR_NOT_THIS_HOST` before registering anything.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, needs_up_to, HostCheck, IcClickFn, IcColumn, IcHost, IcSelection, IcSelectionItem,
    IcTable, IC_ABI_VERSION, IC_ENABLE_ALWAYS, IC_ENABLE_ON_FILE, IC_ERR_HOST_TOO_OLD,
    IC_ERR_HOST_UNKNOWN, IC_ERR_INIT_FAILED, IC_ERR_NOT_THIS_HOST, IC_HOST_GTK, IC_OK,
    IC_SIDE_RIGHT,
};
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-source",
    "Hello Source",
    sdk_version!(),
    "SDK example: a panel of tasks with buttons that act on the selected ones"
);

pub const SOURCE_ID: &str = "sdk.hello.source";

const COLUMNS: [(&str, &str, i32); 3] = [
    ("task", "Task", 280),
    ("number", "No.", 60),
    ("state", "State", 90),
];
const KEY_COLUMN: u32 = 1;

const LIST: &str = r#"<path d="M6 4h7M6 8h7M6 12h7"/><circle cx="3" cy="4" r=".8" fill="currentColor"/><circle cx="3" cy="8" r=".8" fill="currentColor"/><circle cx="3" cy="12" r=".8" fill="currentColor"/>"#;
const BACK: &str = r#"<path d="M10 3L5 8l5 5"/>"#;
const DONE: &str = r#"<path d="M3 8.5l3 3L13 5"/>"#;
const REMOVE: &str = r#"<path d="M4 4l8 8M12 4l-8 8"/>"#;

const ACTIONS: [(&str, &str, &str, u32, IcClickFn); 3] = [
    (
        "sdk.hello.source.back",
        BACK,
        "Back",
        IC_ENABLE_ALWAYS,
        on_back,
    ),
    (
        "sdk.hello.source.done",
        DONE,
        "Done or not done",
        IC_ENABLE_ON_FILE,
        on_done,
    ),
    (
        "sdk.hello.source.remove",
        REMOVE,
        "Remove",
        IC_ENABLE_ON_FILE,
        on_remove,
    ),
];

fn icon(drawn: &str) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round">{drawn}</svg>"#
    )
}

#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub number: u32,
    pub title: String,
    pub done: bool,
}

pub fn seeded() -> Vec<Task> {
    [
        "Read this example",
        "Pick a task and press Done",
        "Remove one with the cross",
        "Go back with the arrow",
    ]
    .iter()
    .zip(1..)
    .map(|(title, number)| Task {
        number,
        title: title.to_string(),
        done: number == 1,
    })
    .collect()
}

pub fn toggle_done(tasks: &mut [Task], picked: &[u32]) {
    for task in tasks
        .iter_mut()
        .filter(|task| picked.contains(&task.number))
    {
        task.done = !task.done;
    }
}

pub fn remove(tasks: &mut Vec<Task>, picked: &[u32]) {
    tasks.retain(|task| !picked.contains(&task.number));
}

pub fn cells_of(task: &Task) -> [String; 3] {
    [
        task.title.clone(),
        task.number.to_string(),
        if task.done { "done" } else { "to do" }.to_string(),
    ]
}

pub fn picked(selection: IcSelection) -> Vec<u32> {
    selection
        .as_slice()
        .iter()
        .filter_map(IcSelectionItem::key_string)
        .filter_map(|key| key.parse().ok())
        .collect()
}

struct Answer {
    texts: Vec<CString>,
    columns: Vec<IcColumn>,
    cells: Vec<*const c_char>,
}

static HOST: AtomicUsize = AtomicUsize::new(0);

fn host() -> *const IcHost {
    HOST.load(Ordering::Relaxed) as *const IcHost
}

thread_local! {
    static TASKS: RefCell<Vec<Task>> = RefCell::new(seeded());
    static ANSWER: RefCell<Answer> = const {
        RefCell::new(Answer { texts: Vec::new(), columns: Vec::new(), cells: Vec::new() })
    };
}

extern "C" fn rows(_user_data: *mut c_void) -> IcTable {
    let cells: Vec<String> = TASKS.with(|tasks| tasks.borrow().iter().flat_map(cells_of).collect());
    ANSWER.with(|slot| {
        let held = &mut *slot.borrow_mut();
        held.texts = COLUMNS
            .iter()
            .flat_map(|(key, title, _)| [key.to_string(), title.to_string()])
            .chain(cells)
            .map(|text| CString::new(text).unwrap_or_default())
            .collect();
        let (headings, body) = held.texts.split_at(COLUMNS.len() * 2);
        held.columns = COLUMNS
            .iter()
            .zip(headings.chunks(2))
            .map(|((_, _, width), pair)| IcColumn {
                key: pair[0].as_ptr(),
                title: pair[1].as_ptr(),
                width: *width,
            })
            .collect();
        held.cells = body.iter().map(|text| text.as_ptr()).collect();
        IcTable {
            columns: held.columns.as_ptr(),
            column_count: COLUMNS.len() as u32,
            cells: held.cells.as_ptr(),
            row_count: (body.len() / COLUMNS.len()) as u32,
            key_column: KEY_COLUMN,
        }
    })
}

fn open_the_source() {
    let host = host();
    if host.is_null() {
        return;
    }
    if let Ok(id) = CString::new(SOURCE_ID) {
        unsafe { ((*host).open_panel_source)(id.as_ptr()) };
    }
}

fn act_on_selection(change: impl FnOnce(&mut Vec<Task>, &[u32])) {
    let host = host();
    if host.is_null() {
        return;
    }
    let picked = picked(unsafe { ((*host).selection)() });
    if picked.is_empty() {
        return;
    }
    TASKS.with(|tasks| change(&mut tasks.borrow_mut(), &picked));
    open_the_source();
}

extern "C" fn on_open(_user_data: *mut c_void, _parent_window: *mut c_void) {
    open_the_source();
}

extern "C" fn on_back(_user_data: *mut c_void, _parent_window: *mut c_void) {
    let host = host();
    if !host.is_null() {
        unsafe { ((*host).close_panel_source)() };
    }
}

extern "C" fn on_done(_user_data: *mut c_void, _parent_window: *mut c_void) {
    act_on_selection(|tasks, picked| toggle_done(tasks, picked));
}

extern "C" fn on_remove(_user_data: *mut c_void, _parent_window: *mut c_void) {
    act_on_selection(remove);
}

/// A null kind is an application from before `init` was told which one it is:
/// the desktop.
fn draws_panel_sources(kind: *const c_char) -> bool {
    kind.is_null() || unsafe { CStr::from_ptr(kind) }.to_str() == Ok(IC_HOST_GTK)
}

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_init(host: *const IcHost, kind: *const c_char) -> c_int {
    match check_host(
        host,
        IC_ABI_VERSION,
        needs_up_to(std::mem::offset_of!(IcHost, register_panel_action)),
    ) {
        HostCheck::Ok => {}
        HostCheck::WrongMagic => return IC_ERR_HOST_UNKNOWN,
        HostCheck::TooOld { .. } | HostCheck::Truncated { .. } => return IC_ERR_HOST_TOO_OLD,
    }
    if !draws_panel_sources(kind) {
        return IC_ERR_NOT_THIS_HOST;
    }
    HOST.store(host as usize, Ordering::Relaxed);

    let (Ok(id), Ok(title), Ok(svg)) = (
        CString::new(SOURCE_ID),
        CString::new("Tasks"),
        CString::new(icon(LIST)),
    ) else {
        return IC_ERR_INIT_FAILED;
    };
    let registered = unsafe {
        ((*host).register_panel_source)(
            id.as_ptr(),
            title.as_ptr(),
            svg.as_ptr(),
            rows,
            std::ptr::null_mut(),
        )
    };
    if registered != IC_OK {
        return registered;
    }

    for (action, drawn, tooltip, enable_flags, on_click) in ACTIONS {
        let (Ok(action), Ok(picture), Ok(tooltip)) = (
            CString::new(action),
            CString::new(icon(drawn)),
            CString::new(tooltip),
        ) else {
            return IC_ERR_INIT_FAILED;
        };
        let registered = unsafe {
            ((*host).register_panel_action)(
                id.as_ptr(),
                action.as_ptr(),
                picture.as_ptr(),
                tooltip.as_ptr(),
                enable_flags,
                on_click,
                std::ptr::null_mut(),
            )
        };
        if registered != IC_OK {
            return registered;
        }
    }

    unsafe {
        ((*host).add_toolbar_button)(
            id.as_ptr(),
            svg.as_ptr(),
            title.as_ptr(),
            IC_SIDE_RIGHT,
            80,
            IC_ENABLE_ALWAYS,
            on_open,
            std::ptr::null_mut(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_three_columns_keyed_by_the_number() {
        let table = rows(std::ptr::null_mut());
        let keys: Vec<String> = table
            .columns_slice()
            .iter()
            .map(IcColumn::key_string)
            .collect();
        assert_eq!(keys, ["task", "number", "state"]);
        assert_eq!(table.row_count as usize, seeded().len());
        assert_eq!(table.cell(0, 0).as_deref(), Some("Read this example"));
        assert_eq!(table.cell(0, table.key_column).as_deref(), Some("1"));
        assert_eq!(table.cell(0, 2).as_deref(), Some("done"));
        assert_eq!(table.cell(1, 2).as_deref(), Some("to do"));
    }

    #[test]
    fn the_table_follows_the_list() {
        TASKS.with(|tasks| remove(&mut tasks.borrow_mut(), &[1, 3]));
        let table = rows(std::ptr::null_mut());
        let numbers: Vec<String> = (0..table.row_count)
            .filter_map(|row| table.cell(row, KEY_COLUMN))
            .collect();
        assert_eq!(numbers, ["2", "4"]);
    }

    #[test]
    fn the_selection_arrives_as_keys_and_only_numbers_count() {
        let texts: Vec<CString> = ["2", "4", "not a number"]
            .iter()
            .map(|key| CString::new(*key).expect("a key"))
            .collect();
        let mut items: Vec<IcSelectionItem> = texts
            .iter()
            .map(|key| IcSelectionItem {
                path: std::ptr::null(),
                key: key.as_ptr(),
                is_dir: 0,
            })
            .collect();
        items.push(IcSelectionItem {
            path: std::ptr::null(),
            key: std::ptr::null(),
            is_dir: 0,
        });
        let selection = IcSelection {
            items: items.as_ptr(),
            count: items.len() as u32,
        };
        assert_eq!(picked(selection), [2, 4]);
        assert!(picked(IcSelection::EMPTY).is_empty());
    }

    #[test]
    fn done_flips_only_the_picked_tasks() {
        let mut tasks = seeded();
        toggle_done(&mut tasks, &[1, 2]);
        let done: Vec<bool> = tasks.iter().map(|task| task.done).collect();
        assert_eq!(done, [false, true, false, false]);
    }

    #[test]
    fn remove_drops_the_picked_tasks_and_ignores_unknown_ones() {
        let mut tasks = seeded();
        remove(&mut tasks, &[2, 99]);
        let left: Vec<u32> = tasks.iter().map(|task| task.number).collect();
        assert_eq!(left, [1, 3, 4]);
    }

    #[test]
    fn neither_a_terminal_nor_a_browser_is_handed_the_panel() {
        let host = ic_plugin_api::testing::silent_host();
        for kind in [ic_plugin_api::IC_HOST_CONSOLE, ic_plugin_api::IC_HOST_WEB] {
            let kind = CString::new(kind).expect("a kind");
            assert_eq!(ic_plugin_init(&host, kind.as_ptr()), IC_ERR_NOT_THIS_HOST);
        }
        assert!(
            draws_panel_sources(std::ptr::null()),
            "an unnamed host is the desktop"
        );
    }

    thread_local! {
        static PICKED: RefCell<Vec<IcSelectionItem>> = const { RefCell::new(Vec::new()) };
        static ASKED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    extern "C" fn picked_in_the_panel() -> IcSelection {
        PICKED.with(|items| {
            let items = items.borrow();
            IcSelection {
                items: items.as_ptr(),
                count: items.len() as u32,
            }
        })
    }

    extern "C" fn opened(id: *const c_char) -> c_int {
        let id = unsafe { CStr::from_ptr(id) }.to_string_lossy();
        ASKED.with(|asked| asked.borrow_mut().push(format!("open {id}")));
        IC_OK
    }

    extern "C" fn closed() -> c_int {
        ASKED.with(|asked| asked.borrow_mut().push("close".to_string()));
        IC_OK
    }

    #[test]
    fn a_button_changes_the_picked_tasks_and_has_the_panel_list_again() {
        let mut host = ic_plugin_api::testing::silent_host();
        host.selection = picked_in_the_panel;
        host.open_panel_source = opened;
        host.close_panel_source = closed;
        let host: &'static IcHost = Box::leak(Box::new(host));
        let gtk = CString::new(IC_HOST_GTK).expect("a kind");
        assert_eq!(ic_plugin_init(host, gtk.as_ptr()), IC_OK);

        let nothing = std::ptr::null_mut();
        let asked = || ASKED.with(|asked| asked.take());
        let reopened = [format!("open {SOURCE_ID}")];

        on_open(nothing, nothing);
        assert_eq!(asked(), reopened, "the toolbar button");

        on_done(nothing, nothing);
        on_remove(nothing, nothing);
        assert!(asked().is_empty(), "nothing picked, nothing to list again");
        assert_eq!(TASKS.with(|tasks| tasks.borrow().clone()), seeded());

        PICKED.with(|items| {
            items.borrow_mut().push(IcSelectionItem {
                path: std::ptr::null(),
                key: c"2".as_ptr(),
                is_dir: 0,
            })
        });
        on_done(nothing, nothing);
        assert!(TASKS.with(|tasks| tasks.borrow()[1].done));
        assert_eq!(asked(), reopened);

        on_remove(nothing, nothing);
        let left: Vec<u32> = TASKS.with(|tasks| tasks.borrow().iter().map(|t| t.number).collect());
        assert_eq!(left, [1, 3, 4]);
        assert_eq!(asked(), reopened);

        on_back(nothing, nothing);
        assert_eq!(asked(), ["close"]);
    }
}
