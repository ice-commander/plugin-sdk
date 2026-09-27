//! A viewer that plays a file without decoding a byte of it, and hears when it ends.
//!
//! A `media` node names what to play — `file:` and a path on the filesystem the
//! window was opened on — and the application plays it with a player of its own.
//! The plugin never sees a sample. What it does hear is the end: an `ended` event
//! (`IC_EVENT_ENDED`) whose `node` is the id of the player that ran out.
//!
//! The answer here is the one a music player needs: the next `.wav` in the folder.
//! Another file is another document, so the reply is `redescribe`, and the
//! document that follows says `autoplay` — a window the user has just opened
//! waits to be pressed, one that moved on by itself keeps playing. After the last
//! file only the words change, and a `set` is enough for that.
//!
//! A terminal has no player and never reports an end, so `init` refuses it with
//! `IC_ERR_NOT_THIS_HOST`.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, needs_up_to, HostCheck, IcBytes, IcFsSource, IcHost, IcViewVTable, IcViewerVTable,
    IC_ABI_VERSION, IC_ERR_HOST_TOO_OLD, IC_ERR_HOST_UNKNOWN, IC_ERR_INIT_FAILED,
    IC_ERR_NOT_THIS_HOST, IC_EVENT_ENDED, IC_HOST_CONSOLE, IC_OK,
};
use serde_json::{json, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-media",
    "Hello Media",
    sdk_version!(),
    "SDK example: a viewer that plays media and hears when it ends"
);

pub const ID: &str = "hello-media";
pub const EXTENSIONS: &str = ".wav";
// Below the real music player, which should keep a .wav wherever both are switched on.
const PRIORITY: i32 = -1;
const PLAYER: &str = "sound";
const THE_END: &str = "Played to the end of the folder";

static HOST: AtomicUsize = AtomicUsize::new(0);

struct Playing {
    tracks: Vec<String>,
    at: usize,
    autoplay: bool,
    ended: bool,
}

thread_local! {
    static PLAYING: RefCell<BTreeMap<u64, Playing>> = const { RefCell::new(BTreeMap::new()) };
    static ANSWER: RefCell<String> = const { RefCell::new(String::new()) };
}

/// The tracks of a folder in the order a person would play them, and where the
/// opened one is among them.
pub fn playlist(names: impl IntoIterator<Item = String>, opened: &str) -> (Vec<String>, usize) {
    let mut tracks: Vec<String> = names
        .into_iter()
        .filter(|name| name.to_lowercase().ends_with(EXTENSIONS))
        .collect();
    tracks.sort_by_key(|name| name.to_lowercase());
    match tracks.iter().position(|name| name == opened) {
        Some(at) => (tracks, at),
        None => (vec![opened.to_string()], 0),
    }
}

fn names_beside(source: IcFsSource) -> Vec<String> {
    let host = HOST.load(Ordering::Relaxed) as *const IcHost;
    if host.is_null() {
        return Vec::new();
    }
    let listing = unsafe { ((*host).fs_list)(source, c"/".as_ptr()) };
    listing
        .as_slice()
        .iter()
        .filter(|entry| entry.is_dir == 0)
        .map(|entry| entry.name_string())
        .collect()
}

fn said(playing: &Playing) -> String {
    if playing.ended {
        return THE_END.to_string();
    }
    let (place, of) = (playing.at + 1, playing.tracks.len());
    match playing.tracks.get(playing.at + 1) {
        Some(next) => format!("{place} of {of}, then {next}"),
        None => format!("{place} of {of}, the last in the folder"),
    }
}

fn document_for(playing: &Playing) -> String {
    let now = playing.tracks.get(playing.at).cloned().unwrap_or_default();
    json!({
        "schema": 1,
        "data": { "said": said(playing) },
        "fields": [],
        "form": {
            "t": "view", "surface": "window", "spacing": 8, "padding": 12,
            "children": [
                { "t": "text", "id": "name", "role": "title1", "wrap": true,
                  "text": { "literal": now } },
                { "t": "media", "id": PLAYER, "media": "audio",
                  "src": format!("file:{now}"), "autoplay": playing.autoplay },
                { "t": "text", "id": "said", "role": "dim", "text": "{data.said}" }
            ]
        }
    })
    .to_string()
}

/// What the window answers an event with, moving on through the folder when the
/// event is its player running out.
fn heard(playing: &mut Playing, event: &Value) -> Value {
    if event["type"] != IC_EVENT_ENDED || event["node"] != PLAYER {
        return json!({});
    }
    if playing.at + 1 < playing.tracks.len() {
        playing.at += 1;
        playing.autoplay = true;
        return json!({ "redescribe": true });
    }
    playing.ended = true;
    json!({ "set": { "data.said": THE_END } })
}

fn parsed(raw: *const u8, len: u64) -> Value {
    if raw.is_null() || len == 0 {
        return Value::Null;
    }
    serde_json::from_slice(unsafe { std::slice::from_raw_parts(raw, len as usize) })
        .unwrap_or(Value::Null)
}

fn answer_with(text: String) -> IcBytes {
    ANSWER.with(|held| {
        let mut held = held.borrow_mut();
        *held = text;
        IcBytes {
            data: held.as_ptr(),
            len: held.len() as u64,
        }
    })
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
    let opened = unsafe { CStr::from_ptr(path) }
        .to_string_lossy()
        .trim_matches('/')
        .to_string();
    let (tracks, at) = playlist(names_beside(source), &opened);
    let playing = Playing {
        tracks,
        at,
        autoplay: false,
        ended: false,
    };
    PLAYING.with(|open| open.borrow_mut().insert(instance, playing));
    IC_OK
}

extern "C" fn viewer_closed(instance: u64, _user_data: *mut c_void) {
    PLAYING.with(|open| open.borrow_mut().remove(&instance));
}

extern "C" fn viewer_describe(ctx: *const u8, len: u64, _user_data: *mut c_void) -> IcBytes {
    let instance = parsed(ctx, len)["instance"].as_u64().unwrap_or(0);
    match PLAYING.with(|open| open.borrow().get(&instance).map(document_for)) {
        Some(drawn) => answer_with(drawn),
        None => IcBytes::EMPTY,
    }
}

extern "C" fn viewer_event(raw: *const u8, len: u64, _user_data: *mut c_void) -> IcBytes {
    let event = parsed(raw, len);
    let instance = event["instance"].as_u64().unwrap_or(0);
    let reply = PLAYING.with(|open| match open.borrow_mut().get_mut(&instance) {
        Some(playing) => heard(playing, &event),
        None => json!({}),
    });
    answer_with(reply.to_string())
}

pub fn view_vtable() -> IcViewVTable {
    IcViewVTable {
        struct_size: std::mem::size_of::<IcViewVTable>() as u32,
        describe: viewer_describe,
        on_event: Some(viewer_event),
        closed: None,
    }
}

pub fn viewer_vtable(view: *const IcViewVTable) -> IcViewerVTable {
    IcViewerVTable {
        struct_size: std::mem::size_of::<IcViewerVTable>() as u32,
        view,
        open: viewer_open,
        closed: Some(viewer_closed),
        content: None,
        closing: None,
        canvas_ready: None,
        canvas_draw: None,
        canvas_gone: None,
    }
}

/// A terminal draws a `media` node as its name and plays nothing. A null kind
/// is the desktop application.
fn has_no_player(kind: *const c_char) -> bool {
    !kind.is_null() && unsafe { CStr::from_ptr(kind) }.to_str() == Ok(IC_HOST_CONSOLE)
}

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_init(host: *const IcHost, kind: *const c_char) -> c_int {
    if has_no_player(kind) {
        return IC_ERR_NOT_THIS_HOST;
    }
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
            PRIORITY,
            &viewer,
            std::ptr::null_mut(),
        )
    }
}

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_shutdown() {
    PLAYING.with(|open| open.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened_on(instance: u64, tracks: &[&str]) {
        let playing = Playing {
            tracks: tracks.iter().map(|name| name.to_string()).collect(),
            at: 0,
            autoplay: false,
            ended: false,
        };
        PLAYING.with(|open| open.borrow_mut().insert(instance, playing));
    }

    fn answered(bytes: IcBytes) -> Value {
        parsed(bytes.data, bytes.len)
    }

    fn sent(instance: u64, kind: &str, node: &str) -> Value {
        let event =
            json!({ "type": kind, "node": node, "values": {}, "instance": instance }).to_string();
        answered(viewer_event(
            event.as_ptr(),
            event.len() as u64,
            std::ptr::null_mut(),
        ))
    }

    fn described(instance: u64) -> Value {
        let context = format!(r#"{{"host":{{"kind":"gtk"}},"instance":{instance}}}"#);
        answered(viewer_describe(
            context.as_ptr(),
            context.len() as u64,
            std::ptr::null_mut(),
        ))
    }

    fn player_in(document: &Value) -> Value {
        document["form"]["children"]
            .as_array()
            .and_then(|children| children.iter().find(|node| node["id"] == PLAYER))
            .cloned()
            .unwrap_or(Value::Null)
    }

    #[test]
    fn the_folder_is_the_playlist() {
        let folder = ["b.wav", "notes.txt", "A.WAV", "c.wav"].map(String::from);
        let (tracks, at) = playlist(folder, "b.wav");
        assert_eq!(tracks, ["A.WAV", "b.wav", "c.wav"]);
        assert_eq!(at, 1, "the one that was opened is the one playing");
        assert_eq!(
            playlist(Vec::new(), "lone.wav"),
            (vec!["lone.wav".to_string()], 0),
            "a folder that could not be listed still plays what was opened"
        );
    }

    #[test]
    fn an_ended_track_moves_on_and_the_last_one_says_so() {
        opened_on(3, &["a.wav", "b.wav"]);
        let first = player_in(&described(3));
        assert_eq!(first["src"], "file:a.wav");
        assert_eq!(first["autoplay"], false, "what was just opened waits");

        assert_eq!(
            sent(3, IC_EVENT_ENDED, PLAYER),
            json!({ "redescribe": true })
        );
        let next = described(3);
        assert_eq!(player_in(&next)["src"], "file:b.wav");
        assert_eq!(
            player_in(&next)["autoplay"],
            true,
            "what it moved on to plays"
        );
        assert_eq!(next["data"]["said"], "2 of 2, the last in the folder");

        assert_eq!(
            sent(3, IC_EVENT_ENDED, PLAYER),
            json!({ "set": { "data.said": THE_END } })
        );
        let after = described(3);
        assert_eq!(player_in(&after)["src"], "file:b.wav");
        assert_eq!(after["data"]["said"], THE_END, "a later describe agrees");

        viewer_closed(3, std::ptr::null_mut());
        assert_eq!(described(3), Value::Null);
    }

    #[test]
    fn only_its_own_player_running_out_moves_it() {
        opened_on(4, &["a.wav", "b.wav"]);
        assert_eq!(sent(4, IC_EVENT_ENDED, "another"), json!({}));
        assert_eq!(sent(4, "activate", PLAYER), json!({}));
        assert_eq!(
            sent(99, IC_EVENT_ENDED, PLAYER),
            json!({}),
            "a window nobody opened"
        );
        assert_eq!(player_in(&described(4))["src"], "file:a.wav");
        viewer_closed(4, std::ptr::null_mut());
    }

    #[test]
    fn a_terminal_is_refused_before_anything_is_asked() {
        let console = CString::new(IC_HOST_CONSOLE).expect("a kind");
        assert_eq!(
            ic_plugin_init(std::ptr::null(), console.as_ptr()),
            IC_ERR_NOT_THIS_HOST
        );
    }
}
