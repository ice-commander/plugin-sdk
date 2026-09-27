//! A button on the panel toolbar that opens a window described in JSON.
//!
//! A panel toolbar button must be registered as `IC_SIDE_RIGHT`: the desktop
//! application reads only that side for the panel, and one on the left is taken
//! and then never shown. It is also the only frontend that draws a panel
//! toolbar, so the button is asked for there and nowhere else — while the asset
//! and the window are registered whatever the frontend is.

// Called from C with raw pointers: clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, HostCheck, IcBytes, IcHost, IcViewVTable, IC_ABI_VERSION, IC_ENABLE_ALWAYS,
    IC_ERR_HOST_TOO_OLD, IC_ERR_HOST_UNKNOWN, IC_ERR_INIT_FAILED, IC_HOST_GTK, IC_OK,
    IC_SIDE_RIGHT,
};
use serde_json::json;
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-toolbar",
    "Hello Toolbar",
    sdk_version!(),
    "SDK example: a toolbar button opening a declarative window"
);

pub const ID: &str = "sdk-hello-toolbar";
pub const VIEW_ID: &str = "sdk.hello.toolbar";
const ICON: &str = include_str!("../assets/hello.svg");

static HOST: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static ANSWER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

pub fn document() -> serde_json::Value {
    json!({
        "schema": 1,
        "kind": "sdk.hello",
        "form": {
            "t": "view",
            "surface": "dialog",
            "width": 420,
            "padding": 16,
            "spacing": 8,
            "children": [
                { "t": "row", "spacing": 8, "children": [
                    { "t": "icon", "icon": "asset:sdk-hello-toolbar/sdk-hello", "width": 24 },
                    { "t": "text", "text": "Hello from a plugin", "role": "title2" },
                ]},
                { "t": "separator" },
                { "t": "text", "wrap": true, "role": "dim",
                  "text": "This window is a JSON document. The plugin sent it; the frontend you are looking at drew it." },
                { "t": "text", "margin_top": 8,
                  "text": "Running on: {host.kind}, language {host.locale}" },
            ]
        }
    })
}

extern "C" fn describe(_ctx: *const u8, _ctx_len: u64, _user_data: *mut c_void) -> IcBytes {
    ANSWER.with(|slot| {
        *slot.borrow_mut() = document().to_string().into_bytes();
        let held = slot.borrow();
        IcBytes {
            data: held.as_ptr(),
            len: held.len() as u64,
        }
    })
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

fn draws_a_panel_toolbar(kind: *const c_char) -> bool {
    kind.is_null() || unsafe { CStr::from_ptr(kind) }.to_str() == Ok(IC_HOST_GTK)
}

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_init(host: *const IcHost, kind: *const c_char) -> c_int {
    match check_host(host, IC_ABI_VERSION, ic_plugin_api::needs_plugin_assets()) {
        HostCheck::Ok => {}
        HostCheck::WrongMagic => return IC_ERR_HOST_UNKNOWN,
        HostCheck::TooOld { .. } | HostCheck::Truncated { .. } => return IC_ERR_HOST_TOO_OLD,
    }
    HOST.store(host as usize, Ordering::Relaxed);

    let (Ok(id), Ok(asset), Ok(svg), Ok(tooltip)) = (
        CString::new(VIEW_ID),
        CString::new("sdk-hello"),
        CString::new(ICON),
        CString::new("Hello from a plugin"),
    ) else {
        return IC_ERR_INIT_FAILED;
    };

    let Ok(owner) = CString::new(ID) else {
        return IC_ERR_INIT_FAILED;
    };
    unsafe {
        ((*host).register_plugin_asset)(
            owner.as_ptr(),
            asset.as_ptr(),
            ICON.as_ptr(),
            ICON.len() as u64,
        );
    }

    let table = IcViewVTable {
        struct_size: std::mem::size_of::<IcViewVTable>() as u32,
        describe,

        on_event: None,
        closed: None,
    };
    let registered = unsafe {
        ((*host).register_view)(id.as_ptr(), tooltip.as_ptr(), &table, std::ptr::null_mut())
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
            tooltip.as_ptr(),
            IC_SIDE_RIGHT,
            100,
            IC_ENABLE_ALWAYS,
            on_clicked,
            std::ptr::null_mut(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_hands_back_the_same_document() {
        let answer = describe(std::ptr::null(), 0, std::ptr::null_mut());
        let seen = unsafe { std::slice::from_raw_parts(answer.data, answer.len as usize) };
        assert_eq!(seen, document().to_string().as_bytes());
    }
}
