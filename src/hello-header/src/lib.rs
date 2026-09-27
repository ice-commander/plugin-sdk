//! The same window as `hello-toolbar`, from a button on the window header.
//!
//! The header takes either side, and `set_header_label` and `set_header_visible`
//! change the button after the fact — `hello-panel` shows both. `set_header_icon`
//! swaps its picture, which is how a button says what state something is in; here
//! every press alternates between two. All three are the
//! same header bar, which only the desktop application draws, so the button is
//! asked for there alone while the window is registered everywhere.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, HostCheck, IcBytes, IcHost, IcViewVTable, IC_ABI_VERSION, IC_ERR_HOST_TOO_OLD,
    IC_ERR_HOST_UNKNOWN, IC_ERR_INIT_FAILED, IC_HOST_GTK, IC_OK, IC_SIDE_LEFT,
};
use serde_json::json;
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-header",
    "Hello Header",
    sdk_version!(),
    "SDK example: a header bar button opening a declarative window"
);

pub const VIEW_ID: &str = "sdk.hello.header";
const ICON: &str = include_str!("../../hello-toolbar/assets/hello.svg");
const PRESSED: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" width="16" height="16"><circle cx="8" cy="8" r="6.5" fill="#4e7ab5"/></svg>"##;

static HOST: AtomicUsize = AtomicUsize::new(0);
static SHOWING_PRESSED: AtomicBool = AtomicBool::new(false);

fn next_icon() -> &'static str {
    if SHOWING_PRESSED.fetch_xor(true, Ordering::Relaxed) {
        ICON
    } else {
        PRESSED
    }
}

thread_local! {
    static ANSWER: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

pub fn document() -> serde_json::Value {
    json!({
        "schema": 1,
        "kind": "sdk.hello.header",
        "form": {
            "t": "view",
            "surface": "dialog",
            "width": 420,
            "padding": 16,
            "spacing": 8,
            "children": [
                { "t": "text", "text": "Opened from the header bar", "role": "title2" },
                { "t": "separator" },
                { "t": "text", "wrap": true, "role": "dim",
                  "text": "A header button belongs to the window, not to a panel, so it has no selection to react to and is always live." },
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

pub extern "C" fn on_clicked(_user_data: *mut c_void, _parent_window: *mut c_void) {
    let host = HOST.load(Ordering::Relaxed) as *const IcHost;
    if host.is_null() {
        return;
    }
    let (Ok(id), Ok(svg)) = (CString::new(VIEW_ID), CString::new(next_icon())) else {
        return;
    };
    unsafe {
        ((*host).set_header_icon)(id.as_ptr(), svg.as_ptr());
        ((*host).open_view)(id.as_ptr(), std::ptr::null(), 0);
    }
}

/// A frontend with a header bar of its own. A null kind means the desktop.
fn has_a_header_bar(kind: *const c_char) -> bool {
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

    let (Ok(id), Ok(svg), Ok(label), Ok(tooltip)) = (
        CString::new(VIEW_ID),
        CString::new(ICON),
        CString::new("Hello"),
        CString::new("A window from the header bar"),
    ) else {
        return IC_ERR_INIT_FAILED;
    };

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

    // The window is the offering; the button is only one way in, and where
    // there is no header bar it would be a registration nobody draws.
    if !has_a_header_bar(kind) {
        return IC_OK;
    }

    unsafe {
        ((*host).add_header_button)(
            id.as_ptr(),
            svg.as_ptr(),
            label.as_ptr(),
            tooltip.as_ptr(),
            IC_SIDE_LEFT,
            100,
            on_clicked,
            std::ptr::null_mut(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_press_alternates_the_picture() {
        let first = next_icon();
        let second = next_icon();
        assert_ne!(first, second);
        assert_eq!(next_icon(), first);
    }
}
