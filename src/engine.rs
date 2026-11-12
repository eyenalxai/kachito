//! Playback through libmpv, behind a small seam the UI can fake in tests.

use rune::native_surface::{ByteFrame, ColorInfo, Format};
use rune::{NativeSurface, Waker};
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(test)]
use std::sync::atomic::{AtomicU64, AtomicUsize};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const VIDEO_WIDTH: i32 = 1280;
pub const VIDEO_HEIGHT: i32 = 720;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackKind {
    Audio,
    Subtitle,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub id: i64,
    pub kind: TrackKind,
    pub title: Option<String>,
    pub language: Option<String>,
    pub selected: bool,
}

impl Track {
    pub fn label(&self) -> String {
        if let Some(title) = &self.title {
            return title.clone();
        }
        if let Some(language) = &self.language {
            return language.to_ascii_uppercase();
        }
        format!("Track {}", self.id)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaybackState {
    Empty,
    Loading,
    Playing,
    Paused,
    Ended,
    Failed(String),
}

impl PlaybackState {
    pub fn is_loaded(&self) -> bool {
        matches!(self, Self::Playing | Self::Paused | Self::Ended)
    }
}

/// What the UI needs from a playback backend.
pub trait PlayerEngine: Send {
    fn open(&mut self, path: &Path) -> Result<(), String>;
    fn toggle(&mut self);
    fn seek(&mut self, seconds: f64);
    fn set_volume(&mut self, volume: f32);
    fn set_speed(&mut self, speed: f32);
    fn position(&self) -> f64;
    fn duration(&self) -> Option<f64>;
    fn frame_duration(&self) -> Option<f64>;
    fn state(&self) -> PlaybackState;
    fn current_path(&self) -> Option<PathBuf>;
    fn surface(&self) -> NativeSurface;
    fn tracks(&self) -> Result<Vec<Track>, String>;
    fn select_audio(&mut self, id: i64) -> Result<(), String>;
    fn select_subtitle(&mut self, id: Option<i64>) -> Result<(), String>;
    /// Installs the app waker for playback state changes and surface pushes.
    fn set_waker(&self, waker: Waker);
}

pub struct MpvEngine {
    core: Arc<Core>,
    surface: NativeSurface,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl MpvEngine {
    #[cfg(test)]
    fn frames(&self) -> u64 {
        self.core.frames.load(Ordering::Relaxed)
    }

    /// `headless` keeps audio off so tests do not open a device.
    pub fn new(headless: bool) -> Result<Self, String> {
        let core = Arc::new(Core::new(headless)?);
        let surface = NativeSurface::new();
        let stop = Arc::new(AtomicBool::new(false));
        let (ready, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = {
            let core = core.clone();
            let surface = surface.clone();
            let stop = stop.clone();
            thread::Builder::new()
                .name("kachito-mpv".into())
                .spawn(move || {
                    let render = match unsafe { create_render(core.handle) } {
                        Ok(render) => {
                            let _ = ready.send(Ok(()));
                            render
                        }
                        Err(error) => {
                            let _ = ready.send(Err(error));
                            return;
                        }
                    };
                    RenderContext(render).run(core, surface, stop);
                })
                .map_err(|error| format!("could not start the playback thread: {error}"))?
        };
        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                core,
                surface,
                stop,
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                let _ = thread.join();
                Err("the playback thread stopped before it was ready".into())
            }
        }
    }
}

impl PlayerEngine for MpvEngine {
    fn open(&mut self, path: &Path) -> Result<(), String> {
        self.core.open(path)
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
    fn frame_duration(&self) -> Option<f64> {
        self.core.frame_duration()
    }
    fn state(&self) -> PlaybackState {
        self.core.state()
    }
    fn current_path(&self) -> Option<PathBuf> {
        self.core.current_path()
    }
    fn surface(&self) -> NativeSurface {
        self.surface.clone()
    }
    fn tracks(&self) -> Result<Vec<Track>, String> {
        self.core.tracks()
    }
    fn select_audio(&mut self, id: i64) -> Result<(), String> {
        self.core.select_audio(id)
    }
    fn select_subtitle(&mut self, id: Option<i64>) -> Result<(), String> {
        self.core.select_subtitle(id)
    }
    fn set_waker(&self, waker: Waker) {
        self.core.set_waker(waker.clone());
        self.surface.set_waker(waker);
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
    state: Mutex<PlaybackState>,
    waker: Mutex<Option<Waker>>,
    #[cfg(test)]
    frames: AtomicU64,
}

unsafe impl Send for Core {}
unsafe impl Sync for Core {}

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
                ("keep-open", "yes"),
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
                state: Mutex::new(PlaybackState::Empty),
                waker: Mutex::new(None),
                #[cfg(test)]
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

    fn set_state(&self, state: PlaybackState) {
        *self.state.lock().unwrap() = state;
        self.wake();
    }

    fn open(&self, path: &Path) -> Result<(), String> {
        let path = path.to_str().ok_or("the file path is not valid UTF-8")?;
        self.set_state(PlaybackState::Loading);
        match self.command(&["loadfile", path]) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.set_state(PlaybackState::Failed(error.clone()));
                Err(error)
            }
        }
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.get_string("path").map(PathBuf::from)
    }

    fn state(&self) -> PlaybackState {
        self.state.lock().unwrap().clone()
    }

    fn set_paused(&self, paused: bool) {
        if !paused && matches!(*self.state.lock().unwrap(), PlaybackState::Ended) {
            self.seek(0.0);
        }
        let _ = self.set_flag("pause", paused);
        self.set_state(if paused {
            PlaybackState::Paused
        } else {
            PlaybackState::Playing
        });
    }

    fn toggle(&self) {
        match self.state() {
            PlaybackState::Playing => self.set_paused(true),
            PlaybackState::Paused | PlaybackState::Ended => self.set_paused(false),
            _ => {}
        }
    }

    fn seek(&self, seconds: f64) {
        if !seconds.is_finite() {
            return;
        }
        let seconds = seconds.max(0.0);
        let _ = self.command(&["seek", &format!("{seconds:.3}"), "absolute+exact"]);
        if matches!(self.state(), PlaybackState::Ended) {
            self.set_state(PlaybackState::Paused);
        }
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

    fn frame_duration(&self) -> Option<f64> {
        self.get_double("container-fps")
            .filter(|fps| fps.is_finite() && *fps > 0.0)
            .map(|fps| 1.0 / fps)
    }

    fn tracks(&self) -> Result<Vec<Track>, String> {
        let mut node = mpv::Node {
            data: mpv::NodeData { int64: 0 },
            format: 0,
        };
        let name = CString::new("track-list").unwrap();
        let result = unsafe {
            mpv::mpv_get_property(
                self.handle,
                name.as_ptr(),
                mpv::FORMAT_NODE,
                &mut node as *mut mpv::Node as *mut c_void,
            )
        };
        if result < 0 {
            return Err(error_string(result));
        }
        let mut tracks = Vec::new();
        unsafe {
            if node.format == mpv::FORMAT_NODE_ARRAY {
                let list = &*node.data.list;
                for index in 0..list.num.max(0) as isize {
                    let item = &*list.values.offset(index);
                    if item.format != mpv::FORMAT_NODE_MAP {
                        continue;
                    }
                    let map = &*item.data.list;
                    let mut id = None;
                    let mut kind = None;
                    let mut title = None;
                    let mut language = None;
                    let mut selected = false;
                    for pair in 0..map.num.max(0) as isize {
                        let key = CStr::from_ptr(*map.keys.offset(pair)).to_string_lossy();
                        let value = &*map.values.offset(pair);
                        match key.as_ref() {
                            "id" if value.format == mpv::FORMAT_INT64 => {
                                id = Some(value.data.int64)
                            }
                            "type" if value.format == mpv::FORMAT_STRING => {
                                kind = Some(
                                    CStr::from_ptr(value.data.string)
                                        .to_string_lossy()
                                        .into_owned(),
                                )
                            }
                            "title" if value.format == mpv::FORMAT_STRING => {
                                let text = CStr::from_ptr(value.data.string)
                                    .to_string_lossy()
                                    .into_owned();
                                if !text.is_empty() {
                                    title = Some(text);
                                }
                            }
                            "lang" if value.format == mpv::FORMAT_STRING => {
                                let text = CStr::from_ptr(value.data.string)
                                    .to_string_lossy()
                                    .into_owned();
                                if !text.is_empty() {
                                    language = Some(text);
                                }
                            }
                            "selected" if value.format == mpv::FORMAT_FLAG => {
                                selected = value.data.flag != 0
                            }
                            _ => {}
                        }
                    }
                    let kind = match kind.as_deref() {
                        Some("audio") => TrackKind::Audio,
                        Some("sub") => TrackKind::Subtitle,
                        _ => continue,
                    };
                    if let Some(id) = id {
                        tracks.push(Track {
                            id,
                            kind,
                            title,
                            language,
                            selected,
                        });
                    }
                }
            }
            mpv::mpv_free_node_contents(&mut node);
        }
        Ok(tracks)
    }

    fn select_audio(&self, id: i64) -> Result<(), String> {
        self.set_int("aid", id)
    }

    fn select_subtitle(&self, id: Option<i64>) -> Result<(), String> {
        match id {
            Some(id) => self.set_int("sid", id),
            None => self.command(&["set", "sid", "no"]),
        }
    }

    fn set_int(&self, name: &str, value: i64) -> Result<(), String> {
        let name = CString::new(name).unwrap();
        let mut value = value;
        check(
            unsafe {
                mpv::mpv_set_property(
                    self.handle,
                    name.as_ptr(),
                    mpv::FORMAT_INT64,
                    &mut value as *mut i64 as *mut c_void,
                )
            },
            "set property",
        )
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
                mpv::EVENT_FILE_LOADED => {
                    let paused = self.get_flag("pause").unwrap_or(false);
                    self.set_state(if paused {
                        PlaybackState::Paused
                    } else {
                        PlaybackState::Playing
                    });
                }
                mpv::EVENT_END_FILE => {
                    let data = unsafe { (*event).data };
                    if data.is_null() {
                        self.set_state(PlaybackState::Failed("the file ended unexpectedly".into()));
                    } else {
                        let end = unsafe { &*(data as *const mpv::EventEndFile) };
                        if end.reason == mpv::END_FILE_REASON_ERROR {
                            self.set_state(PlaybackState::Failed(error_string(end.error)));
                        } else if end.reason == mpv::END_FILE_REASON_EOF {
                            self.set_state(PlaybackState::Ended);
                        } else {
                            self.set_state(PlaybackState::Empty);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn command(&self, args: &[&str]) -> Result<(), String> {
        let strings: Vec<CString> = args
            .iter()
            .map(|arg| CString::new(*arg))
            .collect::<Result<_, _>>()
            .map_err(|_| "an argument contains a NUL byte".to_string())?;
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

    fn get_string(&self, name: &str) -> Option<String> {
        let name = CString::new(name).unwrap();
        let mut value: *mut c_char = ptr::null_mut();
        let result = unsafe {
            mpv::mpv_get_property(
                self.handle,
                name.as_ptr(),
                mpv::FORMAT_STRING,
                &mut value as *mut *mut c_char as *mut c_void,
            )
        };
        if result < 0 || value.is_null() {
            return None;
        }
        let text = unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() };
        unsafe { mpv::mpv_free(value as *mut c_void) };
        Some(text)
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
                    if surface.push_bytes(frame(bytes), id).is_ok() {
                        id += 1;
                        #[cfg(test)]
                        core.frames.fetch_add(1, Ordering::Relaxed);
                    }
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
    #[repr(C)]
    pub union NodeData {
        pub string: *mut c_char,
        pub flag: c_int,
        pub int64: i64,
        pub double_: f64,
        pub list: *mut NodeList,
        pub ba: *mut c_void,
    }
    #[repr(C)]
    pub struct Node {
        pub data: NodeData,
        pub format: c_int,
    }
    #[repr(C)]
    pub struct NodeList {
        pub num: c_int,
        pub values: *mut Node,
        pub keys: *mut *mut c_char,
    }

    pub const EVENT_NONE: c_int = 0;
    pub const EVENT_END_FILE: c_int = 7;
    pub const EVENT_FILE_LOADED: c_int = 8;
    pub const END_FILE_REASON_EOF: c_int = 0;
    pub const END_FILE_REASON_ERROR: c_int = 4;

    pub const FORMAT_STRING: c_int = 1;
    pub const FORMAT_FLAG: c_int = 3;
    pub const FORMAT_INT64: c_int = 4;
    pub const FORMAT_DOUBLE: c_int = 5;
    pub const FORMAT_NODE: c_int = 6;
    pub const FORMAT_NODE_ARRAY: c_int = 7;
    pub const FORMAT_NODE_MAP: c_int = 8;

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
        pub fn mpv_free(data: *mut c_void);
        pub fn mpv_free_node_contents(node: *mut Node);
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

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/test")
            .join(name)
    }

    fn wait_for_frame_count(engine: &MpvEngine, target: u64) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.frames() < target && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            engine.frames() >= target,
            "libmpv pushed {} frames, wanted {target}: {:?}",
            engine.frames(),
            engine.state()
        );
    }

    fn wait_for_frame(engine: &MpvEngine) {
        wait_for_frame_count(engine, 1);
    }

    fn wait_for_error(engine: &MpvEngine) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(engine.state(), PlaybackState::Failed(_)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            matches!(engine.state(), PlaybackState::Failed(_)),
            "a missing file must report an error"
        );
    }

    fn wait_for_tracks(engine: &MpvEngine) -> Vec<Track> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut tracks = Vec::new();
        while Instant::now() < deadline {
            if let Ok(found) = engine.tracks()
                && !found.is_empty()
            {
                tracks = found;
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !tracks.is_empty(),
            "libmpv listed no tracks: {:?}",
            engine.state()
        );
        tracks
    }

    #[test]
    fn opens_an_mp4_file_and_pushes_a_frame() {
        let mut engine = MpvEngine::new(true).unwrap();
        engine.open(&fixture("rgbw.mp4")).unwrap();
        wait_for_frame(&engine);
        assert!(
            engine.surface().last_error().is_none(),
            "{:?}",
            engine.surface().last_error()
        );
    }

    #[test]
    fn opens_an_mkv_file_and_pushes_a_frame() {
        let mut engine = MpvEngine::new(true).unwrap();
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);
        assert!(
            engine.surface().last_error().is_none(),
            "{:?}",
            engine.surface().last_error()
        );
    }

    #[test]
    fn the_engine_reports_a_loaded_state_after_a_frame() {
        let mut engine = MpvEngine::new(true).unwrap();
        assert_eq!(engine.state(), PlaybackState::Empty);
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);
        assert!(engine.state().is_loaded(), "{:?}", engine.state());
    }

    #[test]
    fn the_stored_playback_state_agrees_across_reads_and_toggles() {
        let mut engine = MpvEngine::new(true).unwrap();
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);
        let first = engine.state();
        let second = engine.state();
        assert_eq!(first, second, "two reads of the playback state must agree");
        assert!(
            matches!(first, PlaybackState::Playing | PlaybackState::Paused),
            "{first:?}"
        );
        engine.toggle();
        assert_eq!(
            engine.state(),
            PlaybackState::Paused,
            "a pause toggle stores Paused"
        );
        engine.toggle();
        assert_eq!(
            engine.state(),
            PlaybackState::Playing,
            "a play toggle stores Playing"
        );
    }

    #[test]
    fn the_engine_reports_the_frame_duration() {
        let mut engine = MpvEngine::new(true).unwrap();
        engine.open(&fixture("tracks.mkv")).unwrap();
        wait_for_frame(&engine);
        let frame_duration = engine.frame_duration().expect("a source frame duration");
        assert!(
            (frame_duration - 1.0 / 25.0).abs() < 1e-6,
            "{frame_duration}"
        );
    }

    #[test]
    fn a_seek_wakes_the_surface_without_a_state_change() {
        let mut engine = MpvEngine::new(true).unwrap();
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        engine.set_waker(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);

        // Pause so the next frame is the seek's, and let an in-flight frame land.
        engine.toggle();
        thread::sleep(Duration::from_millis(100));
        let state_before = engine.state();
        let frames = engine.frames();
        let wakes_before = wakes.load(Ordering::Relaxed);

        engine.seek(0.5);
        wait_for_frame_count(&engine, frames + 1);

        assert_eq!(
            engine.state(),
            state_before,
            "the seek must not change the core state"
        );
        assert!(
            wakes.load(Ordering::Relaxed) > wakes_before,
            "the surface push after a seek did not wake the app"
        );
    }

    #[test]
    fn a_missing_file_sets_an_error() {
        let mut engine = MpvEngine::new(true).unwrap();
        let _ = engine.open(Path::new("/definitely/not/here.mkv"));
        wait_for_error(&engine);
    }

    #[test]
    fn lists_and_selects_audio_and_subtitle_tracks() {
        let mut engine = MpvEngine::new(true).unwrap();
        engine.open(&fixture("tracks.mkv")).unwrap();
        let tracks = wait_for_tracks(&engine);
        let audio: Vec<&Track> = tracks
            .iter()
            .filter(|track| track.kind == TrackKind::Audio)
            .collect();
        assert!(audio.len() >= 2, "{tracks:?}");
        assert!(audio.iter().any(|track| track.label() == "Main"));
        assert!(audio.iter().any(|track| track.label() == "Commentary"));
        let subtitle = tracks
            .iter()
            .find(|track| track.kind == TrackKind::Subtitle)
            .expect("a subtitle track");
        engine.select_audio(audio[1].id).unwrap();
        engine.select_subtitle(Some(subtitle.id)).unwrap();
        engine.select_subtitle(None).unwrap();
    }
}
