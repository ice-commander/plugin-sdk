//! A connection whose panel carries a terminal and changes permissions.
//!
//! The kind is a folder on this machine, and the mount is rooted at it. That is
//! deliberate and unlike `website-fs`, which mounts the whole site and only
//! opens at a folder: here the connection is the folder. A path that climbs out
//! with `..` or names another drive is refused. A symbolic link inside is
//! followed wherever its owner pointed it, so this keeps the panel's paths in
//! the folder; it is not a fence around the disk.
//!
//! Filling `shell_open`, `shell_read` and `shell_write` is what puts a terminal
//! on the panel; `shell_resize` and `shell_close` are optional. One host thread
//! drives all five in turn and keeps polling `shell_read`, which answers at
//! once: an empty slice while there is nothing to say, a null one when the shell
//! has ended. The shell here is a line-echo shell the plugin answers itself, not
//! a process it starts, so it behaves the same on every system and runs nothing
//! on the user's machine. A shell that talks on its own — a process, a server —
//! gets a thread of its own and hands what it hears to `shell_read` through a
//! channel.
//!
//! `set_permissions` is what makes the host offer chmod, and the host also calls
//! it after writing a copied file or folder, to give it the mode the original
//! had. Windows has no mode bits, so there the slot stays empty and the host
//! offers nothing. `remove` and `rename` stay empty everywhere, so nothing here
//! deletes or moves a file.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, needs_connection_kinds, HostCheck, IcBytes, IcConnectionVTable, IcDirEntry,
    IcFsHandle, IcFsSource, IcFsVTable, IcHost, IcListing, IcShellHandle, IC_ABI_VERSION,
    IC_ERR_HOST_TOO_OLD, IC_ERR_HOST_UNKNOWN, IC_ERR_INIT_FAILED, IC_ERR_IO, IC_OK,
};
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-shell",
    "Hello Shell",
    sdk_version!(),
    "SDK example: a folder connection with a terminal and chmod"
);

pub const KIND: &str = "local-folder";
pub const DOCUMENT: &str = include_str!("../documents/folder.json");
const PROMPT: &str = "> ";

#[derive(Default)]
struct Answers {
    names: Vec<CString>,
    view: Vec<IcDirEntry>,
    bytes: Vec<u8>,
    error: CString,
}

pub struct Mounted {
    root: PathBuf,
    // last_error is asked after the host lets go of the mount, so it may arrive beside a listing.
    answers: Mutex<Answers>,
}

pub fn mounted_from(settings: &str) -> Option<Mounted> {
    let parsed: serde_json::Value = serde_json::from_str(settings).ok()?;
    let root = PathBuf::from(parsed.get("path")?.as_str()?);
    root.is_dir().then(|| Mounted {
        root,
        answers: Mutex::default(),
    })
}

pub fn inside(root: &Path, asked: &str) -> Option<PathBuf> {
    let mut at = root.to_path_buf();
    for part in asked.split('/').filter(|part| !part.is_empty()) {
        let mut pieces = Path::new(part).components();
        match (pieces.next(), pieces.next()) {
            (Some(Component::Normal(_)), None) => at.push(part),
            _ => return None,
        }
    }
    Some(at)
}

fn text(path: *const c_char) -> Option<String> {
    (!path.is_null()).then(|| {
        unsafe { CStr::from_ptr(path) }
            .to_string_lossy()
            .into_owned()
    })
}

impl Mounted {
    fn answers(&self) -> MutexGuard<'_, Answers> {
        self.answers.lock().unwrap_or_else(|held| held.into_inner())
    }
}

fn mount<'a>(handle: IcFsHandle) -> Option<&'a Mounted> {
    (!handle.is_null()).then(|| unsafe { &*(handle as *const Mounted) })
}

fn reach<T>(
    handle: IcFsHandle,
    path: *const c_char,
    work: impl FnOnce(&Path) -> std::io::Result<T>,
) -> Option<T> {
    let mounted = mount(handle)?;
    let outcome = match text(path) {
        None => Err("no path given".to_string()),
        Some(asked) => match inside(&mounted.root, &asked) {
            Some(at) => work(&at).map_err(|e| format!("{}: {e}", at.display())),
            None => Err(format!("{asked} leads out of {}", mounted.root.display())),
        },
    };
    outcome
        .map_err(|why| mounted.answers().error = CString::new(why).unwrap_or_default())
        .ok()
}

fn status(done: Option<()>) -> c_int {
    done.map_or(IC_ERR_IO, |()| IC_OK)
}

fn rows_in(at: &Path) -> std::io::Result<Vec<(CString, IcDirEntry)>> {
    let mut rows: Vec<(CString, IcDirEntry)> = std::fs::read_dir(at)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let meta = entry.path().metadata().ok()?;
            #[cfg(unix)]
            let (permissions, has_permissions) = (meta.permissions().mode() & 0o7777, 1);
            #[cfg(not(unix))]
            let (permissions, has_permissions) = (0, 0);
            let row = IcDirEntry {
                name: std::ptr::null(),
                is_dir: c_int::from(meta.is_dir()),
                size: meta.len(),
                modified: 0,
                permissions,
                has_permissions,
            };
            let name = CString::new(entry.file_name().to_string_lossy().as_bytes()).ok()?;
            Some((name, row))
        })
        .collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(rows)
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
    match std::str::from_utf8(raw).ok().and_then(mounted_from) {
        Some(mounted) => Box::into_raw(Box::new(mounted)) as IcFsHandle,
        None => std::ptr::null_mut(),
    }
}

extern "C" fn never_opened_inside_a_file(
    _source: IcFsSource,
    _path: *const c_char,
    _user_data: *mut c_void,
) -> IcFsHandle {
    std::ptr::null_mut()
}

extern "C" fn fs_close(handle: IcFsHandle) {
    if !handle.is_null() {
        drop(unsafe { Box::from_raw(handle as *mut Mounted) });
    }
}

extern "C" fn fs_list(handle: IcFsHandle, path: *const c_char) -> IcListing {
    let (Some(mounted), Some(rows)) = (mount(handle), reach(handle, path, rows_in)) else {
        return IcListing::EMPTY;
    };
    let (names, mut view): (Vec<CString>, Vec<IcDirEntry>) = rows.into_iter().unzip();
    for (row, name) in view.iter_mut().zip(&names) {
        row.name = name.as_ptr();
    }
    let mut answers = mounted.answers();
    (answers.names, answers.view) = (names, view);
    IcListing {
        items: answers.view.as_ptr(),
        count: answers.view.len() as u32,
    }
}

extern "C" fn fs_read(handle: IcFsHandle, path: *const c_char) -> IcBytes {
    let (Some(mounted), Some(bytes)) = (mount(handle), reach(handle, path, |at| std::fs::read(at)))
    else {
        return IcBytes::EMPTY;
    };
    let mut answers = mounted.answers();
    answers.bytes = bytes;
    IcBytes {
        data: answers.bytes.as_ptr(),
        len: answers.bytes.len() as u64,
    }
}

extern "C" fn fs_write(
    handle: IcFsHandle,
    path: *const c_char,
    bytes: *const u8,
    len: u64,
) -> c_int {
    let body: &[u8] = if bytes.is_null() || len == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(bytes, len as usize) }
    };
    status(reach(handle, path, |at| std::fs::write(at, body)))
}

extern "C" fn fs_create_dir(handle: IcFsHandle, path: *const c_char) -> c_int {
    status(reach(handle, path, |at| std::fs::create_dir(at)))
}

#[cfg(unix)]
extern "C" fn fs_set_permissions(handle: IcFsHandle, path: *const c_char, mode: u32) -> c_int {
    let wanted = std::fs::Permissions::from_mode(mode & 0o7777);
    status(reach(handle, path, |at| {
        std::fs::set_permissions(at, wanted)
    }))
}

extern "C" fn fs_is_read_only(_handle: IcFsHandle) -> c_int {
    0
}

extern "C" fn fs_last_error(handle: IcFsHandle) -> *const c_char {
    mount(handle).map_or(std::ptr::null(), |mounted| mounted.answers().error.as_ptr())
}

#[derive(Default)]
struct Shell {
    at: PathBuf,
    rows: u32,
    cols: u32,
    line: String,
    pending: Vec<u8>,
    handed: Vec<u8>,
    ended: bool,
}

impl Shell {
    fn new(at: PathBuf, rows: u32, cols: u32) -> Shell {
        let mut shell = Shell {
            at,
            rows,
            cols,
            ..Shell::default()
        };
        shell.say("A line-echo shell: pwd, size and exit; anything else is said back.");
        shell.put(PROMPT);
        shell
    }

    fn put(&mut self, text: &str) {
        self.pending.extend_from_slice(text.as_bytes());
    }

    fn say(&mut self, text: &str) {
        self.put(text);
        self.put("\r\n");
    }

    fn typed(&mut self, bytes: &[u8]) {
        // Arrow and function keys arrive whole, one write each, and mean nothing here.
        if bytes.first() == Some(&0x1b) {
            return;
        }
        for key in String::from_utf8_lossy(bytes).chars() {
            if self.ended {
                return;
            }
            match key {
                '\r' => {
                    self.put("\r\n");
                    let line = std::mem::take(&mut self.line);
                    self.answer(line.trim());
                }
                '\u{7f}' | '\u{8}' => {
                    if self.line.pop().is_some() {
                        self.put("\u{8} \u{8}");
                    }
                }
                key if key.is_control() => {}
                key => {
                    self.line.push(key);
                    self.put(key.encode_utf8(&mut [0; 4]));
                }
            }
        }
    }

    fn answer(&mut self, line: &str) {
        match line {
            "" => {}
            "pwd" => self.say(&self.at.display().to_string()),
            "size" => self.say(&format!("{} rows, {} columns", self.rows, self.cols)),
            "exit" => {
                self.say("bye");
                self.ended = true;
                return;
            }
            said => self.say(said),
        }
        self.put(PROMPT);
    }

    fn take(&mut self) -> Option<&[u8]> {
        if self.ended && self.pending.is_empty() {
            return None;
        }
        self.handed = std::mem::take(&mut self.pending);
        Some(&self.handed)
    }
}

fn running<'a>(shell: IcShellHandle) -> Option<&'a mut Shell> {
    (!shell.is_null()).then(|| unsafe { &mut *(shell as *mut Shell) })
}

extern "C" fn shell_open(
    handle: IcFsHandle,
    cwd: *const c_char,
    rows: u32,
    cols: u32,
) -> IcShellHandle {
    let Some(mounted) = mount(handle) else {
        return std::ptr::null_mut();
    };
    let at = text(cwd)
        .and_then(|cwd| inside(&mounted.root, &cwd))
        .unwrap_or_else(|| mounted.root.clone());
    Box::into_raw(Box::new(Shell::new(at, rows, cols))) as IcShellHandle
}

extern "C" fn shell_read(shell: IcShellHandle) -> IcBytes {
    match running(shell).and_then(Shell::take) {
        Some(said) => IcBytes {
            data: said.as_ptr(),
            len: said.len() as u64,
        },
        None => IcBytes::EMPTY,
    }
}

extern "C" fn shell_write(shell: IcShellHandle, bytes: *const u8, len: u64) -> c_int {
    let Some(shell) = running(shell) else {
        return IC_ERR_IO;
    };
    if bytes.is_null() || shell.ended {
        return IC_ERR_IO;
    }
    shell.typed(unsafe { std::slice::from_raw_parts(bytes, len as usize) });
    IC_OK
}

extern "C" fn shell_resize(shell: IcShellHandle, rows: u32, cols: u32) -> c_int {
    let Some(shell) = running(shell) else {
        return IC_ERR_IO;
    };
    (shell.rows, shell.cols) = (rows, cols);
    IC_OK
}

extern "C" fn shell_close(shell: IcShellHandle) {
    if !shell.is_null() {
        drop(unsafe { Box::from_raw(shell as *mut Shell) });
    }
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
        write: Some(fs_write),
        create_dir: Some(fs_create_dir),
        remove: None,
        rename: None,
        shell_open: Some(shell_open),
        shell_read: Some(shell_read),
        shell_write: Some(shell_write),
        shell_resize: Some(shell_resize),
        shell_close: Some(shell_close),
        shell_available: None,
        columns: None,
        list_rows: None,
        action_state: None,
        cell_clicked: None,
        #[cfg(unix)]
        set_permissions: Some(fs_set_permissions),
        #[cfg(not(unix))]
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

    fn mounted_at(name: &str) -> (PathBuf, IcFsHandle) {
        let root =
            std::env::temp_dir().join(format!("sdk-hello-shell-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("a scratch folder");
        let settings = serde_json::json!({ "path": root }).to_string();
        let handle = connection_open(
            settings.as_ptr(),
            settings.len() as u64,
            std::ptr::null_mut(),
        );
        assert!(!handle.is_null());
        (root, handle)
    }

    fn c(text: &str) -> CString {
        CString::new(text).expect("a path")
    }

    fn failure_of(handle: IcFsHandle) -> String {
        text(fs_last_error(handle)).unwrap_or_default()
    }

    fn typed(shell: IcShellHandle, keys: &str) -> String {
        assert_eq!(shell_write(shell, keys.as_ptr(), keys.len() as u64), IC_OK);
        String::from_utf8_lossy(shell_read(shell).as_slice()).into_owned()
    }

    #[test]
    fn the_shell_says_back_what_was_typed_and_ends_on_exit() {
        let (root, handle) = mounted_at("echo");
        let shell = shell_open(handle, c("/inner").as_ptr(), 24, 80);
        assert!(String::from_utf8_lossy(shell_read(shell).as_slice()).ends_with(PROMPT));
        let idle = shell_read(shell);
        assert!(
            !idle.data.is_null() && idle.len == 0,
            "nothing to say is not the end"
        );

        assert_eq!(
            typed(shell, "hellp\u{7f}o\r"),
            "hellp\u{8} \u{8}o\r\nhello\r\n> "
        );
        let inner = root.join("inner").display().to_string();
        assert!(
            typed(shell, "pwd\r").contains(&inner),
            "starts where the panel stands"
        );
        assert_eq!(shell_resize(shell, 30, 100), IC_OK);
        assert_eq!(typed(shell, "size\r"), "size\r\n30 rows, 100 columns\r\n> ");

        assert_eq!(typed(shell, "exit\r"), "exit\r\nbye\r\n");
        assert!(shell_read(shell).data.is_null(), "ended");
        assert_ne!(shell_write(shell, b"x".as_ptr(), 1), IC_OK);
        shell_close(shell);
        fs_close(handle);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_path_that_climbs_out_is_refused_with_a_reason() {
        let (root, handle) = mounted_at("out");
        assert_eq!(inside(&root, "/a/b"), Some(root.join("a").join("b")));
        assert_eq!(inside(&root, "/a/../../x"), None);
        assert_eq!(
            fs_write(handle, c("/../escaped").as_ptr(), b"x".as_ptr(), 1),
            IC_ERR_IO
        );
        assert!(failure_of(handle).contains("leads out"));
        assert_ne!(fs_create_dir(handle, std::ptr::null()), IC_OK);
        assert_eq!(failure_of(handle), "no path given");
        assert!(mounted_from(r#"{"path":""}"#).is_none());
        fs_close(handle);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn a_mode_set_through_the_mount_lands_on_the_disk_and_comes_back_listed() {
        let (root, handle) = mounted_at("chmod");
        let before = std::fs::metadata(&root).expect("the folder").permissions();
        assert_ne!(fs_set_permissions(handle, std::ptr::null(), 0o600), IC_OK);
        assert_eq!(
            std::fs::metadata(&root).expect("the folder").permissions(),
            before
        );

        let script = c("/run.sh");
        assert_eq!(
            fs_write(handle, script.as_ptr(), b"echo hi\n".as_ptr(), 8),
            IC_OK
        );

        for mode in [0o755, 0o600] {
            assert_eq!(fs_set_permissions(handle, script.as_ptr(), mode), IC_OK);
            let on_disk = std::fs::metadata(root.join("run.sh")).expect("the file");
            assert_eq!(on_disk.permissions().mode() & 0o7777, mode);
            let row = fs_list(handle, c("/").as_ptr()).as_slice()[0];
            assert_eq!(row.name_string(), "run.sh");
            assert_eq!(row.permissions_opt(), Some(mode));
        }
        assert_ne!(
            fs_set_permissions(handle, c("/missing").as_ptr(), 0o644),
            IC_OK
        );
        assert!(failure_of(handle).contains("missing"));
        fs_close(handle);
        let _ = std::fs::remove_dir_all(root);
    }
}
