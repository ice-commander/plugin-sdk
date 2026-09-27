//! Reading a field, reacting to typing, and handling button presses.
//!
//! Every event carries `values`: what the user has typed, under the binds the
//! document declared. The reply writes back — `put` by node, `set` by namespace,
//! `clipboard`, `redescribe` to ask for the document again, and `close` to
//! dismiss the window. Events reach the window in every frontend; only the panel
//! toolbar button is desktop-only, so that is the one call `kind` guards.

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
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-events",
    "Hello Events",
    sdk_version!(),
    "SDK example: typing, buttons and what a plugin answers"
);

pub const VIEW_ID: &str = "sdk.hello.events";
const ICON: &str = include_str!("../../hello-toolbar/assets/hello.svg");

static HOST: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static ANSWER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

pub fn document() -> Value {
    json!({
        "schema": 1,
        "kind": "sdk.hello.events",
        "data": { "count": "nothing typed yet", "status": "" },
        "fields": [ { "bind": "text", "type": "text" } ],
        "form": {
            "t": "view",
            "surface": "dialog",
            "width": 460,
            "padding": 16,
            "spacing": 8,
            "children": [
                { "t": "text", "text": "Type, then press a button", "role": "title2" },
                { "t": "input", "id": "field", "bind": "text", "chrome": "row",
                  "title": "Text", "placeholder": "say something",
                  "emit": "change", "debounce_ms": 150 },
                { "t": "text", "id": "count", "role": "dim", "text": "{data.count}" },
                { "t": "row", "spacing": 8, "margin_top": 8, "children": [
                    { "t": "button", "id": "shout", "label": "SHOUT", "role": "primary",
                      "intent": { "do": "emit", "node": "shout" } },
                    { "t": "button", "id": "copy", "label": "Copy",
                      "intent": { "do": "emit", "node": "copy" } },
                    { "t": "button", "id": "clear", "label": "Clear", "role": "destructive",
                      "intent": { "do": "emit", "node": "clear" } }
                ]},
                { "t": "text", "id": "status", "role": "caption", "text": "{data.status}",
                  "visible": { "not": { "empty": "data.status" } } }
            ]
        }
    })
}

fn counted(text: &str) -> String {
    match text.chars().count() {
        0 => "nothing typed yet".to_string(),
        1 => "1 character".to_string(),
        many => format!("{many} characters"),
    }
}

pub fn reply_for(event: &Value) -> Value {
    let text = event["values"]["text"].as_str().unwrap_or_default();
    let kind = event["type"].as_str().unwrap_or_default();
    let node = event["node"].as_str().unwrap_or_default();

    match (kind, node) {
        ("change", _) => json!({ "set": { "data.count": counted(text), "data.status": "" } }),

        ("activate", "shout") => json!({
            "put": { "field": text.to_uppercase() },
            "set": { "data.count": counted(text), "data.status": "shouted" }
        }),

        ("activate", "clear") => json!({
            "put": { "field": "" },
            "set": { "data.count": counted(""), "data.status": "cleared" }
        }),

        ("activate", "copy") if !text.is_empty() => json!({
            "clipboard": text,
            "set": { "data.status": "copied to the clipboard" }
        }),
        ("activate", "copy") => json!({ "set": { "data.status": "nothing to copy" } }),

        ("opened", _) => json!({ "set": { "data.count": counted(text) } }),

        _ => json!({}),
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
    answer_with(&document().to_string())
}

extern "C" fn on_event(event: *const u8, len: u64, _user_data: *mut c_void) -> IcBytes {
    if event.is_null() || len == 0 {
        return answer_with("{}");
    }
    let raw = unsafe { std::slice::from_raw_parts(event, len as usize) };
    let parsed: Value = serde_json::from_slice(raw).unwrap_or(Value::Null);
    answer_with(&reply_for(&parsed).to_string())
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

    let (Ok(id), Ok(svg), Ok(tooltip)) = (
        CString::new(VIEW_ID),
        CString::new(ICON),
        CString::new("Events from a plugin window"),
    ) else {
        return IC_ERR_INIT_FAILED;
    };

    let table = IcViewVTable {
        struct_size: std::mem::size_of::<IcViewVTable>() as u32,
        describe,
        on_event: Some(on_event),
        closed: None,
    };
    let registered = unsafe {
        ((*host).register_view)(id.as_ptr(), tooltip.as_ptr(), &table, std::ptr::null_mut())
    };
    if registered != IC_OK {
        return registered;
    }

    // The window above is the offering and every frontend has it; the button
    // below would be taken and then never drawn anywhere but the desktop.
    if !draws_a_panel_toolbar(kind) {
        return IC_OK;
    }

    unsafe {
        ((*host).add_toolbar_button)(
            id.as_ptr(),
            svg.as_ptr(),
            tooltip.as_ptr(),
            IC_SIDE_RIGHT,
            90,
            IC_ENABLE_ALWAYS,
            on_clicked,
            std::ptr::null_mut(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(text: &str) -> Value {
        json!({ "type": "change", "node": "field", "bind": "text",
                "values": { "text": text } })
    }

    fn pressed(node: &str, text: &str) -> Value {
        json!({ "type": "activate", "node": node, "values": { "text": text } })
    }

    #[test]
    fn typing_is_answered_with_a_fresh_count() {
        assert_eq!(
            reply_for(&typed(""))["set"]["data.count"],
            json!("nothing typed yet")
        );
        assert_eq!(
            reply_for(&typed("a"))["set"]["data.count"],
            json!("1 character")
        );
        assert_eq!(
            reply_for(&typed("hello"))["set"]["data.count"],
            json!("5 characters")
        );
        assert_eq!(
            reply_for(&typed("日本語"))["set"]["data.count"],
            json!("3 characters"),
            "counted in characters, not bytes"
        );
    }

    #[test]
    fn shouting_writes_back_into_the_field_by_its_node_id() {
        let answer = reply_for(&pressed("shout", "hello"));
        assert_eq!(answer["put"]["field"], json!("HELLO"));
        assert_eq!(answer["set"]["data.status"], json!("shouted"));
        assert!(
            answer["put"].get("text").is_none(),
            "put is keyed by node id, values by bind: here `field` and `text`"
        );
    }

    #[test]
    fn clearing_empties_the_field_and_the_count_follows() {
        let answer = reply_for(&pressed("clear", "hello"));
        assert_eq!(answer["put"]["field"], json!(""));
        assert_eq!(answer["set"]["data.count"], json!("nothing typed yet"));
    }

    #[test]
    fn copying_asks_the_host_and_says_so_when_there_is_nothing() {
        let full = reply_for(&pressed("copy", "hello"));
        assert_eq!(full["clipboard"], json!("hello"));
        let empty = reply_for(&pressed("copy", ""));
        assert!(empty.get("clipboard").is_none());
        assert_eq!(empty["set"]["data.status"], json!("nothing to copy"));
    }

    #[test]
    fn an_event_the_plugin_does_not_know_changes_nothing() {
        assert_eq!(reply_for(&pressed("nosuchbutton", "hello")), json!({}));
        assert_eq!(reply_for(&json!(null)), json!({}));
    }
}
