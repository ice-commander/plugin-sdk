//! One file, shown three ways: the same plugin in the desktop application, in a
//! browser and in a terminal.
//!
//! Which application it is arrives twice: as `kind` at `init`, and again in the
//! context of every `describe` as `host.kind`. The first decides what to
//! register, the second what to draw. A plugin whose whole offering needs pixels
//! should instead return `IC_ERR_NOT_THIS_HOST` from `init`, and the application
//! will not ask it anything else.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, needs_up_to, HostCheck, IcBytes, IcFsSource, IcHost, IcViewVTable, IcViewerVTable,
    IC_ABI_VERSION, IC_ERR_HOST_TOO_OLD, IC_ERR_HOST_UNKNOWN, IC_ERR_INIT_FAILED, IC_HOST_CONSOLE,
    IC_OK, IC_OPEN_READ, IC_SEEK_END,
};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-view",
    "Hello View",
    sdk_version!(),
    "SDK example: one file shown three ways, by the application it is shown in"
);

pub const ID: &str = "hello-view";
pub const EXTENSIONS: &str = ".test";

/// A picture the plugin makes rather than reads, so that both frontends with
/// pixels have something to draw.
const MARK: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 48 48">
<rect x="4" y="4" width="40" height="40" rx="6" fill="#3584e4"/>
<path d="M14 24l7 7 13-14" stroke="#fff" stroke-width="4" fill="none"/></svg>"##;

static HOST: AtomicUsize = AtomicUsize::new(0);

fn host() -> *const IcHost {
    HOST.load(Ordering::Relaxed) as *const IcHost
}

struct Showing {
    name: String,
    size: u64,
    head: String,
}

thread_local! {
    static SHOWING: RefCell<BTreeMap<u64, Showing>> = const { RefCell::new(BTreeMap::new()) };
    static DRAWN: RefCell<String> = const { RefCell::new(String::new()) };
}

fn looked_at(source: IcFsSource, path: &CStr) -> (u64, String) {
    let host = host();
    if host.is_null() {
        return (0, String::new());
    }
    let stream = unsafe { ((*host).fs_open)(source, path.as_ptr(), IC_OPEN_READ) };
    if stream.is_null() {
        return (0, String::new());
    }
    let size = unsafe { ((*host).fs_seek)(stream, 0, IC_SEEK_END) }.max(0) as u64;
    unsafe { ((*host).fs_seek)(stream, 0, ic_plugin_api::IC_SEEK_SET) };
    let mut buffer = [0u8; 256];
    let read = unsafe { ((*host).fs_read)(stream, buffer.as_mut_ptr(), buffer.len() as u64) };
    unsafe { ((*host).fs_close)(stream) };
    let head = String::from_utf8_lossy(&buffer[..read.max(0) as usize])
        .lines()
        .next()
        .unwrap_or_default()
        .to_string();
    (size, head)
}

extern "C" fn viewer_open(
    instance: u64,
    source: IcFsSource,
    path: *const c_char,
    _user_data: *mut c_void,
) -> c_int {
    if path.is_null() {
        return IC_ERR_INIT_FAILED;
    }
    let held = unsafe { CStr::from_ptr(path) }.to_owned();
    let (size, head) = looked_at(source, &held);
    SHOWING.with(|open| {
        open.borrow_mut().insert(
            instance,
            Showing {
                name: held.to_string_lossy().into_owned(),
                size,
                head,
            },
        )
    });
    IC_OK
}

extern "C" fn viewer_closed(instance: u64, _user_data: *mut c_void) {
    SHOWING.with(|open| open.borrow_mut().remove(&instance));
}

fn asked_by(ctx: *const u8, len: u64) -> (String, u64) {
    if ctx.is_null() || len == 0 {
        return (String::new(), 0);
    }
    let held = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(ctx, len as usize) });
    let context: Value = serde_json::from_str(&held).unwrap_or(Value::Null);
    (
        context["host"]["kind"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        context["instance"].as_u64().unwrap_or(0),
    )
}

/// A terminal is given the same facts in words, everything else the picture as
/// well: what differs between frontends is how it is put, not what is said.
pub fn document_for(kind: &str, name: &str, size: u64, head: &str) -> String {
    let mut shown: Vec<Value> = Vec::new();
    if kind != IC_HOST_CONSOLE {
        shown.push(json!({
            "t": "image", "id": "mark", "src": "part:mark", "fit": "contain", "height": 64
        }));
    }
    shown.push(json!({
        "t": "text", "id": "name", "role": "title1", "wrap": true,
        "text": { "literal": name }
    }));
    shown.push(json!({
        "t": "text", "id": "facts", "role": "dim",
        "text": { "literal": format!("{size} bytes, shown by {kind}") }
    }));
    shown.push(json!({
        "t": "text", "id": "head", "role": "mono", "wrap": true,
        "text": { "literal": head }
    }));

    json!({
        "schema": 1,
        "fields": [],
        "form": {
            "t": "view", "surface": "window", "spacing": 8, "padding": 12,
            "children": shown
        }
    })
    .to_string()
}

extern "C" fn viewer_describe(ctx: *const u8, len: u64, _user_data: *mut c_void) -> IcBytes {
    let (kind, instance) = asked_by(ctx, len);
    let drawn = SHOWING.with(|open| {
        let open = open.borrow();
        let showing = open.get(&instance)?;
        Some(document_for(
            &kind,
            &showing.name,
            showing.size,
            &showing.head,
        ))
    });
    let Some(drawn) = drawn else {
        return IcBytes::EMPTY;
    };
    DRAWN.with(|held| {
        let mut held = held.borrow_mut();
        *held = drawn;
        IcBytes {
            data: held.as_ptr(),
            len: held.len() as u64,
        }
    })
}

extern "C" fn viewer_content(
    _instance: u64,
    name: *const c_char,
    _user_data: *mut c_void,
) -> IcBytes {
    if name.is_null() || unsafe { CStr::from_ptr(name) }.to_bytes() != b"mark" {
        return IcBytes::EMPTY;
    }
    IcBytes {
        data: MARK.as_ptr(),
        len: MARK.len() as u64,
    }
}

pub fn view_vtable() -> IcViewVTable {
    IcViewVTable {
        struct_size: std::mem::size_of::<IcViewVTable>() as u32,
        describe: viewer_describe,
        on_event: None,
        closed: None,
    }
}

pub fn viewer_vtable(view: *const IcViewVTable) -> IcViewerVTable {
    IcViewerVTable {
        struct_size: std::mem::size_of::<IcViewerVTable>() as u32,
        view,
        open: viewer_open,
        closed: Some(viewer_closed),
        content: Some(viewer_content),
        closing: None,
        // This viewer hands over bytes the host draws; nothing here paints.
        canvas_ready: None,
        canvas_draw: None,
        canvas_gone: None,
    }
}

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_init(host: *const IcHost, _kind: *const c_char) -> c_int {
    match check_host(
        host,
        IC_ABI_VERSION,
        needs_up_to(std::mem::offset_of!(IcHost, register_viewer)),
    ) {
        HostCheck::Ok => {}
        HostCheck::WrongMagic => return IC_ERR_HOST_UNKNOWN,
        HostCheck::TooOld { .. } | HostCheck::Truncated { .. } => return IC_ERR_HOST_TOO_OLD,
    }
    HOST.store(host as usize, Ordering::Relaxed);
    let (Ok(id), Ok(extensions)) = (CString::new(ID), CString::new(EXTENSIONS)) else {
        return IC_ERR_INIT_FAILED;
    };
    let window = view_vtable();
    let viewer = viewer_vtable(&window);
    unsafe {
        ((*host).register_viewer)(
            id.as_ptr(),
            extensions.as_ptr(),
            0,
            &viewer,
            std::ptr::null_mut(),
        )
    }
}

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_shutdown() {
    SHOWING.with(|open| open.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The picture is the plugin's own, handed over when the host draws it.
    #[test]
    fn the_mark_is_handed_over_by_name() {
        let asked = CString::new("mark").expect("a name");
        let answered = viewer_content(1, asked.as_ptr(), std::ptr::null_mut());
        assert_eq!(
            unsafe { std::slice::from_raw_parts(answered.data, answered.len as usize) },
            MARK
        );
        let other = CString::new("something-else").expect("a name");
        assert!(viewer_content(1, other.as_ptr(), std::ptr::null_mut())
            .data
            .is_null());
    }

    #[test]
    fn the_context_says_which_application_and_which_window() {
        let context = r#"{"host":{"kind":"web"},"instance":4}"#;
        let (kind, instance) = asked_by(context.as_ptr(), context.len() as u64);
        assert_eq!(kind, "web");
        assert_eq!(instance, 4);

        let (kind, instance) = asked_by(std::ptr::null(), 0);
        assert!(kind.is_empty());
        assert_eq!(instance, 0);
    }
}
