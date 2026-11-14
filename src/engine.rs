//! Playback through libmpv, behind a small seam the UI can fake in tests.

use crate::log::RunLog;
use rune::native_surface::{ByteFrame, ColorInfo, Format};
use rune::{NativeSurface, Waker};
use std::ffi::{CStr, CString, c_char, c_int, c_ulong, c_void};
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

/// The libmpv client API major this build is written against.
pub const CLIENT_API_MAJOR: u32 = 2;

/// Accepts a libmpv client API version only when its major matches this build.
///
/// `mpv_client_api_version` packs the major in the high 16 bits and the minor in
/// the low 16 bits.
pub fn check_client_api_version(version: u64) -> Result<(), String> {
    let found_major = (version >> 16) as u32;
    if found_major == CLIENT_API_MAJOR {
        return Ok(());
    }
    let minor = (version & 0xffff) as u32;
    Err(format!(
        "libmpv client API {found_major}.{minor} is not supported: found major {found_major}, expected major {CLIENT_API_MAJOR}. Install a libmpv that provides client API {CLIENT_API_MAJOR}.x."
    ))
}

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
    Ready,
    Playing,
    Paused,
    Ended,
    Failed(String),
}

impl PlaybackState {
    pub fn is_loaded(&self) -> bool {
        matches!(
            self,
            Self::Ready | Self::Playing | Self::Paused | Self::Ended
        )
    }
}

/// A command or property write whose arguments name its log line and failure.
enum Action<'a> {
    Command(&'a [&'a [u8]]),
    Property { name: &'a str, value: &'a str },
}

impl Action<'_> {
    fn describe(&self) -> String {
        match self {
            Self::Command(args) => lossy_text(args),
            Self::Property { name, value } => format!("{name} = {value}"),
        }
    }
}

fn lossy_text(args: &[&[u8]]) -> String {
    args.iter()
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// What the UI needs from a playback backend.
pub trait PlayerEngine: Send {
    fn open(&mut self, path: &Path) -> Result<(), String>;
    fn toggle(&mut self);
    fn seek(&mut self, seconds: f64);
    /// A transient seek for a scrub preview. The session coalesces these.
    fn seek_preview(&mut self, seconds: f64);
    /// True while the player is seeking or restarting playback.
    fn is_seeking(&self) -> bool;
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
    /// Takes the most recent rejected command, once, for the one-notice rule.
    fn take_command_failure(&mut self) -> Option<String>;
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

    #[cfg(test)]
    fn last_seek_flags(&self) -> Option<String> {
        self.core.last_seek_flags.lock().unwrap().clone()
    }

    /// `headless` keeps audio off so tests do not open a device.
    pub fn new(headless: bool, log: Arc<RunLog>) -> Result<Self, String> {
        let core = Arc::new(Core::new(headless, log)?);
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
    fn seek_preview(&mut self, seconds: f64) {
        self.core.seek_preview(seconds);
    }
    fn is_seeking(&self) -> bool {
        self.core.is_seeking()
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
    fn take_command_failure(&mut self) -> Option<String> {
        self.core.failure.lock().unwrap().take()
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
    log: Arc<RunLog>,
    state: Mutex<PlaybackState>,
    ready_reported: AtomicBool,
    end_reported: AtomicBool,
    failure: Mutex<Option<String>>,
    waker: Mutex<Option<Waker>>,
    #[cfg(test)]
    frames: AtomicU64,
    #[cfg(test)]
    last_seek_flags: Mutex<Option<String>>,
}

unsafe impl Send for Core {}
unsafe impl Sync for Core {}

impl Core {
    fn new(headless: bool, log: Arc<RunLog>) -> Result<Self, String> {
        unsafe {
            check_client_api_version(u64::from(mpv::mpv_client_api_version()))?;
            let handle = mpv::mpv_create();
            if handle.is_null() {
                log.warn("mpv could not create a player");
                return Err("libmpv could not create a player".into());
            }
            let mut options: Vec<(&str, &str)> = vec![
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
            let hwdec = if headless { "no" } else { "auto-safe" };
            options.push(("hwdec", hwdec));
            if headless {
                options.push(("ao", "null"));
            }
            for (name, value) in &options {
                if let Err(error) = set_option(handle, name, value) {
                    log.warn(format!("option failed: {name}={value}: {error}"));
                    mpv::mpv_terminate_destroy(handle);
                    return Err(error);
                }
            }
            let summary = options
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<_>>()
                .join(" ");
            log.info(format!("options: {summary}"));
            // The handle accepts the log request before initialization, so
            // warnings emitted while the core starts are not lost.
            let warn = CString::new("warn").unwrap();
            if let Err(error) = check(
                mpv::mpv_request_log_messages(handle, warn.as_ptr()),
                "request log messages",
            ) {
                log.warn(format!("could not request mpv log messages: {error}"));
            }
            if let Err(error) = check(mpv::mpv_initialize(handle), "libmpv initialize") {
                log.warn(format!("startup failed: {error}"));
                mpv::mpv_terminate_destroy(handle);
                return Err(error);
            }
            let core = Self {
                handle,
                log,
                state: Mutex::new(PlaybackState::Empty),
                ready_reported: AtomicBool::new(false),
                end_reported: AtomicBool::new(false),
                failure: Mutex::new(None),
                waker: Mutex::new(None),
                #[cfg(test)]
                frames: AtomicU64::new(0),
                #[cfg(test)]
                last_seek_flags: Mutex::new(None),
            };
            let version = core
                .get_string("mpv-version")
                .unwrap_or_else(|| "unknown".into());
            core.log.info(format!("mpv version: {version}"));
            let api = mpv::mpv_client_api_version();
            core.log
                .info(format!("mpv client API: {}.{}", api >> 16, api & 0xffff));
            Ok(core)
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
        if matches!(&state, PlaybackState::Ready) {
            self.ready_reported.store(false, Ordering::Relaxed);
        }
        *self.state.lock().unwrap() = state;
        self.wake();
    }

    fn open(&self, path: &Path) -> Result<(), String> {
        let bytes = path_bytes(path);
        self.log.info(format!("load: {}", path.to_string_lossy()));
        self.end_reported.store(false, Ordering::Relaxed);
        self.set_state(PlaybackState::Loading);
        match self.command_bytes(&[b"loadfile".as_slice(), bytes.as_slice()]) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.set_state(PlaybackState::Failed(error.clone()));
                Err(error)
            }
        }
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.get_path("path")
    }

    fn state(&self) -> PlaybackState {
        let state = self.state.lock().unwrap().clone();
        match state {
            PlaybackState::Ready => {
                if !self.ready_reported.swap(true, Ordering::Relaxed) {
                    PlaybackState::Ready
                } else if self.get_flag("pause").unwrap_or(true) {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Playing
                }
            }
            PlaybackState::Playing | PlaybackState::Paused => {
                if self.get_flag("pause").unwrap_or(true) {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Playing
                }
            }
            other => other,
        }
    }

    fn set_paused(&self, paused: bool) {
        let ended = matches!(*self.state.lock().unwrap(), PlaybackState::Ended);
        if !paused && ended {
            self.seek(0.0);
        }
        let _ = self.set_flag("pause", paused);
    }

    fn toggle(&self) {
        if matches!(*self.state.lock().unwrap(), PlaybackState::Ended) {
            self.set_paused(false);
            return;
        }
        let paused = self.get_flag("pause").unwrap_or(false);
        self.set_paused(!paused);
    }

    fn seek(&self, seconds: f64) {
        if !seconds.is_finite() {
            return;
        }
        self.seek_exact(seconds);
        let mut state = self.state.lock().unwrap();
        if matches!(*state, PlaybackState::Ended) {
            *state = PlaybackState::Paused;
        }
    }

    fn seek_preview(&self, seconds: f64) {
        if !seconds.is_finite() {
            return;
        }
        self.seek_exact(seconds);
    }

    fn seek_exact(&self, seconds: f64) {
        let seconds = seconds.max(0.0);
        let flags = "absolute+exact";
        let _ = self.command(&["seek", &format!("{seconds:.3}"), flags]);
        #[cfg(test)]
        {
            *self.last_seek_flags.lock().unwrap() = Some(flags.to_string());
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

    fn is_seeking(&self) -> bool {
        self.get_flag("seeking").unwrap_or(false)
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
        let mut value = value;
        let text = value.to_string();
        self.record_property(
            name,
            &text,
            mpv::FORMAT_INT64,
            &mut value as *mut i64 as *mut c_void,
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
                mpv::EVENT_LOG_MESSAGE => {
                    let data = unsafe { (*event).data };
                    if data.is_null() {
                        continue;
                    }
                    let message = unsafe { &*(data as *const mpv::EventLogMessage) };
                    if message.log_level > mpv::LOG_LEVEL_WARN {
                        continue;
                    }
                    self.log.record_mpv_message(
                        &text_at(message.level),
                        &text_at(message.prefix),
                        &text_at(message.text),
                    );
                }
                mpv::EVENT_FILE_LOADED => self.set_state(PlaybackState::Ready),
                mpv::EVENT_END_FILE => {
                    let data = unsafe { (*event).data };
                    if data.is_null() {
                        self.log_end_of_file("unknown");
                        self.set_state(PlaybackState::Failed("the file ended unexpectedly".into()));
                    } else {
                        let end = unsafe { &*(data as *const mpv::EventEndFile) };
                        let reason = end_reason(end.reason);
                        if end.reason == mpv::END_FILE_REASON_ERROR {
                            self.log_end_of_file(&format!("{reason}: {}", error_string(end.error)));
                        } else {
                            self.log_end_of_file(&reason);
                        }
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
        self.observe_natural_eof();
    }

    /// With keep-open, a natural end pauses at the last frame and emits no
    /// END_FILE event, so the eof-reached property is the only signal.
    fn observe_natural_eof(&self) {
        let state = self.state.lock().unwrap().clone();
        if !matches!(
            state,
            PlaybackState::Ready | PlaybackState::Playing | PlaybackState::Paused
        ) {
            return;
        }
        if self.get_flag("eof-reached").unwrap_or(false) {
            if !self.end_reported.swap(true, Ordering::Relaxed) {
                self.log.info("end of file: eof");
                self.set_state(PlaybackState::Ended);
            }
        } else {
            self.end_reported.store(false, Ordering::Relaxed);
        }
    }

    fn log_end_of_file(&self, reason: &str) {
        if !self.end_reported.swap(true, Ordering::Relaxed) {
            self.log.info(format!("end of file: {reason}"));
        }
    }

    fn command(&self, args: &[&str]) -> Result<(), String> {
        let args: Vec<&[u8]> = args.iter().map(|arg| arg.as_bytes()).collect();
        self.command_bytes(&args)
    }

    fn command_bytes(&self, args: &[&[u8]]) -> Result<(), String> {
        let text = lossy_text(args);
        self.log.command(&[text.as_str()]);
        let action = Action::Command(args);
        self.run_command(args.iter().copied(), &action)
    }

    fn run_command<'a>(
        &self,
        args: impl Iterator<Item = &'a [u8]>,
        action: &Action,
    ) -> Result<(), String> {
        let strings: Vec<CString> = match args.map(CString::new).collect::<Result<_, _>>() {
            Ok(strings) => strings,
            Err(_) => return self.record_failure(action, "an argument contains a NUL byte"),
        };
        let mut pointers: Vec<*const c_char> = strings.iter().map(|arg| arg.as_ptr()).collect();
        pointers.push(ptr::null());
        let code = unsafe { mpv::mpv_command(self.handle, pointers.as_ptr()) };
        self.record_if_failed(action, code)
    }

    /// Logs and stores a failure when the return code is negative.
    fn record_if_failed(&self, action: &Action, code: c_int) -> Result<(), String> {
        if code >= 0 {
            return Ok(());
        }
        self.record_failure(action, &error_string(code))
    }

    fn record_failure(&self, action: &Action, error: &str) -> Result<(), String> {
        let failure = format!("{}: {error}", action.describe());
        match action {
            Action::Command(_) => self.log.command_failed(&failure),
            Action::Property { .. } => self.log.property_failed(&failure),
        }
        *self.failure.lock().unwrap() = Some(failure.clone());
        Err(failure)
    }

    fn record_property(
        &self,
        name: &str,
        text: &str,
        format: c_int,
        data: *mut c_void,
    ) -> Result<(), String> {
        let action = Action::Property { name, value: text };
        self.log.property_write(name, text);
        let name = CString::new(name).unwrap();
        let code = unsafe { mpv::mpv_set_property(self.handle, name.as_ptr(), format, data) };
        self.record_if_failed(&action, code)
    }

    fn set_double(&self, name: &str, value: f64) -> Result<(), String> {
        let mut value = value;
        let text = format!("{value}");
        self.record_property(
            name,
            &text,
            mpv::FORMAT_DOUBLE,
            &mut value as *mut f64 as *mut c_void,
        )
    }

    fn set_flag(&self, name: &str, value: bool) -> Result<(), String> {
        let mut value: c_int = c_int::from(value);
        let text = if value != 0 { "yes" } else { "no" };
        self.record_property(
            name,
            text,
            mpv::FORMAT_FLAG,
            &mut value as *mut c_int as *mut c_void,
        )
    }

    fn get_bytes(&self, name: &str) -> Option<Vec<u8>> {
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
        let bytes = unsafe { CStr::from_ptr(value) }.to_bytes().to_vec();
        unsafe { mpv::mpv_free(value as *mut c_void) };
        Some(bytes)
    }

    fn get_string(&self, name: &str) -> Option<String> {
        self.get_bytes(name)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }

    fn get_path(&self, name: &str) -> Option<PathBuf> {
        self.get_bytes(name).map(path_from_bytes)
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

fn end_reason(reason: c_int) -> String {
    match reason {
        mpv::END_FILE_REASON_EOF => "eof".into(),
        mpv::END_FILE_REASON_STOP => "stop".into(),
        mpv::END_FILE_REASON_QUIT => "quit".into(),
        mpv::END_FILE_REASON_ERROR => "error".into(),
        mpv::END_FILE_REASON_REDIRECT => "redirect".into(),
        other => format!("unknown ({other})"),
    }
}

fn text_at(pointer: *const c_char) -> String {
    if pointer.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
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
    pub struct EventLogMessage {
        pub prefix: *const c_char,
        pub level: *const c_char,
        pub text: *const c_char,
        pub log_level: c_int,
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
    pub const EVENT_LOG_MESSAGE: c_int = 2;
    pub const EVENT_END_FILE: c_int = 7;
    pub const EVENT_FILE_LOADED: c_int = 8;
    pub const END_FILE_REASON_EOF: c_int = 0;
    pub const END_FILE_REASON_STOP: c_int = 2;
    pub const END_FILE_REASON_QUIT: c_int = 3;
    pub const END_FILE_REASON_ERROR: c_int = 4;
    pub const END_FILE_REASON_REDIRECT: c_int = 5;

    pub const LOG_LEVEL_WARN: c_int = 30;

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

    unsafe extern "C" {
        pub fn mpv_client_api_version() -> c_ulong;
        pub fn mpv_create() -> *mut Handle;
        pub fn mpv_set_option_string(
            ctx: *mut Handle,
            name: *const c_char,
            data: *const c_char,
        ) -> c_int;
        pub fn mpv_initialize(ctx: *mut Handle) -> c_int;
        pub fn mpv_command(ctx: *mut Handle, args: *const *const c_char) -> c_int;
        pub fn mpv_request_log_messages(ctx: *mut Handle, min_level: *const c_char) -> c_int;
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

fn path_bytes(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

fn path_from_bytes(bytes: Vec<u8>) -> PathBuf {
    use std::os::unix::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_vec(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn a_matching_client_api_major_is_accepted() {
        assert_eq!(check_client_api_version((2 << 16) | 5), Ok(()));
    }

    #[test]
    fn a_mismatched_client_api_major_names_both_majors() {
        let error = check_client_api_version(3 << 16).unwrap_err();
        assert!(error.contains("found major 3"), "{error}");
        assert!(error.contains("expected major 2"), "{error}");
    }

    #[test]
    fn the_installed_libmpv_matches_the_client_api_major() {
        check_client_api_version(u64::from(unsafe { mpv::mpv_client_api_version() })).unwrap();
    }

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

    fn wait_for_ended(engine: &MpvEngine) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while engine.state() != PlaybackState::Ended && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(engine.state(), PlaybackState::Ended);
    }

    fn quiet() -> Arc<RunLog> {
        Arc::new(RunLog::disabled())
    }

    fn engine() -> MpvEngine {
        MpvEngine::new(true, quiet()).unwrap()
    }

    #[test]
    fn the_run_log_records_the_mpv_version_and_options() {
        let (log, sink, path) = crate::log::testing::log_to_file("engine-startup");
        let engine = MpvEngine::new(true, log).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("mpv version: "), "{text}");
        assert!(text.contains("options: "), "{text}");
        assert!(text.contains("vo=libmpv"), "{text}");
        assert!(text.contains("ao=null"), "{text}");
        assert_eq!(sink.lines(), text.lines().collect::<Vec<_>>());
        drop(engine);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_rejected_command_is_logged_with_its_name_and_arguments() {
        let (log, _sink, path) = crate::log::testing::log_to_file("engine-rejected");
        let mut engine = MpvEngine::new(true, log).unwrap();
        let error = engine
            .core
            .command(&["seek", "not-a-time", "absolute"])
            .unwrap_err();
        assert!(error.contains("seek"), "{error}");
        let failure = engine.take_command_failure().expect("a stored failure");
        assert!(
            failure.starts_with("seek not-a-time absolute: "),
            "{failure}"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("command: seek not-a-time absolute"), "{text}");
        assert!(
            text.contains("command failed: seek not-a-time absolute: "),
            "{text}"
        );
        drop(engine);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn rejected_volume_and_speed_property_writes_are_recorded() {
        let (log, _sink, path) = crate::log::testing::log_to_file("engine-properties");
        let mut engine = MpvEngine::new(true, log).unwrap();
        let volume_error = engine.core.set_double("volume", f64::NAN).unwrap_err();
        assert!(volume_error.contains("volume"), "{volume_error}");
        let failure = engine.take_command_failure().expect("a stored failure");
        assert!(failure.starts_with("volume = NaN: "), "{failure}");
        let speed_error = engine.core.set_double("speed", 0.0).unwrap_err();
        assert!(speed_error.contains("speed"), "{speed_error}");
        let failure = engine.take_command_failure().expect("a stored failure");
        assert!(failure.starts_with("speed = 0: "), "{failure}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("property write failed: volume = NaN"),
            "{text}"
        );
        assert!(text.contains("property write failed: speed = 0"), "{text}");
        assert!(!text.contains("command: set volume"), "{text}");
        assert!(!text.contains("command: set speed"), "{text}");
        drop(engine);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_failure_is_stored_once_when_the_log_is_off() {
        let mut engine = MpvEngine::new(true, quiet()).unwrap();
        engine
            .core
            .command(&["seek", "not-a-time", "absolute"])
            .unwrap_err();
        let failure = engine.take_command_failure().expect("a stored failure");
        assert!(
            failure.starts_with("seek not-a-time absolute: "),
            "{failure}"
        );
        assert_eq!(engine.take_command_failure(), None);
    }

    #[test]
    fn user_commands_are_recorded_with_their_arguments() {
        let (log, _sink, path) = crate::log::testing::log_to_file("engine-user");
        let mut engine = MpvEngine::new(true, log).unwrap();
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);
        engine.toggle();
        engine.set_volume(0.4);
        engine.set_speed(1.25);
        engine.seek_preview(0.5);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("property write: pause = yes"), "{text}");
        assert!(text.contains("property write: volume = 40"), "{text}");
        assert!(text.contains("property write: speed = 1.25"), "{text}");
        assert!(
            text.contains("command: seek 0.500 absolute+exact"),
            "{text}"
        );
        assert!(!text.contains("command: set pause"), "{text}");
        drop(engine);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_run_log_records_a_natural_end_of_file_once() {
        let (log, _sink, path) = crate::log::testing::log_to_file("engine-eof");
        let mut engine = MpvEngine::new(true, log).unwrap();
        assert_eq!(engine.core.get_flag("keep-open"), Some(true));
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_ended(&engine);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("end of file: eof").count(), 1, "{text}");
        assert_eq!(
            text.lines()
                .filter(|line| line.contains("end of file:"))
                .count(),
            1,
            "{text}"
        );
        let lines = our_lines(&path);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(lines, our_lines(&path), "the eof check wrote per frame");
        drop(engine);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mpv_log_messages_are_mirrored_to_both_sinks() {
        let (log, sink, path) = crate::log::testing::log_to_file("engine-mpv");
        let mut engine = MpvEngine::new(true, log).unwrap();
        let dir = std::env::temp_dir().join(format!("kachito-engine-mpv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let corrupt = dir.join("corrupt.mkv");
        std::fs::write(&corrupt, b"not a video").unwrap();
        engine.open(&corrupt).unwrap();
        wait_for_error(&engine);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.lines().any(|line| line.starts_with("mpv:")), "{text}");
        assert_eq!(sink.lines(), text.lines().collect::<Vec<_>>());
        drop(engine);
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn playback_writes_no_per_frame_log_lines() {
        let (log, _sink, path) = crate::log::testing::log_to_file("engine-frames");
        let mut engine = MpvEngine::new(true, log).unwrap();
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame_count(&engine, 2);
        let before = our_lines(&path);
        wait_for_frame_count(&engine, engine.frames() + 1);
        thread::sleep(Duration::from_millis(100));
        let after = our_lines(&path);
        assert_eq!(before, after, "the frame path wrote to the run log");
        drop(engine);
        let _ = std::fs::remove_file(path);
    }

    fn our_lines(path: &Path) -> usize {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .filter(|line| !line.starts_with("mpv:"))
            .count()
    }

    #[test]
    fn opens_an_mp4_file_and_pushes_a_frame() {
        let mut engine = engine();
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
        let mut engine = engine();
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);
        assert!(
            engine.surface().last_error().is_none(),
            "{:?}",
            engine.surface().last_error()
        );
    }

    #[cfg(unix)]
    #[test]
    fn opens_a_non_utf8_path() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir =
            std::env::temp_dir().join(format!("kachito-engine-non-utf8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join(OsString::from_vec(b"source-\xff.mkv".to_vec()));
        std::fs::copy(fixture("rgbw.mkv"), &source).unwrap();

        let mut engine = engine();
        engine.open(&source).unwrap();
        wait_for_frame(&engine);
        assert_eq!(engine.current_path().as_deref(), Some(source.as_path()));
        assert!(engine.state().is_loaded(), "{:?}", engine.state());
        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_engine_reports_a_loaded_state_after_a_frame() {
        let mut engine = engine();
        assert_eq!(engine.state(), PlaybackState::Empty);
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);
        assert!(engine.state().is_loaded(), "{:?}", engine.state());
    }

    #[test]
    fn the_engine_reports_the_frame_duration() {
        let mut engine = engine();
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
        let mut engine = engine();
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
        // Consume the one-shot Ready so the comparison measures a real transition.
        let _ = engine.state();
        let state_before = engine.state();
        let frames = engine.frames();
        let wakes_before = wakes.load(Ordering::Relaxed);

        engine.seek(0.5);
        assert_eq!(engine.last_seek_flags().as_deref(), Some("absolute+exact"));
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
    fn a_preview_seek_lands_on_the_requested_frame() {
        let mut engine = engine();
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);

        engine.toggle();
        thread::sleep(Duration::from_millis(150));
        let frames = engine.frames();

        // rgbw.mkv has keyframes at 0.0, 0.5 and 0.75 s. 0.25 s is between
        // the first two keyframes, so a keyframe seek cannot show it.
        engine.seek_preview(0.25);
        wait_for_frame_count(&engine, frames + 1);

        let landed = engine.position();
        assert!(
            (landed - 0.25).abs() <= 1.0 / 8.0,
            "the preview landed at {landed:.3} s instead of the requested 0.250 s"
        );
    }

    #[test]
    fn a_release_after_a_preview_keeps_the_preview_frame() {
        let mut engine = engine();
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);
        engine.toggle();
        thread::sleep(Duration::from_millis(150));

        engine.seek_preview(0.25);
        let deadline = Instant::now() + Duration::from_secs(5);
        while (engine.position() - 0.25).abs() > 1.0 / 8.0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let preview = engine.position();

        engine.seek(0.25);
        let deadline = Instant::now() + Duration::from_secs(5);
        while (engine.position() - 0.25).abs() > 1.0 / 8.0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        let released = engine.position();
        assert!(
            (released - preview).abs() <= 1.0 / 8.0,
            "the release landed at {released:.3} s after a {preview:.3} s preview"
        );
    }

    #[test]
    fn a_preview_seek_pushes_a_frame_without_changing_state() {
        let mut engine = engine();
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);

        engine.toggle();
        thread::sleep(Duration::from_millis(100));
        // Consume the one-shot Ready so the comparison measures a real transition.
        let _ = engine.state();
        let state_before = engine.state();
        let frames = engine.frames();

        engine.seek_preview(0.5);
        assert_eq!(engine.last_seek_flags().as_deref(), Some("absolute+exact"));
        wait_for_frame_count(&engine, frames + 1);

        assert_eq!(engine.state(), state_before);
    }

    #[test]
    fn a_missing_file_sets_an_error() {
        let mut engine = engine();
        let _ = engine.open(Path::new("/definitely/not/here.mkv"));
        wait_for_error(&engine);
    }

    #[test]
    fn lists_and_selects_audio_and_subtitle_tracks() {
        let mut engine = engine();
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
