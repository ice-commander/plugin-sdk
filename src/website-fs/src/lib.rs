//! A connection kind: its settings form, and a filesystem behind it.
//!
//! The form is a document like any other, and what the user types comes back as
//! the settings `open` is handed. A field marked `secret` is stored encrypted and
//! never shown to the plugin in a snapshot.
//!
//! A connection reaches outside, so every call into its filesystem runs on a
//! worker thread: blocking there is expected and costs the interface nothing.
//!
//! Mount the whole thing, wherever the user wants to start. `"opens_at": "<bind>"`
//! names the field holding that folder and the panel walks there after mounting;
//! rooting the mount there instead would take the way back out with it.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod scan;

use ic_plugin_api::{
    check_host, needs_connection_kinds, HostCheck, IcBytes, IcConnectionVTable, IcDirEntry,
    IcFsHandle, IcFsSource, IcFsVTable, IcHost, IcListing, IC_ABI_VERSION, IC_ERR_HOST_TOO_OLD,
    IC_ERR_HOST_UNKNOWN, IC_ERR_INIT_FAILED,
};
use scan::Found;
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-website-fs",
    "Website",
    sdk_version!(),
    "SDK example: browses what a web page links to"
);

pub const KIND: &str = "website";
pub const DOCUMENT: &str = include_str!("../documents/website.json");

pub struct Opened {
    url: String,
    user_agent: String,
    token: Option<String>,
    found: Option<Found>,
    names: Vec<CString>,
    view: Vec<IcDirEntry>,
    bytes: Vec<u8>,
    error: CString,
}

impl Opened {
    fn ensure_fetched(&mut self) -> Result<(), String> {
        if self.found.is_some() {
            return Ok(());
        }
        let mut request = ureq::get(&self.url).header("user-agent", &self.user_agent);
        if let Some(token) = &self.token {
            request = request.header("authorization", &format!("Bearer {token}"));
        }
        let body = request
            .call()
            .map_err(|e| format!("{}: {e}", self.url))?
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("{}: {e}", self.url))?;
        self.found = Some(scan::scan(&self.url, &body));
        Ok(())
    }
}

fn with_opened<R>(handle: IcFsHandle, f: impl FnOnce(&mut Opened) -> R) -> Option<R> {
    if handle.is_null() {
        return None;
    }
    let cell = unsafe { &*(handle as *const RefCell<Opened>) };
    let mut held = cell.borrow_mut();
    Some(f(&mut held))
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

pub fn opened_from(settings: &str) -> Option<Opened> {
    let parsed: serde_json::Value = serde_json::from_str(settings).ok()?;
    let text = |key: &str| -> Option<String> {
        parsed
            .get(key)
            .and_then(|held| held.as_str())
            .map(str::to_string)
            .filter(|held| !held.is_empty())
    };
    let url = text("url")?;
    let url = if url.starts_with("http://") || url.starts_with("https://") {
        url
    } else {
        format!("https://{url}")
    };
    Some(Opened {
        url,
        user_agent: text("user_agent").unwrap_or_else(|| "ice-commander-sdk".to_string()),
        token: text("send_token")
            .filter(|held| held == "true")
            .and_then(|_| text("token")),
        found: None,
        names: Vec::new(),
        view: Vec::new(),
        bytes: Vec::new(),
        error: CString::default(),
    })
}

extern "C" fn connection_open(
    settings: *const u8,
    settings_len: u64,
    _user_data: *mut c_void,
) -> IcFsHandle {
    if settings.is_null() {
        return std::ptr::null_mut();
    }
    let raw = unsafe { std::slice::from_raw_parts(settings, settings_len as usize) };
    let Ok(text) = std::str::from_utf8(raw) else {
        return std::ptr::null_mut();
    };
    match opened_from(text) {
        Some(opened) => Box::into_raw(Box::new(RefCell::new(opened))) as IcFsHandle,
        None => std::ptr::null_mut(),
    }
}

/// This filesystem is reached by connecting to a site, never by opening a file
/// of another one, so there is nothing to open inside. A mount arrives through
/// `connection_open` above.
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

extern "C" fn fs_list(handle: IcFsHandle, path: *const c_char) -> IcListing {
    let inside = wanted(path);
    with_opened(handle, |opened| {
        if let Err(why) = opened.ensure_fetched() {
            opened.error = CString::new(why).unwrap_or_default();
            return IcListing::EMPTY;
        }
        let found = opened.found.as_ref().expect("fetched above");

        let rows: Vec<(String, bool, u64)> = if inside.is_empty() {
            Found::groups()
                .iter()
                .map(|name| {
                    let count = found.group(name).map(<[String]>::len).unwrap_or(0);
                    ((*name).to_string(), true, count as u64)
                })
                .collect()
        } else {
            found
                .group(&inside)
                .unwrap_or(&[])
                .iter()
                .map(|address| (address.clone(), false, address.len() as u64))
                .collect()
        };

        opened.names = rows
            .iter()
            .map(|(name, _, _)| CString::new(name.as_str()).unwrap_or_default())
            .collect();
        opened.view = rows
            .iter()
            .enumerate()
            .map(|(at, (_, is_dir, size))| IcDirEntry {
                name: opened.names[at].as_ptr(),
                is_dir: c_int::from(*is_dir),
                size: *size,
                modified: 0,
                permissions: 0o444,
                has_permissions: 1,
            })
            .collect();
        IcListing {
            items: opened.view.as_ptr(),
            count: opened.view.len() as u32,
        }
    })
    .unwrap_or(IcListing::EMPTY)
}

extern "C" fn fs_read(handle: IcFsHandle, path: *const c_char) -> IcBytes {
    let asked = wanted(path);
    with_opened(handle, |opened| {
        let address = asked.rsplit('/').next().unwrap_or(&asked).to_string();
        let _ = address;
        opened.bytes = format!("{asked}\n").into_bytes();
        IcBytes {
            data: opened.bytes.as_ptr(),
            len: opened.bytes.len() as u64,
        }
    })
    .unwrap_or(IcBytes::EMPTY)
}

extern "C" fn fs_is_read_only(_handle: IcFsHandle) -> c_int {
    1
}

extern "C" fn fs_last_error(handle: IcFsHandle) -> *const c_char {
    with_opened(handle, |opened| opened.error.as_ptr()).unwrap_or(std::ptr::null())
}

pub fn fs_vtable() -> IcFsVTable {
    IcFsVTable {
        struct_size: std::mem::size_of::<IcFsVTable>() as u32,
        open_in: never_opened_inside_a_file,
        close: fs_close,
        list: fs_list,
        read: fs_read,
        is_read_only: fs_is_read_only,
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

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_init(host: *const IcHost, _kind: *const c_char) -> c_int {
    match check_host(host, IC_ABI_VERSION, needs_connection_kinds()) {
        HostCheck::Ok => {}
        HostCheck::WrongMagic => return IC_ERR_HOST_UNKNOWN,
        HostCheck::TooOld { .. } | HostCheck::Truncated { .. } => return IC_ERR_HOST_TOO_OLD,
    }
    let Ok(id) = CString::new(KIND) else {
        return IC_ERR_INIT_FAILED;
    };

    let fs = fs_vtable();
    let connection = IcConnectionVTable {
        struct_size: std::mem::size_of::<IcConnectionVTable>() as u32,
        open: connection_open,
        fs: &fs,
        describe: None,
        on_event: None,
    };
    unsafe {
        ((*host).register_connection_kind)(
            id.as_ptr(),
            DOCUMENT.as_ptr(),
            DOCUMENT.len() as u64,
            &connection,
            std::ptr::null_mut(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_address_without_a_scheme_is_still_fetchable() {
        let opened = opened_from(r#"{"url":"example.org"}"#).expect("a connection");
        assert_eq!(opened.url, "https://example.org");
        let kept = opened_from(r#"{"url":"http://example.org"}"#).expect("a connection");
        assert_eq!(kept.url, "http://example.org");
    }

    #[test]
    fn settings_without_an_address_do_not_open_a_connection() {
        assert!(opened_from(r#"{}"#).is_none());
        assert!(opened_from(r#"{"url":""}"#).is_none());
        assert!(opened_from("not json").is_none());
    }

    #[test]
    fn the_token_is_sent_only_when_the_switch_asked_for_it() {
        let without =
            opened_from(r#"{"url":"example.org","token":"t-1","send_token":"false"}"#).unwrap();
        assert_eq!(without.token, None);
        let with =
            opened_from(r#"{"url":"example.org","token":"t-1","send_token":"true"}"#).unwrap();
        assert_eq!(with.token.as_deref(), Some("t-1"));
    }

    fn seeded() -> IcFsHandle {
        let mut opened = opened_from(r#"{"url":"https://example.org/"}"#).expect("a connection");
        opened.found = Some(scan::scan(
            "https://example.org/",
            r#"<link rel="stylesheet" href="/a.css"><script src="/b.js"></script>
               <img src="/c.png"><a href="/d">d</a>"#,
        ));
        Box::into_raw(Box::new(RefCell::new(opened))) as IcFsHandle
    }

    #[test]
    fn the_root_is_the_four_groups_and_a_group_holds_its_addresses() {
        let handle = seeded();
        let root = CString::new("").expect("a path");
        let listing = fs_list(handle, root.as_ptr());
        let names: Vec<String> = listing
            .as_slice()
            .iter()
            .map(|entry| entry.name_string())
            .collect();
        assert_eq!(names, vec!["images", "links", "scripts", "styles"]);
        assert!(listing.as_slice().iter().all(|entry| entry.is_directory()));

        let group = CString::new("styles").expect("a path");
        let inside = fs_list(handle, group.as_ptr());
        let held: Vec<String> = inside
            .as_slice()
            .iter()
            .map(|entry| entry.name_string())
            .collect();
        assert_eq!(held, vec!["https://example.org/a.css"]);
        assert!(inside.as_slice().iter().all(|entry| !entry.is_directory()));
        fs_close(handle);
    }

    #[test]
    fn a_group_nobody_declared_is_empty_rather_than_an_error() {
        let handle = seeded();
        let nonsense = CString::new("videos").expect("a path");
        assert_eq!(fs_list(handle, nonsense.as_ptr()).count, 0);
        fs_close(handle);
    }
}
