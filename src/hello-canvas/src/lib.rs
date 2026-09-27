//! A viewer that paints its own picture instead of describing one.
//!
//! The document holds a `canvas` node and the frontend makes a place for it.
//! `canvas_ready` comes once the place exists, with its GL context current: that
//! is when the GL entry points are looked up, through the `get_proc_address` the
//! host hands over, so the plugin links against no GL library at all.
//! `canvas_draw` then paints one frame into the framebuffer it names, whose size
//! is in real pixels with the screen's scale already in it, and `canvas_gone`
//! lets go of whatever `canvas_ready` made, the context still current.
//!
//! Nothing is drawn unless asked for. `canvas_invalidate` asks, from any thread:
//! the clock here is a thread of its own, the way a decoder's would be, and it
//! stops in `canvas_gone`. Keep the document the same shape while the picture is
//! up — a window built again around a canvas is a new GL context, and
//! `canvas_gone` and `canvas_ready` come round again.
//!
//! Only the desktop application hands a plugin a GL context. A browser or a
//! terminal would show an empty box where the picture should be, so `init`
//! answers `IC_ERR_NOT_THIS_HOST` there and the application asks nothing more.

// The entry points below are called from C with raw pointers: that is what the
// boundary is, and clippy cannot see that the caller is the application.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use ic_plugin_api::{
    check_host, needs_canvas, HostCheck, IcBytes, IcCanvas, IcFrame, IcFsSource, IcHost,
    IcViewVTable, IcViewerVTable, IC_ABI_VERSION, IC_CANVAS_GL, IC_ERR_HOST_TOO_OLD,
    IC_ERR_HOST_UNKNOWN, IC_ERR_INIT_FAILED, IC_ERR_NOT_THIS_HOST, IC_HOST_GTK, IC_OK,
};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

include!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../version.rs"));

ic_plugin_api::declare_about!(
    "sdk-hello-canvas",
    "Hello Canvas",
    sdk_version!(),
    "SDK example: a viewer that paints its own picture"
);

pub const ID: &str = "hello-canvas";
pub const EXTENSIONS: &str = ".hellocanvas";

pub const DOCUMENT: &str = r#"{
  "schema": 1,
  "fields": [],
  "form": { "t": "view", "surface": "window", "spacing": 8, "padding": 8, "children": [
    { "t": "canvas", "id": "picture", "weight": 1 },
    { "t": "text", "id": "note", "role": "dim",
      "text": { "literal": "Every frame above is painted by the plugin itself." } }
  ] }
}"#;

pub const TICK: Duration = Duration::from_millis(33);
/// Seconds the bar takes from one edge to the other.
pub const SWEEP: f64 = 2.0;
const BAR: [f32; 3] = [0.95, 0.95, 0.9];

const GL_FRAMEBUFFER: u32 = 0x8D40;
const GL_COLOR_BUFFER_BIT: u32 = 0x4000;
const GL_SCISSOR_TEST: u32 = 0x0C11;

static HOST: AtomicUsize = AtomicUsize::new(0);

type Bind = unsafe extern "system" fn(u32, u32);
type Tint = unsafe extern "system" fn(f32, f32, f32, f32);
type Flag = unsafe extern "system" fn(u32);
type Rect = unsafe extern "system" fn(i32, i32, i32, i32);

struct Gl {
    bind_framebuffer: Bind,
    clear_color: Tint,
    clear: Flag,
    enable: Flag,
    disable: Flag,
    scissor: Rect,
}

impl Gl {
    fn load(canvas: &IcCanvas) -> Option<Gl> {
        let find = |name: &CStr| {
            let found = (canvas.get_proc_address)(canvas.proc_ctx, name.as_ptr());
            (!found.is_null()).then_some(found)
        };
        // SAFETY: each pointer is the host's answer for that GL name, cast to that function's signature.
        unsafe {
            Some(Gl {
                bind_framebuffer: std::mem::transmute::<*mut c_void, Bind>(find(
                    c"glBindFramebuffer",
                )?),
                clear_color: std::mem::transmute::<*mut c_void, Tint>(find(c"glClearColor")?),
                clear: std::mem::transmute::<*mut c_void, Flag>(find(c"glClear")?),
                enable: std::mem::transmute::<*mut c_void, Flag>(find(c"glEnable")?),
                disable: std::mem::transmute::<*mut c_void, Flag>(find(c"glDisable")?),
                scissor: std::mem::transmute::<*mut c_void, Rect>(find(c"glScissor")?),
            })
        }
    }

    fn paint(&self, fbo: i32, picture: &Picture) {
        let [r, g, b] = picture.background;
        let [x, y, width, height] = picture.bar;
        // SAFETY: called only from canvas_draw, between canvas_ready and canvas_gone, with the context current.
        unsafe {
            (self.bind_framebuffer)(GL_FRAMEBUFFER, fbo as u32);
            (self.clear_color)(r, g, b, 1.0);
            (self.clear)(GL_COLOR_BUFFER_BIT);
            (self.enable)(GL_SCISSOR_TEST);
            (self.scissor)(x, y, width, height);
            (self.clear_color)(BAR[0], BAR[1], BAR[2], 1.0);
            (self.clear)(GL_COLOR_BUFFER_BIT);
            (self.disable)(GL_SCISSOR_TEST);
        }
    }
}

/// One frame: a background drifting through colours and a bar sweeping across it.
pub struct Picture {
    pub background: [f32; 3],
    /// x, y, width, height in pixels, from the bottom left as GL counts.
    pub bar: [i32; 4],
}

pub fn picture_at(seconds: f64, width: i32, height: i32) -> Picture {
    let bar = (width / 8).max(1);
    let lap = (seconds / SWEEP).rem_euclid(2.0);
    let along = if lap < 1.0 { lap } else { 2.0 - lap };
    let x = (along * f64::from((width - bar).max(0))).round() as i32;
    let tint = |shift: f64| (0.3 + 0.2 * (seconds / 3.0 + shift).sin()) as f32;
    Picture {
        background: [tint(0.0), tint(2.1), tint(4.2)],
        bar: [x, 0, bar, height.max(0)],
    }
}

/// What `canvas_ready` made, let go in `canvas_gone`: the entry points, and the
/// clock asking for frames, which is stopped and waited for before it goes.
struct Painting {
    gl: Gl,
    since: Instant,
    stop: Sender<()>,
    clock: Option<JoinHandle<()>>,
}

impl Drop for Painting {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(clock) = self.clock.take() {
            let _ = clock.join();
        }
    }
}

/// A frame asked for from inside `canvas_draw` may be folded into the one being
/// drawn, so the asking is done from a thread of its own.
fn ticking(instance: u64) -> (Sender<()>, JoinHandle<()>) {
    let (stop, stopped) = std::sync::mpsc::channel();
    let clock = std::thread::spawn(move || {
        while stopped.recv_timeout(TICK) == Err(RecvTimeoutError::Timeout) {
            let host = HOST.load(Ordering::Relaxed) as *const IcHost;
            if !host.is_null() {
                unsafe { ((*host).canvas_invalidate)(instance) };
            }
        }
    });
    (stop, clock)
}

thread_local! {
    static PAINTING: RefCell<BTreeMap<u64, Painting>> = const { RefCell::new(BTreeMap::new()) };
}

/// The file only chose this viewer; what is shown is the plugin's own.
extern "C" fn viewer_open(
    _instance: u64,
    _source: IcFsSource,
    _path: *const c_char,
    _user_data: *mut c_void,
) -> c_int {
    IC_OK
}

extern "C" fn describe(_ctx: *const u8, _len: u64, _user_data: *mut c_void) -> IcBytes {
    IcBytes {
        data: DOCUMENT.as_ptr(),
        len: DOCUMENT.len() as u64,
    }
}

extern "C" fn canvas_ready(
    instance: u64,
    canvas: *const IcCanvas,
    _user_data: *mut c_void,
) -> c_int {
    if canvas.is_null() {
        return IC_ERR_INIT_FAILED;
    }
    let canvas = unsafe { *canvas };
    if canvas.api != IC_CANVAS_GL {
        return IC_ERR_NOT_THIS_HOST;
    }
    let Some(gl) = Gl::load(&canvas) else {
        return IC_ERR_INIT_FAILED;
    };
    let (stop, clock) = ticking(instance);
    let painting = Painting {
        gl,
        since: Instant::now(),
        stop,
        clock: Some(clock),
    };
    PAINTING.with(|held| held.borrow_mut().insert(instance, painting));
    IC_OK
}

extern "C" fn canvas_draw(instance: u64, frame: *const IcFrame, _user_data: *mut c_void) {
    if frame.is_null() {
        return;
    }
    let frame = unsafe { *frame };
    if frame.width <= 0 || frame.height <= 0 {
        return;
    }
    PAINTING.with(|held| {
        if let Some(painting) = held.borrow().get(&instance) {
            let seconds = painting.since.elapsed().as_secs_f64();
            let picture = picture_at(seconds, frame.width, frame.height);
            painting.gl.paint(frame.fbo, &picture);
        }
    });
}

extern "C" fn canvas_gone(instance: u64, _user_data: *mut c_void) {
    PAINTING.with(|held| held.borrow_mut().remove(&instance));
}

pub fn view_vtable() -> IcViewVTable {
    IcViewVTable {
        struct_size: std::mem::size_of::<IcViewVTable>() as u32,
        describe,
        on_event: None,
        closed: None,
    }
}

pub fn viewer_vtable(view: *const IcViewVTable) -> IcViewerVTable {
    IcViewerVTable {
        struct_size: std::mem::size_of::<IcViewerVTable>() as u32,
        view,
        open: viewer_open,
        // Everything kept is let go in canvas_gone, which comes before the window closes.
        closed: None,
        content: None,
        closing: None,
        canvas_ready: Some(canvas_ready),
        canvas_draw: Some(canvas_draw),
        canvas_gone: Some(canvas_gone),
    }
}

fn hands_out_gl(kind: *const c_char) -> bool {
    !kind.is_null() && unsafe { CStr::from_ptr(kind) }.to_str() == Ok(IC_HOST_GTK)
}

#[cfg_attr(feature = "export-abi", no_mangle)]
pub extern "C" fn ic_plugin_init(host: *const IcHost, kind: *const c_char) -> c_int {
    if !hands_out_gl(kind) {
        return IC_ERR_NOT_THIS_HOST;
    }
    match check_host(host, IC_ABI_VERSION, needs_canvas()) {
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
    PAINTING.with(|held| held.borrow_mut().clear());
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static CALLED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    fn called(what: String) {
        CALLED.with(|seen| seen.borrow_mut().push(what));
    }

    unsafe extern "system" fn bind(_: u32, fbo: u32) {
        called(format!("bind {fbo}"));
    }
    unsafe extern "system" fn clear(_: u32) {
        called("clear".to_string());
    }
    unsafe extern "system" fn scissor(_: i32, _: i32, width: i32, height: i32) {
        called(format!("bar {width}x{height}"));
    }
    unsafe extern "system" fn tint(_: f32, _: f32, _: f32, _: f32) {}
    unsafe extern "system" fn flag(_: u32) {}

    extern "C" fn fake_gl(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
        match unsafe { CStr::from_ptr(name) }.to_bytes() {
            b"glBindFramebuffer" => bind as *mut c_void,
            b"glClear" => clear as *mut c_void,
            b"glScissor" => scissor as *mut c_void,
            b"glClearColor" => tint as *mut c_void,
            b"glEnable" | b"glDisable" => flag as *mut c_void,
            _ => std::ptr::null_mut(),
        }
    }

    extern "C" fn no_gl(_ctx: *mut c_void, _name: *const c_char) -> *mut c_void {
        std::ptr::null_mut()
    }

    static ASKED: AtomicUsize = AtomicUsize::new(0);

    extern "C" fn counted(_instance: u64) -> c_int {
        ASKED.fetch_add(1, Ordering::Relaxed);
        IC_OK
    }

    fn canvas(
        api: u32,
        find: extern "C" fn(*mut c_void, *const c_char) -> *mut c_void,
    ) -> IcCanvas {
        IcCanvas {
            struct_size: std::mem::size_of::<IcCanvas>() as u32,
            api,
            get_proc_address: find,
            proc_ctx: std::ptr::null_mut(),
        }
    }

    #[test]
    fn the_window_is_a_canvas_with_a_note_under_it() {
        let document: serde_json::Value = serde_json::from_str(DOCUMENT).expect("a document");
        assert_eq!(document["form"]["children"][0]["t"], "canvas");
        assert_eq!(document["form"]["children"][0]["weight"], 1);
    }

    #[test]
    fn the_bar_sweeps_edge_to_edge_and_never_leaves_the_picture() {
        assert_eq!(picture_at(0.0, 800, 600).bar, [0, 0, 100, 600]);
        assert_eq!(picture_at(SWEEP, 800, 600).bar[0], 700);
        assert_eq!(picture_at(2.0 * SWEEP, 800, 600).bar[0], 0, "and back");
        for width in [1, 7, 640, 1921] {
            for step in 0..200 {
                let [x, _, bar, _] = picture_at(f64::from(step) * 0.037, width, 10).bar;
                assert!(x >= 0 && bar >= 1 && x + bar <= width);
            }
        }
        let (now, later) = (picture_at(0.0, 8, 8), picture_at(1.0, 8, 8));
        assert_ne!(now.background, later.background, "the colour drifts");
    }

    #[test]
    fn neither_a_terminal_nor_a_browser_is_handed_gl() {
        let host = ic_plugin_api::testing::silent_host();
        for kind in [ic_plugin_api::IC_HOST_CONSOLE, ic_plugin_api::IC_HOST_WEB] {
            let kind = CString::new(kind).expect("a kind");
            assert_eq!(ic_plugin_init(&host, kind.as_ptr()), IC_ERR_NOT_THIS_HOST);
        }
    }

    #[test]
    fn a_canvas_it_cannot_paint_on_is_refused() {
        let unknown = canvas(IC_CANVAS_GL + 1, fake_gl);
        let bare = canvas(IC_CANVAS_GL, no_gl);
        let nothing = std::ptr::null_mut();
        assert_eq!(canvas_ready(3, &unknown, nothing), IC_ERR_NOT_THIS_HOST);
        assert_eq!(canvas_ready(3, &bare, nothing), IC_ERR_INIT_FAILED);
        assert!(PAINTING.with(|held| held.borrow().is_empty()));
    }

    #[test]
    fn a_canvas_is_painted_asked_for_again_and_let_go() {
        let mut host = ic_plugin_api::testing::silent_host();
        host.canvas_invalidate = counted;
        let host: &'static IcHost = Box::leak(Box::new(host));
        let gtk = CString::new(IC_HOST_GTK).expect("a kind");
        assert_eq!(ic_plugin_init(host, gtk.as_ptr()), IC_OK);

        let ready = canvas_ready(9, &canvas(IC_CANVAS_GL, fake_gl), std::ptr::null_mut());
        assert_eq!(ready, IC_OK);
        let waited = Instant::now();
        while ASKED.load(Ordering::Relaxed) < 2 && waited.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(ASKED.load(Ordering::Relaxed) >= 2, "it asks by itself");

        let frame = IcFrame {
            struct_size: std::mem::size_of::<IcFrame>() as u32,
            fbo: 7,
            width: 800,
            height: 600,
            scale: 2.0,
        };
        canvas_draw(9, &frame, std::ptr::null_mut());
        let painted = CALLED.with(|seen| seen.take());
        assert_eq!(painted, ["bind 7", "clear", "bar 100x600", "clear"]);

        canvas_gone(9, std::ptr::null_mut());
        let asked = ASKED.load(Ordering::Relaxed);
        std::thread::sleep(TICK * 3);
        assert_eq!(ASKED.load(Ordering::Relaxed), asked, "and stops when gone");
        canvas_draw(9, &frame, std::ptr::null_mut());
        assert!(CALLED.with(|seen| seen.take()).is_empty());
    }
}
