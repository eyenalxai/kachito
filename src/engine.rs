//! Playback through libmpv, behind a small seam the UI can fake in tests.

use rune::native_surface::{ByteFrame, ColorInfo, Format};
use rune::{NativeSurface, Waker};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::Path;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const VIDEO_WIDTH: i32 = 1280;
pub const VIDEO_HEIGHT: i32 = 720;

/// What the UI needs from a playback backend.
pub trait PlayerEngine: Send {
    fn open(&mut self, path: &Path) -> Result<(), String>;
    fn play(&mut self);
    fn pause(&mut self);
    fn toggle(&mut self);
    fn seek(&mut self, seconds: f64);
    fn set_volume(&mut self, volume: f32);
    fn set_speed(&mut self, speed: f32);
    fn position(&self) -> f64;
    fn duration(&self) -> Option<f64>;
    fn is_playing(&self) -> bool;
    fn error(&self) -> Option<String>;
    fn surface(&self) -> NativeSurface;
    fn frames(&self) -> u64;
    fn set_waker(&self, waker: Waker);
}

pub struct MpvEngine {
    core: Arc<Core>,
    surface: NativeSurface,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MpvEngine {
    /// `headless` keeps audio off so tests do not open a device.
    pub fn new(headless: bool) -> Result<Self, String> {
        let core = Arc::new(Core::new(headless)?);
        let render = unsafe { create_render(core.handle)? };
        let surface = NativeSurface::new();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let core = core.clone();
            let surface = surface.clone();
            let stop = stop.clone();
            let context = RenderContext(render);
            thread::Builder::new()
                .name("kachito-mpv".into())
                .spawn(move || context.run(core, surface, stop))
                .map_err(|error| format!("could not start the playback thread: {error}"))?
        };
        Ok(Self {
            core,
            surface,
            stop,
            thread: Some(thread),
        })
    }
}

impl PlayerEngine for MpvEngine {
    fn open(&mut self, path: &Path) -> Result<(), String> {
        self.surface.clear();
        self.core.open(path)
    }
    fn play(&mut self) {
        self.core.set_paused(false);
    }
    fn pause(&mut self) {
        self.core.set_paused(true);
    }
    fn toggle(&mut self) {
        self.core.toggle();
    }
    fn seek(&mut self, seconds: f64) {
        self.core.seek(seconds);
    }
    fn set_volume(&mut self, volume: f32) {
        self.core.set_volume(volume);
    }
    fn set_speed(&mut self, speed: f32) {
        self.core.set_speed(speed);
    }
    fn position(&self) -> f64 {
        self.core.position()
    }
    fn duration(&self) -> Option<f64> {
        self.core.duration()
    }
    fn is_playing(&self) -> bool {
        self.core.is_playing()
    }
    fn error(&self) -> Option<String> {
        self.core.error()
    }
    fn surface(&self) -> NativeSurface {
        self.surface.clone()
    }
    fn frames(&self) -> u64 {
        self.core.frames.load(Ordering::Relaxed)
    }
    fn set_waker(&self, waker: Waker) {
        self.core.set_waker(waker);
    }
}

impl Drop for MpvEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

struct Core {
    handle: *mut mpv::Handle,
    status: Mutex<Status>,
    waker: Mutex<Option<Waker>>,
    frames: AtomicU64,
}

unsafe impl Send for Core {}
unsafe impl Sync for Core {}

struct Status {
    loaded: bool,
    error: Option<String>,
}

impl Core {
    fn new(headless: bool) -> Result<Self, String> {
        unsafe {
            let handle = mpv::mpv_create();
            if handle.is_null() {
                return Err("libmpv could not create a player".into());
            }
            let options: &[(&str, &str)] = &[
                ("vo", "libmpv"),
                ("idle", "yes"),
                ("keep-open", "no"),
                ("video-sync", "audio"),
                ("video-timing-offset", "0"),
                ("osd-level", "0"),
                ("osc", "no"),
                ("input-default-bindings", "no"),
                ("input-vo-keyboard", "no"),
            ];
            for (name, value) in options {
                if let Err(error) = set_option(handle, name, value) {
                    mpv::mpv_terminate_destroy(handle);
                    return Err(error);
                }
            }
            let hwdec = if headless { "no" } else { "auto-safe" };
            if let Err(error) = set_option(handle, "hwdec", hwdec) {
                mpv::mpv_terminate_destroy(handle);
                return Err(error);
            }
            if headless && let Err(error) = set_option(handle, "ao", "null") {
                mpv::mpv_terminate_destroy(handle);
                return Err(error);
            }
            if let Err(error) = check(mpv::mpv_initialize(handle), "libmpv initialize") {
                mpv::mpv_terminate_destroy(handle);
                return Err(error);
            }
            Ok(Self {
                handle,
                status: Mutex::new(Status {
                    loaded: false,
                    error: None,
                }),
                waker: Mutex::new(None),
                frames: AtomicU64::new(0),
            })
        }
    }

    fn set_waker(&self, waker: Waker) {
        *self.waker.lock().unwrap() = Some(waker);
    }

    fn wake(&self) {
        let waker = self.waker.lock().unwrap().clone();
        if let Some(waker) = waker {
            waker();
        }
    }

    fn set_status(&self, loaded: bool, error: Option<String>) {
        {
            let mut status = self.status.lock().unwrap();
            status.loaded = loaded;
            status.error = error;
        }
        self.wake();
    }

    fn open(&self, path: &Path) -> Result<(), String> {
        let path = path.to_str().ok_or("the file path is not valid UTF-8")?;
        {
            let mut status = self.status.lock().unwrap();
            status.loaded = false;
            status.error = None;
        }
        self.command(&["loadfile", path])
    }

    fn set_paused(&self, paused: bool) {
        let _ = self.set_flag("pause", paused);
    }

    fn toggle(&self) {
        let paused = self.get_flag("pause").unwrap_or(false);
        self.set_paused(!paused);
    }

    fn seek(&self, seconds: f64) {
        if !seconds.is_finite() {
            return;
        }
        let seconds = seconds.max(0.0);
        let _ = self.command(&["seek", &format!("{seconds:.3}"), "absolute+exact"]);
    }

    fn set_volume(&self, volume: f32) {
        let _ = self.set_double("volume", f64::from(volume.clamp(0.0, 1.0)) * 100.0);
    }

    fn set_speed(&self, speed: f32) {
        let _ = self.set_double("speed", f64::from(speed.clamp(0.1, 4.0)));
    }

    fn position(&self) -> f64 {
        self.get_double("time-pos").unwrap_or(0.0).max(0.0)
    }

    fn duration(&self) -> Option<f64> {
        self.get_double("duration")
            .filter(|duration| duration.is_finite() && *duration > 0.0)
    }

    fn is_playing(&self) -> bool {
        let (loaded, failed) = {
            let status = self.status.lock().unwrap();
            (status.loaded, status.error.is_some())
        };
        loaded && !failed && !self.get_flag("pause").unwrap_or(true)
    }

    fn error(&self) -> Option<String> {
        self.status.lock().unwrap().error.clone()
    }

    fn drain_events(&self) {
        loop {
            let event = unsafe { mpv::mpv_wait_event(self.handle, 0.0) };
            if event.is_null() {
                break;
            }
            let id = unsafe { (*event).event_id };
            if id == mpv::EVENT_NONE {
                break;
            }
            match id {
                mpv::EVENT_FILE_LOADED => self.set_status(true, None),
                mpv::EVENT_END_FILE => {
                    let data = unsafe { (*event).data };
                    if data.is_null() {
                        self.set_status(false, Some("the file ended unexpectedly".into()));
                    } else {
                        let end = unsafe { &*(data as *const mpv::EventEndFile) };
                        if end.reason == mpv::END_FILE_REASON_ERROR {
                            self.set_status(false, Some(error_string(end.error)));
                        } else {
                            self.set_status(false, None);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn command(&self, args: &[&str]) -> Result<(), String> {
        let strings: Vec<CString> = args.iter().map(|arg| CString::new(*arg).unwrap()).collect();
        let mut pointers: Vec<*const c_char> = strings.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(ptr::null());
        check(
            unsafe { mpv::mpv_command(self.handle, pointers.as_ptr()) },
            args[0],
        )
    }

    fn set_double(&self, name: &str, value: f64) -> Result<(), String> {
        let name = CString::new(name).unwrap();
        let mut value = value;
        check(
            unsafe {
                mpv::mpv_set_property(
                    self.handle,
                    name.as_ptr(),
                    mpv::FORMAT_DOUBLE,
                    &mut value as *mut f64 as *mut c_void,
                )
            },
            "set property",
        )
    }

    fn set_flag(&self, name: &str, value: bool) -> Result<(), String> {
        let name = CString::new(name).unwrap();
        let mut value: c_int = c_int::from(value);
        check(
            unsafe {
                mpv::mpv_set_property(
                    self.handle,
                    name.as_ptr(),
                    mpv::FORMAT_FLAG,
                    &mut value as *mut c_int as *mut c_void,
                )
            },
            "set property",
        )
    }

    fn get_double(&self, name: &str) -> Option<f64> {
        let name = CString::new(name).unwrap();
        let mut value = 0.0f64;
        let result = unsafe {
            mpv::mpv_get_property(
                self.handle,
                name.as_ptr(),
                mpv::FORMAT_DOUBLE,
                &mut value as *mut f64 as *mut c_void,
            )
        };
        (result >= 0).then_some(value)
    }

    fn get_flag(&self, name: &str) -> Option<bool> {
        let name = CString::new(name).unwrap();
        let mut value: c_int = 0;
        let result = unsafe {
            mpv::mpv_get_property(
                self.handle,
                name.as_ptr(),
                mpv::FORMAT_FLAG,
                &mut value as *mut c_int as *mut c_void,
            )
        };
        (result >= 0).then_some(value != 0)
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        unsafe { mpv::mpv_terminate_destroy(self.handle) };
    }
}

struct RenderContext(*mut mpv::Render);

unsafe impl Send for RenderContext {}

impl Drop for RenderContext {
    fn drop(&mut self) {
        unsafe { mpv::mpv_render_context_free(self.0) };
    }
}

impl RenderContext {
    fn run(&self, core: Arc<Core>, surface: NativeSurface, stop: Arc<AtomicBool>) {
        let size = (VIDEO_WIDTH * VIDEO_HEIGHT * 4) as usize;
        let mut buffer = vec![0u8; size];
        let mut id = 0u64;
        while !stop.load(Ordering::Relaxed) {
            core.drain_events();
            let flags = unsafe { mpv::mpv_render_context_update(self.0) };
            if flags & mpv::UPDATE_FRAME != 0 {
                if self.render_frame(&mut buffer).is_ok() {
                    opaque(&mut buffer);
                    let bytes = std::mem::take(&mut buffer);
                    let _ = surface.push_bytes(frame(bytes), id);
                    id += 1;
                    core.frames.fetch_add(1, Ordering::Relaxed);
                    buffer = vec![0u8; size];
                }
                unsafe { mpv::mpv_render_context_report_swap(self.0) };
            } else {
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    fn render_frame(&self, buffer: &mut [u8]) -> Result<(), String> {
        let mut size = [VIDEO_WIDTH, VIDEO_HEIGHT];
        let mut stride = (VIDEO_WIDTH * 4) as usize;
        let block: c_int = 0;
        let format = b"bgr0\0";
        let mut params = [
            mpv::Param {
                type_: mpv::PARAM_SW_SIZE,
                data: size.as_mut_ptr() as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_SW_FORMAT,
                data: format.as_ptr() as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_SW_STRIDE,
                data: &mut stride as *mut usize as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_SW_POINTER,
                data: buffer.as_mut_ptr() as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_BLOCK_FOR_TARGET_TIME,
                data: &block as *const c_int as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        check(
            unsafe { mpv::mpv_render_context_render(self.0, params.as_mut_ptr()) },
            "libmpv render",
        )
    }
}

fn opaque(buffer: &mut [u8]) {
    for pixel in buffer.as_chunks_mut::<4>().0 {
        pixel[3] = 255;
    }
}

fn frame(bytes: Vec<u8>) -> ByteFrame {
    ByteFrame {
        width: VIDEO_WIDTH as u32,
        height: VIDEO_HEIGHT as u32,
        format: Format::Bgra,
        color: ColorInfo::default(),
        planes: [bytes, Vec::new()],
    }
}

unsafe fn create_render(handle: *mut mpv::Handle) -> Result<*mut mpv::Render, String> {
    let api = CString::new("sw").unwrap();
    let mut params = [
        mpv::Param {
            type_: mpv::PARAM_API_TYPE,
            data: api.as_ptr() as *mut c_void,
        },
        mpv::Param {
            type_: mpv::PARAM_INVALID,
            data: ptr::null_mut(),
        },
    ];
    let mut render = ptr::null_mut();
    check(
        unsafe { mpv::mpv_render_context_create(&mut render, handle, params.as_mut_ptr()) },
        "libmpv render context",
    )?;
    Ok(render)
}

unsafe fn set_option(ctx: *mut mpv::Handle, name: &str, value: &str) -> Result<(), String> {
    let name = CString::new(name).unwrap();
    let value = CString::new(value).unwrap();
    check(
        unsafe { mpv::mpv_set_option_string(ctx, name.as_ptr(), value.as_ptr()) },
        "set option",
    )
}

fn check(code: c_int, what: &str) -> Result<(), String> {
    if code >= 0 {
        return Ok(());
    }
    Err(format!("{what}: {}", error_string(code)))
}

fn error_string(code: c_int) -> String {
    unsafe { CStr::from_ptr(mpv::mpv_error_string(code)) }
        .to_string_lossy()
        .into_owned()
}

mod mpv {
    use super::*;

    #[repr(C)]
    pub struct Handle {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct Render {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct Event {
        pub event_id: c_int,
        pub error: c_int,
        pub reply_userdata: u64,
        pub data: *mut c_void,
    }
    #[repr(C)]
    pub struct EventEndFile {
        pub reason: c_int,
        pub error: c_int,
    }
    #[repr(C)]
    pub struct Param {
        pub type_: c_int,
        pub data: *mut c_void,
    }

    pub const EVENT_NONE: c_int = 0;
    pub const EVENT_END_FILE: c_int = 7;
    pub const EVENT_FILE_LOADED: c_int = 8;
    pub const END_FILE_REASON_ERROR: c_int = 4;

    pub const FORMAT_FLAG: c_int = 3;
    pub const FORMAT_DOUBLE: c_int = 5;

    pub const PARAM_API_TYPE: c_int = 1;
    pub const PARAM_BLOCK_FOR_TARGET_TIME: c_int = 12;
    pub const PARAM_SW_SIZE: c_int = 17;
    pub const PARAM_SW_FORMAT: c_int = 18;
    pub const PARAM_SW_STRIDE: c_int = 19;
    pub const PARAM_SW_POINTER: c_int = 20;
    pub const PARAM_INVALID: c_int = 0;

    pub const UPDATE_FRAME: u64 = 1;

    #[link(name = "mpv")]
    unsafe extern "C" {
        pub fn mpv_create() -> *mut Handle;
        pub fn mpv_set_option_string(
            ctx: *mut Handle,
            name: *const c_char,
            data: *const c_char,
        ) -> c_int;
        pub fn mpv_initialize(ctx: *mut Handle) -> c_int;
        pub fn mpv_command(ctx: *mut Handle, args: *const *const c_char) -> c_int;
        pub fn mpv_set_property(
            ctx: *mut Handle,
            name: *const c_char,
            format: c_int,
            data: *mut c_void,
        ) -> c_int;
        pub fn mpv_get_property(
            ctx: *mut Handle,
            name: *const c_char,
            format: c_int,
            data: *mut c_void,
        ) -> c_int;
        pub fn mpv_wait_event(ctx: *mut Handle, timeout: f64) -> *mut Event;
        pub fn mpv_error_string(error: c_int) -> *const c_char;
        pub fn mpv_terminate_destroy(ctx: *mut Handle);
        pub fn mpv_render_context_create(
            res: *mut *mut Render,
            ctx: *mut Handle,
            params: *mut Param,
        ) -> c_int;
        pub fn mpv_render_context_update(ctx: *mut Render) -> u64;
        pub fn mpv_render_context_render(ctx: *mut Render, params: *mut Param) -> c_int;
        pub fn mpv_render_context_report_swap(ctx: *mut Render);
        pub fn mpv_render_context_free(ctx: *mut Render);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn fixture() -> std::path::PathBuf {
        std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/test/rgbw.mp4"))
    }

    #[test]
    fn opens_a_file_and_pushes_a_frame() {
        let mut engine = MpvEngine::new(true).unwrap();
        engine.open(&fixture()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.frames() == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            engine.frames() > 0,
            "libmpv pushed no frame: {:?}",
            engine.error()
        );
        assert!(
            engine.surface().last_error().is_none(),
            "{:?}",
            engine.surface().last_error()
        );
    }

    #[test]
    fn a_missing_file_sets_an_error() {
        let mut engine = MpvEngine::new(true).unwrap();
        let _ = engine.open(Path::new("/definitely/not/here.mkv"));
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.error().is_none() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            engine.error().is_some(),
            "a missing file must report an error"
        );
    }
}
