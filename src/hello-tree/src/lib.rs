//! A window with an interface of its own: a tree, a table that follows it, keys
//! with no button behind them, and a branch that changes by itself.
//!
//! The tree reads its rows from `data`. A closed branch comes without children
//! and says `"expandable": true`; opening it arrives as `expand` with that row in
//! `value` — `values` still holds the selection, which may be another row — and
//! the plugin answers by setting the rows again with the children in.
//!
//! `keys` binds a key to a node drawn nowhere: Delete and F5 reach the plugin as
//! `activate` of `delete` and `refresh`, carrying the selection like any event.
//!
//! Deliveries arrive while nobody touches the window. From `opened` to `closed`
//! a thread of the plugin's own adds a parcel every few seconds and calls
//! `view_invalidate`, which any thread may call; the host then asks `describe`
//! again, so the document carries the state as it is now. Never from inside an
//! event, though: there the reply's `set` says what changed, and
//! `view_invalidate` would re-enter the desktop while it is still delivering
//! that event.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, HostCheck, IcBytes, IcHost, IcViewVTable, IC_ABI_VERSION, IC_ENABLE_ALWAYS,
    IC_ERR_HOST_TOO_OLD, IC_ERR_HOST_UNKNOWN, IC_ERR_INIT_FAILED, IC_HOST_GTK, IC_OK,
    IC_SIDE_RIGHT,
};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-tree",
    "Hello Tree",
    sdk_version!(),
    "SDK example: a tree, keys with no button, and a branch that changes by itself"
);

pub const VIEW_ID: &str = "sdk.hello.tree";
const ICON: &str = include_str!("../../hello-toolbar/assets/hello.svg");
const LIVE: &str = "Deliveries";
const KEPT: usize = 5;
const EVERY: Duration = Duration::from_secs(3);
const HINT: &str = "Delete removes the selected branch, F5 reads everything again";

const SEED: [(&str, &[(&str, &str)]); 8] = [
    ("Kitchen", &[]),
    ("Kitchen/Pantry", &[]),
    ("Kitchen/Pantry/Jars", &[("honey", "1"), ("jam", "2")]),
    ("Kitchen/Pantry/Tins", &[("beans", "3"), ("soup", "2")]),
    ("Kitchen/Fridge", &[("milk", "1 l"), ("eggs", "6")]),
    ("Garden", &[]),
    ("Garden/Shed", &[("spades", "1"), ("rakes", "2")]),
    (LIVE, &[("arrived", "0")]),
];

static HOST: AtomicUsize = AtomicUsize::new(0);
static FEEDER: Mutex<Option<(Sender<()>, JoinHandle<()>)>> = Mutex::new(None);

thread_local! {
    static ANSWER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub path: String,
    pub values: Vec<(String, String)>,
}

fn item(path: &str, values: &[(&str, &str)]) -> Item {
    let values = values.iter().map(|(n, v)| (n.to_string(), v.to_string()));
    Item {
        path: path.to_string(),
        values: values.collect(),
    }
}

#[derive(Clone, Default)]
pub struct Place {
    pub items: Vec<Item>,
    pub opened: BTreeSet<String>,
    pub chosen: Option<String>,
    pub status: String,
    pub arrived: u32,
}

pub fn seeded() -> Place {
    Place {
        items: SEED.map(|(path, values)| item(path, values)).into(),
        status: HINT.to_string(),
        ..Place::default()
    }
}

fn place() -> MutexGuard<'static, Place> {
    static HELD: OnceLock<Mutex<Place>> = OnceLock::new();
    HELD.get_or_init(|| Mutex::new(seeded()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn within(path: &str, root: &str) -> bool {
    path.strip_prefix(root)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn kids<'a>(items: &'a [Item], under: &'a str) -> impl Iterator<Item = &'a Item> + 'a {
    items
        .iter()
        .filter(move |item| item.path.rsplit_once('/').map_or("", |(up, _)| up) == under)
}

fn rows(at: &Place, under: &str) -> Value {
    kids(&at.items, under)
        .map(|item| {
            let opens = kids(&at.items, &item.path).next().is_some();
            let open = opens && at.opened.contains(&item.path);
            json!({
                "name": item.path.rsplit('/').next().unwrap_or_default(),
                "path": item.path,
                "expandable": opens,
                "expanded": open,
                "children": if open { rows(at, &item.path) } else { json!([]) },
            })
        })
        .collect()
}

fn details(at: &Place) -> Value {
    let chosen = |item: &&Item| at.chosen.as_ref() == Some(&item.path);
    let values = at.items.iter().filter(chosen).flat_map(|item| &item.values);
    let row = |(name, value): &(String, String)| json!({ "name": name, "value": value });
    values.map(row).collect()
}

fn showing(at: &Place) -> Value {
    json!({ "set": {
        "data.tree": rows(at, ""),
        "data.details": details(at),
        "data.status": at.status,
    }})
}

/// The whole state rather than the first one: `view_invalidate` asks for it again.
pub fn document_of(at: &Place) -> Value {
    json!({
        "schema": 1,
        "kind": "sdk.hello.tree",
        "data": { "tree": rows(at, ""), "details": details(at), "status": at.status },
        "fields": [ { "bind": "chosen", "type": "text" } ],
        "keys": [
            { "accel": "Delete", "node": "delete" },
            { "accel": "F5", "node": "refresh" }
        ],
        "form": {
            "t": "view",
            "surface": "window",
            "width": 560,
            "height": 380,
            "padding": 0,
            "spacing": 0,
            "children": [
                { "t": "row", "weight": 1, "spacing": 0, "children": [
                    { "t": "column", "weight": 1, "padding": 8, "children": [
                        { "t": "tree", "id": "tree", "bind": "chosen", "rows_key": "tree",
                          "emit": "change", "weight": 1,
                          "columns": [ { "key": "name", "title": "Where" } ] }
                    ]},
                    { "t": "separator" },
                    { "t": "column", "weight": 1, "padding": 8, "children": [
                        { "t": "table", "id": "details", "rows_key": "details", "weight": 1,
                          "columns": [
                              { "key": "name", "title": "Name", "width": 120 },
                              { "key": "value", "title": "Value" } ] }
                    ]}
                ]},
                { "t": "separator" },
                { "t": "text", "id": "status", "role": "dim", "padding": 6,
                  "text": "{data.status}" }
            ]
        }
    })
}

pub fn reply_for(event: &Value, at: &Place) -> (Value, Place) {
    let kind = event["type"].as_str().unwrap_or_default();
    let node = event["node"].as_str().unwrap_or_default();
    let picked = event["values"]["chosen"]["path"].as_str();
    let arrow = event["value"]["path"].as_str();
    let mut next = at.clone();

    match (kind, node, picked, arrow) {
        ("change", "tree", picked, _) => next.chosen = picked.map(str::to_string),
        ("expand", "tree", _, Some(path)) => {
            next.opened.insert(path.to_string());
        }
        ("collapse", "tree", _, Some(path)) => next.opened.retain(|held| !within(held, path)),
        ("activate", "delete", Some(path), _) => {
            next.items.retain(|item| !within(&item.path, path));
            next.opened.retain(|held| !within(held, path));
            next.chosen = None;
            next.status = format!("Deleted {path}");
        }
        ("activate", "delete", None, _) => next.status = "Nothing is selected".to_string(),
        ("activate", "refresh", _, _) => next.status = HINT.to_string(),
        _ => return (json!({}), next),
    }
    (showing(&next), next)
}

/// One tick of the thread, kept apart so a test can make it happen.
pub fn delivered(at: &Place) -> Place {
    let mut next = at.clone();
    next.arrived += 1;
    let count = next.arrived.to_string();
    next.items.retain(|item| item.path != LIVE);
    next.items.push(item(LIVE, &[("arrived", &count)]));
    let parcel = format!("{LIVE}/Parcel {count}");
    next.items.push(item(&parcel, &[("number", &count)]));
    let parcels: Vec<String> = kids(&next.items, LIVE).map(|i| i.path.clone()).collect();
    let gone = &parcels[..parcels.len().saturating_sub(KEPT)];
    next.items.retain(|item| !gone.contains(&item.path));
    next
}

fn feed() {
    // Let go first: describe takes the same lock, and a host may call it before returning.
    {
        let mut held = place();
        *held = delivered(&held);
    }
    let host = HOST.load(Ordering::Relaxed) as *const IcHost;
    if host.is_null() {
        return;
    }
    if let Ok(id) = CString::new(VIEW_ID) {
        unsafe { ((*host).view_invalidate)(id.as_ptr()) };
    }
}

fn start_feeding() {
    let mut feeding = FEEDER.lock().unwrap_or_else(|held| held.into_inner());
    if feeding.is_some() {
        return;
    }
    let (stop, stopped) = mpsc::channel::<()>();
    // Nothing is ever sent: dropping the sender is what stops it, at once.
    let worker = std::thread::spawn(move || {
        while stopped.recv_timeout(EVERY) == Err(RecvTimeoutError::Timeout) {
            feed();
        }
    });
    *feeding = Some((stop, worker));
}

fn stop_feeding() {
    let taken = FEEDER
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .take();
    if let Some((stop, worker)) = taken {
        drop(stop);
        let _ = worker.join();
    }
}

fn answer_with(source: &str) -> IcBytes {
    ANSWER.with(|slot| {
        *slot.borrow_mut() = source.as_bytes().to_vec();
        let held = slot.borrow();
        IcBytes {
            data: held.as_ptr(),
            len: held.len() as u64,
        }
    })
}

extern "C" fn describe(_ctx: *const u8, _ctx_len: u64, _user_data: *mut c_void) -> IcBytes {
    answer_with(&document_of(&place()).to_string())
}

extern "C" fn on_event(event: *const u8, len: u64, _user_data: *mut c_void) -> IcBytes {
    if event.is_null() || len == 0 {
        return answer_with("{}");
    }
    let raw = unsafe { std::slice::from_raw_parts(event, len as usize) };
    let parsed: Value = serde_json::from_slice(raw).unwrap_or(Value::Null);
    if parsed["type"] == "opened" {
        start_feeding();
    }
    let reply = {
        let mut held = place();
        let (reply, next) = reply_for(&parsed, &held);
        *held = next;
        reply
    };
    answer_with(&reply.to_string())
}

extern "C" fn closed(_instance: u64, _user_data: *mut c_void) {
    stop_feeding();
}

extern "C" fn on_clicked(_user_data: *mut c_void, _parent_window: *mut c_void) {
    let host = HOST.load(Ordering::Relaxed) as *const IcHost;
    if host.is_null() {
        return;
    }
    let Ok(id) = CString::new(VIEW_ID) else {
        return;
    };
    unsafe {
        ((*host).open_view)(id.as_ptr(), std::ptr::null(), 0);
    }
}

/// The one frontend that draws a plugin's panel toolbar button. A null kind
/// is an application from before `init` was told which one it is: the desktop.
fn draws_a_panel_toolbar(kind: *const c_char) -> bool {
    kind.is_null() || unsafe { CStr::from_ptr(kind) }.to_str() == Ok(IC_HOST_GTK)
}

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_init(host: *const IcHost, kind: *const c_char) -> c_int {
    match check_host(host, IC_ABI_VERSION, ic_plugin_api::needs_views()) {
        HostCheck::Ok => {}
        HostCheck::WrongMagic => return IC_ERR_HOST_UNKNOWN,
        HostCheck::TooOld { .. } | HostCheck::Truncated { .. } => return IC_ERR_HOST_TOO_OLD,
    }
    HOST.store(host as usize, Ordering::Relaxed);

    let (Ok(id), Ok(svg), Ok(title)) = (
        CString::new(VIEW_ID),
        CString::new(ICON),
        CString::new("A tree of the plugin's own"),
    ) else {
        return IC_ERR_INIT_FAILED;
    };

    let table = IcViewVTable {
        struct_size: std::mem::size_of::<IcViewVTable>() as u32,
        describe,
        on_event: Some(on_event),
        closed: Some(closed),
    };
    let registered = unsafe {
        ((*host).register_view)(id.as_ptr(), title.as_ptr(), &table, std::ptr::null_mut())
    };
    if registered != IC_OK {
        return registered;
    }

    if !draws_a_panel_toolbar(kind) {
        return IC_OK;
    }

    unsafe {
        ((*host).add_toolbar_button)(
            id.as_ptr(),
            svg.as_ptr(),
            title.as_ptr(),
            IC_SIDE_RIGHT,
            80,
            IC_ENABLE_ALWAYS,
            on_clicked,
            std::ptr::null_mut(),
        )
    }
}

/// Joined rather than left running: the library is unloaded once this returns.
#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_shutdown() {
    stop_feeding();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, node: &str, chosen: Option<&str>, arrow: &str) -> Value {
        json!({ "type": kind, "node": node, "value": { "path": arrow },
                "values": { "chosen": chosen.map(|path| json!({ "path": path })) } })
    }

    fn arrow(kind: &str, path: &str) -> Value {
        event(kind, "tree", Some("Garden"), path)
    }

    fn names(rows: &Value) -> Vec<&str> {
        let rows = rows.as_array().expect("rows");
        rows.iter().filter_map(|row| row["name"].as_str()).collect()
    }

    #[test]
    fn a_closed_branch_holds_nothing_until_its_own_arrow_opens_it() {
        let top = rows(&seeded(), "");
        assert_eq!(names(&top), ["Kitchen", "Garden", "Deliveries"]);
        assert_eq!(top[0]["children"], json!([]));
        assert_eq!(top[2]["expandable"], json!(false));

        let (reply, at) = reply_for(&arrow("expand", "Kitchen"), &seeded());
        let kitchen = &reply["set"]["data.tree"][0]["children"];
        assert_eq!(names(kitchen), ["Pantry", "Fridge"]);
        assert_eq!(Vec::from_iter(at.opened), ["Kitchen"]);
    }

    #[test]
    fn closing_a_branch_shuts_what_was_open_inside_it() {
        let mut at = seeded();
        for path in ["Kitchen", "Kitchen/Pantry", "Garden"] {
            at = reply_for(&arrow("expand", path), &at).1;
        }
        let (_, shut) = reply_for(&arrow("collapse", "Kitchen"), &at);
        assert_eq!(Vec::from_iter(shut.opened), ["Garden"]);
        assert!(!within("Kitchen/Pantry shelf", "Kitchen/Pantry"));
    }

    #[test]
    fn the_table_follows_the_selection() {
        let picked = event("change", "tree", Some("Kitchen/Fridge"), "");
        let (reply, _) = reply_for(&picked, &seeded());
        let rows = &reply["set"]["data.details"];
        assert_eq!(names(rows), ["milk", "eggs"]);
        assert_eq!(rows[0]["value"], json!("1 l"));
    }

    #[test]
    fn delete_is_a_key_and_takes_the_branch_with_everything_under_it() {
        let keys = document_of(&seeded())["keys"].clone();
        assert_eq!(keys[0], json!({ "accel": "Delete", "node": "delete" }));

        let open = reply_for(&arrow("expand", "Kitchen/Pantry"), &seeded()).1;
        let pressed = event("activate", "delete", Some("Kitchen/Pantry"), "");
        let (reply, at) = reply_for(&pressed, &open);
        assert_eq!(names(&rows(&at, "Kitchen")), ["Fridge"]);
        assert_eq!(at.items.len(), SEED.len() - 3);
        assert!(at.opened.is_empty());
        assert_eq!(reply["set"]["data.status"], json!("Deleted Kitchen/Pantry"));

        let nothing = event("activate", "delete", None, "");
        let (_, same) = reply_for(&nothing, &seeded());
        assert_eq!(same.items, seeded().items);
        assert_eq!(same.status, "Nothing is selected");
    }

    #[test]
    fn deliveries_keep_the_latest_parcels_and_outlive_being_deleted() {
        let mut at = seeded();
        for _ in 0..KEPT + 2 {
            at = delivered(&at);
        }
        let parcels = rows(&at, LIVE);
        assert_eq!(names(&parcels)[0], "Parcel 3");
        assert_eq!(parcels.as_array().map(Vec::len), Some(KEPT));

        let gone = event("activate", "delete", Some(LIVE), "");
        let (_, at) = reply_for(&gone, &at);
        assert_eq!(names(&rows(&at, "")), ["Kitchen", "Garden"]);
        assert_eq!(names(&rows(&delivered(&at), ""))[2], LIVE);
    }

    #[test]
    fn deliveries_run_from_opened_until_closed() {
        let feeding = || FEEDER.lock().map(|held| held.is_some()).unwrap_or(false);
        let opened = br#"{"type":"opened"}"#;
        on_event(opened.as_ptr(), opened.len() as u64, std::ptr::null_mut());
        assert!(feeding());
        let asked = std::time::Instant::now();
        closed(1, std::ptr::null_mut());
        assert!(!feeding() && asked.elapsed() < EVERY);
    }
}
