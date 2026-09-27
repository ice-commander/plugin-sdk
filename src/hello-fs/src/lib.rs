//! A file extension that opens like a folder.
//!
//! **The plugin is never handed the bytes.** It is handed the filesystem the file
//! lives on and the path it lives at, and reads what it needs through `fs_open`
//! and the calls beside it — which is what lets the same plugin open a file on a
//! disk, one inside an archive and one on a server without knowing the
//! difference. This example reads its file whole because a `.hello` file is tiny;
//! a format that can be read a piece at a time should seek about instead.
//!
//! Writing goes the same way round: `fs_open(source, path, IC_OPEN_WRITE)`,
//! `fs_write`, `fs_truncate`, `fs_close`, then `fs_changed` so the host throws
//! away what it remembered. There is no slot that hands a whole file back.
//!
//! The vtable slots after `list` and `read` are optional and say what else the
//! mount can do: writing, renaming, removing, a shell, columns of its own. Leave
//! one empty and the host refuses that operation on its own. `hello-panel` is the
//! same idea with those slots filled in.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, needs_up_to, HostCheck, IcBytes, IcDirEntry, IcFsHandle, IcFsSource, IcFsVTable,
    IcHost, IcListing, IC_ABI_VERSION, IC_ERR_HOST_TOO_OLD, IC_ERR_HOST_UNKNOWN,
    IC_ERR_INIT_FAILED, IC_OPEN_READ, IC_SEEK_END, IC_SEEK_SET,
};
use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-fs",
    "Hello Filesystem",
    sdk_version!(),
    "SDK example: opens .hello files as a folder of two files"
);

pub const EXTENSIONS: &str = ".hello";

/// A mount reads through this table later, so the pointer has to outlive `init`.
static HOST: AtomicUsize = AtomicUsize::new(0);

fn host() -> *const IcHost {
    HOST.load(Ordering::Relaxed) as *const IcHost
}

/// The seek to the end is only how the size is asked for, so the buffer is
/// allocated once; a source that cannot really seek answers it all the same,
/// and pays for it by fetching what it must.
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

/// `hello` the mount invents; `world` is the `.hello` file itself, read off the
/// filesystem it lives on. One half needs no host, the other cannot be had
/// without one.
const MADE_UP: (&str, &[u8]) = ("hello", b"hello\n");
const AS_IT_WAS: &str = "world";

#[derive(Default)]
struct Opened {
    /// Both files, settled once when the mount opened: a `.hello` file is
    /// small enough to hold whole, so nothing here goes back to the host.
    contents: Vec<(&'static str, Vec<u8>)>,
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

extern "C" fn fs_open_in(
    source: IcFsSource,
    path: *const c_char,
    _user_data: *mut c_void,
) -> IcFsHandle {
    // `source` is the host's and stays good for as long as this mount, but the
    // reading belongs here: the fs calls run on the thread the source was
    // handed to, while this call is running.
    let Some(said) = read_whole(source, path) else {
        return std::ptr::null_mut();
    };
    let opened = Opened {
        contents: vec![(MADE_UP.0, MADE_UP.1.to_vec()), (AS_IT_WAS, said)],
        ..Opened::default()
    };
    Box::into_raw(Box::new(RefCell::new(opened))) as IcFsHandle
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
        let rows: Vec<(&str, u64)> = if inside.is_empty() {
            opened
                .contents
                .iter()
                .map(|(name, body)| (*name, body.len() as u64))
                .collect()
        } else {
            Vec::new()
        };
        opened.names = rows
            .iter()
            .map(|(name, _)| CString::new(*name).unwrap_or_default())
            .collect();
        opened.view = rows
            .iter()
            .enumerate()
            .map(|(at, (_, size))| IcDirEntry {
                name: opened.names[at].as_ptr(),
                is_dir: 0,
                size: *size,
                modified: 0,
                permissions: 0o644,
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
        let found = opened
            .contents
            .iter()
            .find(|(name, _)| *name == asked)
            .map(|(_, body)| body.clone());
        match found {
            Some(body) => {
                opened.bytes = body;
                IcBytes {
                    data: opened.bytes.as_ptr(),
                    len: opened.bytes.len() as u64,
                }
            }
            None => {
                opened.error =
                    CString::new(format!("no file named {asked:?} in here")).unwrap_or_default();
                IcBytes::EMPTY
            }
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

pub fn vtable() -> IcFsVTable {
    IcFsVTable {
        struct_size: std::mem::size_of::<IcFsVTable>() as u32,
        open_in: fs_open_in,
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
    // The furthest slot this plugin calls. `register_filesystem` is nearer the
    // front of the table, so asking for it would not say that the reads are
    // needed too — and asking for less than is used is how a plugin crashes
    // against an older host.
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

    let table = vtable();
    unsafe { ((*host).register_filesystem)(extensions.as_ptr(), &table, std::ptr::null_mut()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ic_plugin_api::IcStream;

    /// A host of the tests' own: one file, and the four calls this plugin
    /// makes to read it. The plugin is never handed bytes, so neither is it
    /// here — what a test hands over is a filesystem that holds the file.
    mod as_if_hosted {
        use super::*;

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
    /// makes put in over the top.
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
        });
    }

    /// A mount on a filesystem whose one file holds this. The filesystem
    /// outlives the mount, as the host's does; here that is a leak the test
    /// process carries to the end.
    fn open_holding(said: &str) -> IcFsHandle {
        hosted();
        let source = Box::into_raw(Box::new(as_if_hosted::Held(said.as_bytes().to_vec())));
        let name = CString::new("greeting.hello").expect("a name");
        fs_open_in(source as IcFsSource, name.as_ptr(), std::ptr::null_mut())
    }

    fn open() -> IcFsHandle {
        open_holding("world\n")
    }

    #[test]
    fn the_root_holds_the_two_files_and_nothing_else_does() {
        let handle = open();
        let root = CString::new("").expect("a path");
        let listing = fs_list(handle, root.as_ptr());
        let names: Vec<String> = listing
            .as_slice()
            .iter()
            .map(|entry| entry.name_string())
            .collect();
        assert_eq!(names, vec!["hello".to_string(), "world".to_string()]);
        assert!(listing.as_slice().iter().all(|entry| !entry.is_directory()));

        let deeper = CString::new("hello").expect("a path");
        assert_eq!(fs_list(handle, deeper.as_ptr()).count, 0);
        fs_close(handle);
    }

    #[test]
    fn each_file_reads_back_its_own_line() {
        let handle = open();
        for (name, body) in [("hello", "hello\n"), ("world", "world\n")] {
            let asked = CString::new(name).expect("a path");
            let got = fs_read(handle, asked.as_ptr());
            let seen = unsafe { std::slice::from_raw_parts(got.data, got.len as usize) };
            assert_eq!(seen, body.as_bytes());
        }
        fs_close(handle);
    }

    #[test]
    fn what_the_file_held_came_through_the_host_and_nowhere_else() {
        const SAID: &str = "what the file on the disk said\n";
        let handle = open_holding(SAID);
        let asked = CString::new("world").expect("a path");
        let got = fs_read(handle, asked.as_ptr());
        let seen = unsafe { std::slice::from_raw_parts(got.data, got.len as usize) };
        assert_eq!(seen, SAID.as_bytes());

        // And the listing sizes it as what was read, not as what was guessed.
        let root = CString::new("").expect("a path");
        let listing = fs_list(handle, root.as_ptr());
        assert_eq!(listing.as_slice()[1].size, SAID.len() as u64);
        fs_close(handle);
    }

    #[test]
    fn a_file_the_host_will_not_open_is_not_mounted() {
        hosted();
        let name = CString::new("greeting.hello").expect("a name");
        let handle = fs_open_in(std::ptr::null_mut(), name.as_ptr(), std::ptr::null_mut());
        assert!(handle.is_null(), "there is nothing to mount");
    }

    #[test]
    fn a_file_that_is_not_there_answers_empty_and_says_why() {
        let handle = open();
        let asked = CString::new("missing").expect("a path");
        let got = fs_read(handle, asked.as_ptr());
        assert_eq!(got.len, 0);
        let why = unsafe { CStr::from_ptr(fs_last_error(handle)) }
            .to_string_lossy()
            .to_string();
        assert!(why.contains("missing"), "{why}");
        fs_close(handle);
    }

    #[test]
    fn a_null_handle_is_answered_rather_than_dereferenced() {
        let path = CString::new("hello").expect("a path");
        assert_eq!(fs_list(std::ptr::null_mut(), path.as_ptr()).count, 0);
        assert_eq!(fs_read(std::ptr::null_mut(), path.as_ptr()).len, 0);
        assert!(fs_last_error(std::ptr::null_mut()).is_null());
        fs_close(std::ptr::null_mut());
    }
}
