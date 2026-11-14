//! Playback through libmpv, behind a small seam the UI can fake in tests.

use crate::log::RunLog;
use rune::native_surface::{ByteFrame, ColorInfo, Format};
use rune::{NativeSurface, Waker};
use std::ffi::{CStr, CString, c_char, c_int, c_ulong, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
#[cfg(test)]
use std::sync::atomic::AtomicU64;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const VIDEO_WIDTH: i32 = 1280;
pub const VIDEO_HEIGHT: i32 = 720;

/// The libmpv client API major this build is written against.
pub const CLIENT_API_MAJOR: u32 = 2;

/// Exit waits this long for the render thread to free its context.
const RENDER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);

/// The UI waits this long for the event thread to answer a synchronous
/// command. A stalled event thread must not block the UI forever.
const REQUEST_REPLY_TIMEOUT: Duration = Duration::from_secs(2);

/// The event thread blocks in the event API for this long so a keep-open end
/// of file, which emits no event, is still observed.
const EVENT_WAIT_SECONDS: f64 = 0.1;

/// A frame delivers no completed draw for this many pushes before the surface
/// is treated as hidden.
const UNDRAWN_PUSH_LIMIT: u64 = 3;

/// Every built-in script loader, auto-loaded script and loader mpv runs
/// without being asked. All of them are turned off before initialization so a
/// user config cannot start its own UI, console or scripts. The test iterates
/// this list, so a new entry cannot miss the startup check.
const BUILT_IN_SCRIPT_LOADERS: &[&str] = &[
    "load-scripts",
    "load-auto-profiles",
    "load-commands",
    "load-console",
    "load-osd-console",
    "load-positioning",
    "load-select",
    "load-stats-overlay",
    "load-context-menu",
    "osc",
];

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
    Playing,
    Paused,
    Ended,
    Failed(String),
}

impl PlaybackState {
    pub fn is_loaded(&self) -> bool {
        matches!(self, Self::Playing | Self::Paused | Self::Ended)
    }

    /// The explicit edges of the playback state machine. A same-state
    /// transition is a no-op so a repeated event cannot flap the state.
    fn allows(&self, next: &Self) -> bool {
        if self == next {
            return true;
        }
        match (self, next) {
            (_, Self::Loading) => true,
            (_, Self::Failed(_)) => !matches!(self, Self::Failed(_)),
            (Self::Loading, Self::Playing | Self::Paused | Self::Ended | Self::Empty) => true,
            (Self::Playing, Self::Paused | Self::Ended | Self::Empty) => true,
            (Self::Paused, Self::Playing | Self::Ended | Self::Empty) => true,
            (Self::Ended, Self::Playing | Self::Paused | Self::Empty) => true,
            (Self::Failed(_), Self::Empty) => true,
            _ => false,
        }
    }
}

/// Whether three more frames were pushed than the surface completed draws.
fn skip_software_render(submitted: u64, completed: u64) -> bool {
    submitted.saturating_sub(completed) >= UNDRAWN_PUSH_LIMIT
}

/// The software render target. `bgra` carries a defined alpha; `bgr0` leaves it
/// undefined, so the fallback fills it in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SoftwareFormat {
    name: &'static [u8],
    alpha_pass: bool,
}

impl SoftwareFormat {
    const BGRA: Self = Self {
        name: b"bgra\0",
        alpha_pass: false,
    };
    const BGR0: Self = Self {
        name: b"bgr0\0",
        alpha_pass: true,
    };

    fn label(self) -> &'static str {
        if self.alpha_pass { "bgr0" } else { "bgra" }
    }

    /// A format change is allowed only when libmpv rejects the target itself.
    /// Any other render error is not a format problem and keeps `bgra`.
    fn fallback(self, code: c_int) -> Option<Self> {
        (self == Self::BGRA && code == mpv::ERROR_UNSUPPORTED).then_some(Self::BGR0)
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
    /// Sets pause explicitly. The startup sequence uses this before a load.
    fn set_paused(&mut self, paused: bool) -> Result<(), String>;
    /// Takes the pending frame-start signal. True after the first video reconfig
    /// or the first frame the render thread pushes since it was last taken.
    fn take_frame_start(&mut self) -> bool;
    /// Takes the most recent rejected command, once, for the one-notice rule.
    fn take_command_failure(&mut self) -> Option<String>;
}

/// Commands that change playback state run on the event thread, which owns the
/// state machine. Synchronous commands answer when the event thread is done so
/// the UI reads the transition on its next poll.
enum Request {
    Open {
        path: PathBuf,
        reply: SyncSender<Result<(), String>>,
    },
    Toggle {
        reply: SyncSender<()>,
    },
    Seek {
        seconds: f64,
        preview: bool,
        reply: SyncSender<()>,
    },
    Pause {
        paused: bool,
        reply: SyncSender<Result<(), String>>,
    },
    Shutdown,
}

/// The condition variable the render update callback signals. The callback
/// only touches this value; it never calls into mpv.
struct RenderWake {
    pending: Mutex<bool>,
    ready: Condvar,
}

impl RenderWake {
    fn new() -> Self {
        Self {
            pending: Mutex::new(false),
            ready: Condvar::new(),
        }
    }

    fn notify(&self) {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *pending = true;
        self.ready.notify_one();
    }

    /// Blocks until the update callback signals or shutdown starts.
    fn wait(&self, stop: &AtomicBool) -> bool {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while !*pending && !stop.load(Ordering::Relaxed) {
            pending = self
                .ready
                .wait(pending)
                .unwrap_or_else(|error| error.into_inner());
        }
        let ready = *pending;
        *pending = false;
        ready && !stop.load(Ordering::Relaxed)
    }
}

extern "C" fn render_update_callback(ctx: *mut c_void) {
    // The render contract forbids any mpv call from this callback.
    let wake = unsafe { &*(ctx as *const RenderWake) };
    wake.notify();
}

struct RenderThread {
    core: Option<Arc<Core>>,
    stop: Arc<AtomicBool>,
    wake: Arc<RenderWake>,
    done: Receiver<()>,
    handle: Option<JoinHandle<()>>,
}

impl RenderThread {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.wake.notify();
    }

    /// Waits for the render thread to free its context.
    ///
    /// A thread that misses the deadline is left detached, and its reference
    /// to the core is leaked: the thread may still be inside
    /// `mpv_render_context_render` or `mpv_render_context_free`, and destroying
    /// the handle under it would be a use-after-free. The handle then lives
    /// until process exit, which is the safe outcome.
    fn join(&mut self, timeout: Duration, log: &RunLog) -> bool {
        match self.done.recv_timeout(timeout) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => {
                if let Some(handle) = self.handle.take() {
                    let _ = handle.join();
                }
                let _ = self.core.take();
                true
            }
            Err(RecvTimeoutError::Timeout) => {
                log.warn(format!(
                    "render thread did not stop within {:.1} s; keeping the player handle alive until the process exits",
                    timeout.as_secs_f64()
                ));
                // SAFETY: The detached thread still owns the render context
                // and may call into this handle. Leaking the Arc keeps
                // `Core::drop` from running `mpv_terminate_destroy` while
                // that is possible.
                if let Some(core) = self.core.take() {
                    std::mem::forget(core);
                }
                false
            }
        }
    }
}

impl Drop for RenderThread {
    fn drop(&mut self) {
        // A thread that was never joined (for example because the event thread
        // panicked) is in the same position as a timed-out join.
        if self.handle.is_some()
            && let Some(core) = self.core.take()
        {
            std::mem::forget(core);
        }
    }
}

pub struct MpvEngine {
    core: Arc<Core>,
    surface: NativeSurface,
    requests: std::sync::mpsc::Sender<Request>,
    event_thread: Option<JoinHandle<()>>,
    #[cfg(test)]
    frames: Arc<AtomicU64>,
    #[cfg(test)]
    test_draws: Arc<AtomicU64>,
}

impl MpvEngine {
    #[cfg(test)]
    fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn last_seek_flags(&self) -> Option<String> {
        self.core.last_seek_flags.lock().unwrap().clone()
    }

    /// Simulates a completed draw for engine tests, which have no Rune window.
    #[cfg(test)]
    fn note_draw(&self) {
        self.test_draws.fetch_add(1, Ordering::Relaxed);
    }

    /// `headless` keeps audio off so tests do not open a device.
    pub fn new(headless: bool, log: Arc<RunLog>) -> Result<Self, String> {
        let core = Arc::new(Core::new(headless, log.clone())?);
        let frame_start = core.frame_start.clone();
        let surface = NativeSurface::new();
        let wake = Arc::new(RenderWake::new());
        let render_stop = Arc::new(AtomicBool::new(false));
        #[cfg(test)]
        let frames = Arc::new(AtomicU64::new(0));
        #[cfg(test)]
        let test_draws = Arc::new(AtomicU64::new(0));

        let (render_ready, render_ready_rx) = std::sync::mpsc::sync_channel(1);
        let (render_done, render_done_rx) = std::sync::mpsc::sync_channel(1);
        let callback_ctx = Arc::as_ptr(&wake) as usize;
        let render_handle = {
            let surface = surface.clone();
            let wake = wake.clone();
            let stop = render_stop.clone();
            let log = log.clone();
            let frame_start = frame_start.clone();
            #[cfg(test)]
            let frames = frames.clone();
            #[cfg(test)]
            let test_draws = test_draws.clone();
            let handle = core.handle as usize;
            thread::Builder::new()
                .name("kachito-render".into())
                .spawn(move || {
                    let render = match unsafe { create_render(handle as *mut mpv::Handle) } {
                        Ok(render) => render,
                        Err(error) => {
                            let _ = render_ready.send(Err(error));
                            return;
                        }
                    };
                    unsafe {
                        mpv::mpv_render_context_set_update_callback(
                            render,
                            Some(render_update_callback),
                            callback_ctx as *mut c_void,
                        )
                    };
                    let _ = render_ready.send(Ok(()));
                    {
                        let mut renderer = Renderer {
                            render: RenderContext(render),
                            surface,
                            stop,
                            wake,
                            log,
                            frame_start,
                            #[cfg(test)]
                            frames,
                            buffer: Vec::new(),
                            frame_id: 0,
                            pushed: 0,
                            completed_seen: 0,
                            format: SoftwareFormat::BGRA,
                            render_error_logged: false,
                            #[cfg(test)]
                            test_draws,
                        };
                        renderer.run();
                    }
                    let _ = render_done.send(());
                })
                .map_err(|error| format!("could not start the render thread: {error}"))?
        };

        let mut render_thread = RenderThread {
            core: Some(core.clone()),
            stop: render_stop,
            wake: wake.clone(),
            done: render_done_rx,
            handle: Some(render_handle),
        };
        match render_ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                let _ = render_thread.handle.take().map(JoinHandle::join);
                return Err(error);
            }
            Err(_) => {
                let _ = render_thread.handle.take().map(JoinHandle::join);
                return Err("the render thread stopped before it was ready".into());
            }
        }

        let (requests, requests_rx) = std::sync::mpsc::channel();
        let event_core = core.clone();
        let stop_events = render_thread.stop.clone();
        let wake_events = render_thread.wake.clone();
        let event_thread = match thread::Builder::new()
            .name("kachito-events".into())
            .spawn(move || run_event_loop(event_core, requests_rx, render_thread))
        {
            Ok(thread) => thread,
            Err(error) => {
                stop_events.store(true, Ordering::Relaxed);
                wake_events.notify();
                return Err(format!("could not start the event thread: {error}"));
            }
        };

        Ok(Self {
            core,
            surface,
            requests,
            event_thread: Some(event_thread),
            #[cfg(test)]
            frames,
            #[cfg(test)]
            test_draws,
        })
    }

    fn wake_event_thread(&self) {
        unsafe { mpv::mpv_wakeup(self.core.handle) };
    }

    /// Waits for one synchronous command to answer. A timeout is a logged
    /// failure rather than an unbounded UI stall.
    fn wait_for_reply<T>(&self, done: Receiver<T>, what: &str) -> Result<T, String> {
        match done.recv_timeout(REQUEST_REPLY_TIMEOUT) {
            Ok(reply) => Ok(reply),
            Err(RecvTimeoutError::Timeout) => {
                let error = format!(
                    "the playback event thread did not answer {what} within {:.1} s",
                    REQUEST_REPLY_TIMEOUT.as_secs_f64()
                );
                self.core.log.failure(&error);
                Err(error)
            }
            Err(RecvTimeoutError::Disconnected) => {
                let error = format!("the playback event thread stopped before answering {what}");
                self.core.log.failure(&error);
                Err(error)
            }
        }
    }

    fn seek_on_event_thread(&mut self, seconds: f64, preview: bool) {
        let (reply, done) = std::sync::mpsc::sync_channel(1);
        if self
            .requests
            .send(Request::Seek {
                seconds,
                preview,
                reply,
            })
            .is_ok()
        {
            self.wake_event_thread();
            let _ = self.wait_for_reply(done, "seek");
        }
    }
}

impl PlayerEngine for MpvEngine {
    fn open(&mut self, path: &Path) -> Result<(), String> {
        let (reply, done) = std::sync::mpsc::sync_channel(1);
        self.requests
            .send(Request::Open {
                path: path.to_path_buf(),
                reply,
            })
            .map_err(|_| "the playback event thread is not running".to_string())?;
        self.wake_event_thread();
        self.wait_for_reply(done, "open")?
    }
    fn toggle(&mut self) {
        let (reply, done) = std::sync::mpsc::sync_channel(1);
        if self.requests.send(Request::Toggle { reply }).is_ok() {
            self.wake_event_thread();
            let _ = self.wait_for_reply(done, "toggle");
        }
    }
    fn seek(&mut self, seconds: f64) {
        self.seek_on_event_thread(seconds, false);
    }
    fn seek_preview(&mut self, seconds: f64) {
        self.seek_on_event_thread(seconds, true);
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
    fn set_paused(&mut self, paused: bool) -> Result<(), String> {
        let (reply, done) = std::sync::mpsc::sync_channel(1);
        self.requests
            .send(Request::Pause { paused, reply })
            .map_err(|_| "the playback event thread is not running".to_string())?;
        self.wake_event_thread();
        self.wait_for_reply(done, "pause")?
    }
    fn take_frame_start(&mut self) -> bool {
        self.core.take_frame_start()
    }
    fn take_command_failure(&mut self) -> Option<String> {
        self.core.failure.lock().unwrap().take()
    }
}

impl Drop for MpvEngine {
    fn drop(&mut self) {
        let _ = self.requests.send(Request::Shutdown);
        self.wake_event_thread();
        if let Some(thread) = self.event_thread.take() {
            let _ = thread.join();
        }
    }
}

struct Core {
    handle: *mut mpv::Handle,
    log: Arc<RunLog>,
    state: Mutex<PlaybackState>,
    end_reported: AtomicBool,
    frame_start: Arc<AtomicBool>,
    failure: Mutex<Option<String>>,
    waker: Mutex<Option<Waker>>,
    #[cfg(test)]
    pause_reads: AtomicUsize,
    #[cfg(test)]
    last_seek_flags: Mutex<Option<String>>,
}

// SAFETY: The two-thread split makes this load-bearing. The event thread and
// the UI thread call libmpv through the same handle; libmpv's client API is
// thread-safe, and every mutable field is behind a mutex or an atomic, so no
// Rust-level data race is possible. The handle outlives both threads because
// `Core::drop` runs only after the render thread has freed its context (see
// the comment on that impl).
unsafe impl Send for Core {}
unsafe impl Sync for Core {}

/// The options set on the handle before `mpv_initialize`. Every built-in
/// script loader and auto-loaded script is disabled here.
fn startup_options(headless: bool) -> Vec<(&'static str, &'static str)> {
    let mut options = vec![
        ("vo", "libmpv"),
        ("idle", "yes"),
        ("keep-open", "yes"),
        ("video-sync", "audio"),
        ("video-timing-offset", "0"),
        ("osd-level", "0"),
        ("ytdl", "no"),
        ("input-default-bindings", "no"),
        ("input-vo-keyboard", "no"),
    ];
    options.extend(BUILT_IN_SCRIPT_LOADERS.iter().map(|name| (*name, "no")));
    options.push(("hwdec", if headless { "no" } else { "auto-safe" }));
    if headless {
        options.push(("ao", "null"));
    }
    options
}

impl Core {
    fn new(headless: bool, log: Arc<RunLog>) -> Result<Self, String> {
        unsafe {
            check_client_api_version(u64::from(mpv::mpv_client_api_version()))?;
            let handle = mpv::mpv_create();
            if handle.is_null() {
                log.warn("mpv could not create a player");
                return Err("libmpv could not create a player".into());
            }
            let options = startup_options(headless);
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
                end_reported: AtomicBool::new(false),
                frame_start: Arc::new(AtomicBool::new(false)),
                failure: Mutex::new(None),
                waker: Mutex::new(None),
                #[cfg(test)]
                pause_reads: AtomicUsize::new(0),
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

    /// Applies one explicit state transition. An edge outside the machine is
    /// ignored and logged, and the old state stays in place.
    fn transition(&self, next: PlaybackState) -> bool {
        let next_label = format!("{next:?}");
        let rejected = {
            let mut state = self.state.lock().unwrap();
            if !state.allows(&next) {
                Some(state.clone())
            } else if *state == next {
                return false;
            } else {
                *state = next;
                None
            }
        };
        if let Some(previous) = rejected {
            self.log.warn(format!(
                "ignored playback transition: {previous:?} -> {next_label}"
            ));
            return false;
        }
        self.wake();
        true
    }

    fn open(&self, path: &Path) -> Result<(), String> {
        let bytes = path_bytes(path);
        self.log.info(format!("load: {}", path.to_string_lossy()));
        self.end_reported.store(false, Ordering::Relaxed);
        self.frame_start.store(false, Ordering::Relaxed);
        self.transition(PlaybackState::Loading);
        match self.command_bytes(&[b"loadfile".as_slice(), bytes.as_slice()]) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.transition(PlaybackState::Failed(error.clone()));
                Err(error)
            }
        }
    }

    fn current_path(&self) -> Option<PathBuf> {
        self.get_path("path")
    }

    fn state(&self) -> PlaybackState {
        self.state.lock().unwrap().clone()
    }

    fn on_file_loaded(&self) {
        // One pause read decides the stored state; a state query never reads it.
        let paused = self.get_flag("pause").unwrap_or(false);
        let next = if paused {
            PlaybackState::Paused
        } else {
            PlaybackState::Playing
        };
        self.transition(next);
    }

    fn set_paused(&self, paused: bool) -> Result<(), String> {
        if !paused && matches!(self.state(), PlaybackState::Ended) {
            self.seek_exact(0.0);
        }
        self.set_flag("pause", paused)?;
        if self.state().is_loaded() || matches!(self.state(), PlaybackState::Loading) {
            let next = if paused {
                PlaybackState::Paused
            } else {
                PlaybackState::Playing
            };
            self.transition(next);
        }
        Ok(())
    }

    fn take_frame_start(&self) -> bool {
        self.frame_start.swap(false, Ordering::Relaxed)
    }

    fn toggle(&self) {
        match self.state() {
            PlaybackState::Playing => {
                let _ = self.set_paused(true);
            }
            PlaybackState::Paused | PlaybackState::Ended => {
                let _ = self.set_paused(false);
            }
            other => self
                .log
                .warn(format!("ignored toggle in playback state {other:?}")),
        }
    }

    fn seek(&self, seconds: f64) {
        if !seconds.is_finite() {
            return;
        }
        self.seek_exact(seconds);
        if matches!(self.state(), PlaybackState::Ended) {
            self.transition(PlaybackState::Paused);
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

    /// Handles one event on the event thread. Returns true when the event asks
    /// the event loop to stop.
    fn handle_event(&self, event: *const mpv::Event) -> bool {
        let id = unsafe { (*event).event_id };
        match id {
            mpv::EVENT_NONE => {}
            mpv::EVENT_SHUTDOWN => {
                self.log.info("mpv shut down");
                return true;
            }
            mpv::EVENT_LOG_MESSAGE => {
                let data = unsafe { (*event).data };
                if data.is_null() {
                    return false;
                }
                let message = unsafe { &*(data as *const mpv::EventLogMessage) };
                if message.log_level > mpv::LOG_LEVEL_WARN {
                    return false;
                }
                self.log.record_mpv_message(
                    &text_at(message.level),
                    &text_at(message.prefix),
                    &text_at(message.text),
                );
            }
            mpv::EVENT_FILE_LOADED => self.on_file_loaded(),
            mpv::EVENT_VIDEO_RECONFIG => self.frame_start.store(true, Ordering::Relaxed),
            mpv::EVENT_END_FILE => self.on_end_file(unsafe { (*event).data }),
            mpv::EVENT_QUEUE_OVERFLOW => self.on_queue_overflow(),
            _ => {}
        }
        false
    }

    fn on_end_file(&self, data: *mut c_void) {
        if data.is_null() {
            self.log_end_of_file("unknown");
            self.transition(PlaybackState::Failed("the file ended unexpectedly".into()));
            return;
        }
        let end = unsafe { &*(data as *const mpv::EventEndFile) };
        let reason = end_reason(end.reason);
        if end.reason == mpv::END_FILE_REASON_ERROR {
            self.log_end_of_file(&format!("{reason}: {}", error_string(end.error)));
        } else {
            self.log_end_of_file(&reason);
        }
        match end.reason {
            mpv::END_FILE_REASON_EOF => {
                self.transition(PlaybackState::Ended);
            }
            mpv::END_FILE_REASON_ERROR => {
                self.transition(PlaybackState::Failed(error_string(end.error)));
            }
            // A stop or quit says nothing about the new file; wait for the next
            // event and keep the state that is already stored.
            mpv::END_FILE_REASON_STOP | mpv::END_FILE_REASON_QUIT => {}
            _ => {
                self.transition(PlaybackState::Empty);
            }
        }
    }

    fn on_queue_overflow(&self) {
        self.log.failure(
            "event queue overflow: events were dropped before they reached the player state",
        );
        if matches!(self.state(), PlaybackState::Playing) {
            // Pause first: `set_paused` stores Paused only after the write
            // succeeds, so a rejected pause leaves the state at Playing where
            // playback actually is.
            let _ = self.set_paused(true);
        }
    }

    /// With keep-open, a natural end pauses at the last frame and emits no
    /// END_FILE event, so the eof-reached property is the only signal.
    fn observe_natural_eof(&self) {
        let state = self.state();
        if !matches!(state, PlaybackState::Playing | PlaybackState::Paused) {
            return;
        }
        if self.get_flag("eof-reached").unwrap_or(false) {
            if !self.end_reported.swap(true, Ordering::Relaxed) {
                self.log.info("end of file: eof");
                self.transition(PlaybackState::Ended);
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
        #[cfg(test)]
        if name == "pause" {
            self.pause_reads.fetch_add(1, Ordering::Relaxed);
        }
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
        // SAFETY: This runs only when every `Arc<Core>` is gone. A render
        // thread that was not joined leaks its Arc in `RenderThread::join` or
        // `RenderThread::drop`, so the handle cannot be destroyed while that
        // thread may still call into it.
        unsafe { mpv::mpv_terminate_destroy(self.handle) };
    }
}

fn run_event_loop(core: Arc<Core>, requests: Receiver<Request>, mut render: RenderThread) {
    'events: loop {
        loop {
            match requests.try_recv() {
                Ok(Request::Shutdown) => break 'events,
                Ok(request) => core.handle_request(request),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break 'events,
            }
        }

        // Block in the event API. A queued command calls mpv_wakeup, which makes
        // this return with EVENT_NONE so the next pass can run the command. The
        // timeout rechecks the keep-open EOF flag, which emits no event.
        let mut event = unsafe { mpv::mpv_wait_event(core.handle, EVENT_WAIT_SECONDS) };
        while !event.is_null() {
            let id = unsafe { (*event).event_id };
            if id == mpv::EVENT_NONE {
                break;
            }
            if core.handle_event(event) {
                break 'events;
            }
            event = unsafe { mpv::mpv_wait_event(core.handle, 0.0) };
        }
        core.observe_natural_eof();
    }

    // Shutdown: tell the core to quit, then let the render thread free its
    // context before this thread returns and the engine destroys the handle.
    let _ = core.command(&["quit"]);
    render.stop();
    render.join(RENDER_JOIN_TIMEOUT, &core.log);
}

impl Core {
    fn handle_request(&self, request: Request) {
        match request {
            Request::Open { path, reply } => {
                let _ = reply.send(self.open(&path));
            }
            Request::Toggle { reply } => {
                self.toggle();
                let _ = reply.send(());
            }
            Request::Seek {
                seconds,
                preview,
                reply,
            } => {
                if preview {
                    self.seek_preview(seconds);
                } else {
                    self.seek(seconds);
                }
                let _ = reply.send(());
            }
            Request::Pause { paused, reply } => {
                let _ = reply.send(self.set_paused(paused));
            }
            Request::Shutdown => {}
        }
    }
}

struct RenderContext(*mut mpv::Render);

// SAFETY: The render contract requires every render-context call on one
// thread. The context is created on the render thread and freed there when
// `Renderer` drops; it is never shared, so moving the wrapper between threads
// cannot race. `Sync` is deliberately not implemented.
unsafe impl Send for RenderContext {}

impl Drop for RenderContext {
    fn drop(&mut self) {
        unsafe { mpv::mpv_render_context_free(self.0) };
    }
}

struct Renderer {
    render: RenderContext,
    surface: NativeSurface,
    stop: Arc<AtomicBool>,
    wake: Arc<RenderWake>,
    log: Arc<RunLog>,
    frame_start: Arc<AtomicBool>,
    #[cfg(test)]
    frames: Arc<AtomicU64>,
    buffer: Vec<u8>,
    frame_id: u64,
    pushed: u64,
    completed_seen: u64,
    format: SoftwareFormat,
    render_error_logged: bool,
    #[cfg(test)]
    test_draws: Arc<AtomicU64>,
}

impl Renderer {
    fn run(&mut self) {
        self.buffer = vec![0u8; frame_size()];
        while self.wake.wait(&self.stop) {
            let flags = unsafe { mpv::mpv_render_context_update(self.render.0) };
            if flags & mpv::UPDATE_FRAME == 0 {
                continue;
            }
            let completed = self.completed_draws();
            if completed > self.completed_seen {
                // One completed draw resets the visibility window.
                self.completed_seen = completed;
                self.pushed = completed;
            }
            if skip_software_render(self.pushed, self.completed_seen) {
                self.skip_frame();
            } else if let Err(error) = self.render_frame_and_push() {
                self.log_render_error_once(error);
            }
            // The immediate swap report stays next to the render call.
            unsafe { mpv::mpv_render_context_report_swap(self.render.0) };
        }
    }

    fn completed_draws(&self) -> u64 {
        let completed = self.surface.counts().1;
        #[cfg(test)]
        {
            completed + self.test_draws.load(Ordering::Relaxed)
        }
        #[cfg(not(test))]
        {
            completed
        }
    }

    fn render_frame_and_push(&mut self) -> Result<(), String> {
        match self.render_frame(self.format) {
            Ok(()) => self.push_frame(),
            Err(error) => {
                let Some(fallback) = self.format.fallback(error.code) else {
                    return Err(error.to_string());
                };
                self.log.warn(format!(
                    "software render format {} rejected: {error}; falling back to {} with the alpha pass",
                    self.format.label(),
                    fallback.label()
                ));
                self.format = fallback;
                self.render_frame(self.format)
                    .map_err(|error| error.to_string())?;
                self.push_frame()
            }
        }
    }

    fn push_frame(&mut self) -> Result<(), String> {
        if self.format.alpha_pass {
            opaque(&mut self.buffer);
        }
        let bytes = std::mem::take(&mut self.buffer);
        let result = self.surface.push_bytes(frame(bytes), self.frame_id);
        self.buffer = vec![0u8; frame_size()];
        result?;
        self.frame_id += 1;
        self.pushed += 1;
        self.frame_start.store(true, Ordering::Relaxed);
        #[cfg(test)]
        self.frames.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    fn skip_frame(&mut self) {
        let mut size = [VIDEO_WIDTH, VIDEO_HEIGHT];
        let mut stride = (VIDEO_WIDTH * 4) as usize;
        let block: c_int = 0;
        let mut skip: c_int = 1;
        let mut params = [
            mpv::Param {
                type_: mpv::PARAM_SW_SIZE,
                data: size.as_mut_ptr() as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_SW_FORMAT,
                data: self.format.name.as_ptr() as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_SW_STRIDE,
                data: &mut stride as *mut usize as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_SW_POINTER,
                data: self.buffer.as_mut_ptr() as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_BLOCK_FOR_TARGET_TIME,
                data: &block as *const c_int as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_SKIP_RENDERING,
                data: &mut skip as *mut c_int as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_INVALID,
                data: ptr::null_mut(),
            },
        ];
        let code = unsafe { mpv::mpv_render_context_render(self.render.0, params.as_mut_ptr()) };
        if let Err(error) = check_render(code, "libmpv skip render") {
            self.log_render_error_once(error);
        }
    }

    fn render_frame(&mut self, format: SoftwareFormat) -> Result<(), RenderError> {
        let mut size = [VIDEO_WIDTH, VIDEO_HEIGHT];
        let mut stride = (VIDEO_WIDTH * 4) as usize;
        let block: c_int = 0;
        let mut params = [
            mpv::Param {
                type_: mpv::PARAM_SW_SIZE,
                data: size.as_mut_ptr() as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_SW_FORMAT,
                data: format.name.as_ptr() as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_SW_STRIDE,
                data: &mut stride as *mut usize as *mut c_void,
            },
            mpv::Param {
                type_: mpv::PARAM_SW_POINTER,
                data: self.buffer.as_mut_ptr() as *mut c_void,
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
        check_render(
            unsafe { mpv::mpv_render_context_render(self.render.0, params.as_mut_ptr()) },
            "libmpv render",
        )
    }

    /// Render failures can repeat every frame, so only the first one is logged.
    fn log_render_error_once(&mut self, error: impl std::fmt::Display) {
        if !self.render_error_logged {
            self.render_error_logged = true;
            self.log.warn(format!("software render failed: {error}"));
        }
    }
}

fn frame_size() -> usize {
    (VIDEO_WIDTH * VIDEO_HEIGHT * 4) as usize
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
    check_render(
        unsafe { mpv::mpv_render_context_create(&mut render, handle, params.as_mut_ptr()) },
        "libmpv render context",
    )
    .map_err(|error| error.to_string())?;
    Ok(render)
}

/// A rejected render call that keeps the libmpv code, so the format fallback
/// can tell a rejected target from every other failure.
struct RenderError {
    what: &'static str,
    code: c_int,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: libmpv error {}", self.what, self.code)
    }
}

/// A render-thread-safe error check. It must not call `mpv_error_string` or any
/// other non-render client API function.
fn check_render(code: c_int, what: &'static str) -> Result<(), RenderError> {
    if code >= 0 {
        return Ok(());
    }
    Err(RenderError { what, code })
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
    pub const EVENT_SHUTDOWN: c_int = 1;
    pub const EVENT_LOG_MESSAGE: c_int = 2;
    pub const EVENT_END_FILE: c_int = 7;
    pub const EVENT_FILE_LOADED: c_int = 8;
    pub const EVENT_VIDEO_RECONFIG: c_int = 17;
    pub const EVENT_QUEUE_OVERFLOW: c_int = 24;
    pub const END_FILE_REASON_EOF: c_int = 0;
    pub const END_FILE_REASON_STOP: c_int = 2;
    pub const END_FILE_REASON_QUIT: c_int = 3;
    pub const END_FILE_REASON_ERROR: c_int = 4;
    pub const END_FILE_REASON_REDIRECT: c_int = 5;

    pub const LOG_LEVEL_WARN: c_int = 30;

    pub const ERROR_UNSUPPORTED: c_int = -18;
    #[cfg(test)]
    pub const ERROR_INVALID_PARAMETER: c_int = -4;

    pub const FORMAT_STRING: c_int = 1;
    pub const FORMAT_FLAG: c_int = 3;
    pub const FORMAT_INT64: c_int = 4;
    pub const FORMAT_DOUBLE: c_int = 5;
    pub const FORMAT_NODE: c_int = 6;
    pub const FORMAT_NODE_ARRAY: c_int = 7;
    pub const FORMAT_NODE_MAP: c_int = 8;

    pub const PARAM_API_TYPE: c_int = 1;
    pub const PARAM_BLOCK_FOR_TARGET_TIME: c_int = 12;
    pub const PARAM_SKIP_RENDERING: c_int = 13;
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
        pub fn mpv_wakeup(ctx: *mut Handle);
        pub fn mpv_error_string(error: c_int) -> *const c_char;
        pub fn mpv_free(data: *mut c_void);
        pub fn mpv_free_node_contents(node: *mut Node);
        pub fn mpv_terminate_destroy(ctx: *mut Handle);
        pub fn mpv_render_context_create(
            res: *mut *mut Render,
            ctx: *mut Handle,
            params: *mut Param,
        ) -> c_int;
        pub fn mpv_render_context_set_update_callback(
            ctx: *mut Render,
            callback: Option<extern "C" fn(*mut c_void)>,
            callback_ctx: *mut c_void,
        );
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
    use std::sync::mpsc;
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
            // These tests have no Rune window, so mark the surface visible.
            engine.note_draw();
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

    fn wait_for_pushes(engine: &MpvEngine, target: u64) {
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

    fn wait_for_loaded(engine: &MpvEngine) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !engine.state().is_loaded() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(engine.state().is_loaded(), "{:?}", engine.state());
    }

    fn wait_for_paused(engine: &MpvEngine) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.state() != PlaybackState::Paused && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(engine.state(), PlaybackState::Paused);
    }

    fn wait_for_seek_flags(engine: &MpvEngine, wanted: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.last_seek_flags().as_deref() != Some(wanted) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(engine.last_seek_flags().as_deref(), Some(wanted));
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
    fn every_built_in_script_loader_is_off_before_initialization() {
        // The literal names are the spec: the shared list must carry every
        // one, and the generated options must carry every entry of the shared
        // list, so a loader added there cannot miss this test.
        for name in [
            "load-scripts",
            "load-auto-profiles",
            "load-commands",
            "load-console",
            "load-osd-console",
            "load-positioning",
            "load-select",
            "load-stats-overlay",
            "load-context-menu",
            "osc",
        ] {
            assert!(
                BUILT_IN_SCRIPT_LOADERS.contains(&name),
                "{name} is missing from the script loader list"
            );
        }
        let options = startup_options(true);
        for name in BUILT_IN_SCRIPT_LOADERS {
            assert!(
                options.contains(&(*name, "no")),
                "{name} must be off before initialization: {options:?}"
            );
        }
    }

    #[test]
    fn every_built_in_script_loader_is_off_after_initialization() {
        let engine = engine();
        for name in BUILT_IN_SCRIPT_LOADERS {
            assert_eq!(
                engine.core.get_flag(name),
                Some(false),
                "{name} must be accepted and off after initialization"
            );
        }
    }

    #[test]
    fn a_video_reconfig_signals_the_frame_start() {
        let mut engine = engine();
        let event = mpv::Event {
            event_id: mpv::EVENT_VIDEO_RECONFIG,
            error: 0,
            reply_userdata: 0,
            data: ptr::null_mut(),
        };
        assert!(!engine.core.handle_event(&event));
        assert!(engine.take_frame_start());
        assert!(!engine.take_frame_start());
    }

    #[test]
    fn an_explicit_pause_before_load_holds_until_the_first_frame() {
        let mut engine = engine();
        engine.set_paused(true).unwrap();
        assert_eq!(engine.core.get_flag("pause"), Some(true));
        assert_eq!(engine.state(), PlaybackState::Empty);

        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);
        assert_eq!(engine.state(), PlaybackState::Paused);
        assert!(engine.take_frame_start());

        engine.set_paused(false).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.state() != PlaybackState::Playing && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(engine.state(), PlaybackState::Playing);
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
        wait_for_paused(&engine);
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
        wait_for_loaded(&engine);
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
        wait_for_paused(&engine);
        thread::sleep(Duration::from_millis(100));
        let state_before = engine.state();
        let frames = engine.frames();
        let wakes_before = wakes.load(Ordering::Relaxed);

        engine.seek(0.5);
        wait_for_seek_flags(&engine, "absolute+exact");
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
        wait_for_paused(&engine);
        thread::sleep(Duration::from_millis(150));
        let frames = engine.frames();

        // rgbw.mkv has keyframes at 0.0, 0.5 and 0.75 s. 0.25 s is between
        // the first two keyframes, so a keyframe seek cannot show it.
        engine.seek_preview(0.25);
        wait_for_seek_flags(&engine, "absolute+exact");
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
        wait_for_paused(&engine);
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
        wait_for_paused(&engine);
        thread::sleep(Duration::from_millis(100));
        let state_before = engine.state();
        let frames = engine.frames();

        engine.seek_preview(0.5);
        wait_for_seek_flags(&engine, "absolute+exact");
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

    #[test]
    fn the_visibility_skip_waits_for_three_undrawn_pushes() {
        assert!(!skip_software_render(0, 0));
        assert!(!skip_software_render(2, 0));
        assert!(skip_software_render(3, 0));
        assert!(skip_software_render(9, 0));
    }

    #[test]
    fn a_completed_draw_resets_the_visibility_skip() {
        assert!(!skip_software_render(3, 1));
        assert!(skip_software_render(4, 1));
        assert!(!skip_software_render(3, 3));
        assert!(skip_software_render(6, 3));
    }

    #[test]
    fn bgra_is_the_primary_target_without_the_alpha_pass() {
        assert_eq!(SoftwareFormat::BGRA.label(), "bgra");
        assert!(!SoftwareFormat::BGRA.alpha_pass);
        assert_eq!(SoftwareFormat::BGRA.name, b"bgra\0");
    }

    #[test]
    fn a_rejected_bgra_falls_back_to_bgr0_with_the_alpha_pass() {
        assert_eq!(
            SoftwareFormat::BGRA.fallback(mpv::ERROR_UNSUPPORTED),
            Some(SoftwareFormat::BGR0)
        );
        assert_eq!(SoftwareFormat::BGR0.label(), "bgr0");
        assert!(SoftwareFormat::BGR0.alpha_pass);
        assert_eq!(SoftwareFormat::BGR0.fallback(mpv::ERROR_UNSUPPORTED), None);
    }

    #[test]
    fn a_non_unsupported_render_error_keeps_bgra() {
        assert_eq!(
            SoftwareFormat::BGRA.fallback(mpv::ERROR_INVALID_PARAMETER),
            None,
            "only a rejected format is allowed to change the target"
        );
        assert_eq!(SoftwareFormat::BGRA.label(), "bgra");
    }

    #[test]
    fn the_alpha_pass_makes_every_pixel_opaque() {
        let mut buffer = vec![0u8; 8];
        opaque(&mut buffer);
        assert_eq!(buffer, [0, 0, 0, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn a_hidden_surface_stops_after_three_undrawn_pushes_and_resumes_on_a_draw() {
        let mut engine = engine();
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_pushes(&engine, 3);
        thread::sleep(Duration::from_millis(250));
        assert_eq!(
            engine.frames(),
            3,
            "a hidden surface must stop converting after three undrawn pushes"
        );

        engine.note_draw();
        wait_for_frame_count(&engine, 4);
    }

    #[test]
    fn the_file_loaded_event_reads_pause_once_and_stores_the_state() {
        let engine = engine();
        let before = engine.core.pause_reads.load(Ordering::Relaxed);
        assert!(engine.core.transition(PlaybackState::Loading));
        engine.core.on_file_loaded();
        assert_eq!(
            engine.core.pause_reads.load(Ordering::Relaxed) - before,
            1,
            "the file-loaded event must read pause exactly once"
        );
        assert_eq!(engine.core.state(), PlaybackState::Playing);

        engine.core.set_flag("pause", true).unwrap();
        assert!(engine.core.transition(PlaybackState::Loading));
        engine.core.on_file_loaded();
        assert_eq!(
            engine.core.pause_reads.load(Ordering::Relaxed) - before,
            2,
            "the second file-loaded event reads pause once too"
        );
        assert_eq!(engine.core.state(), PlaybackState::Paused);
    }

    #[test]
    fn the_state_query_reads_no_properties() {
        let mut engine = engine();
        engine.open(&fixture("rgbw.mkv")).unwrap();
        wait_for_frame(&engine);
        wait_for_loaded(&engine);
        let before = engine.core.pause_reads.load(Ordering::Relaxed);
        for _ in 0..20 {
            let _ = engine.state();
        }
        assert_eq!(
            engine.core.pause_reads.load(Ordering::Relaxed),
            before,
            "the state query must not read pause"
        );
    }

    #[test]
    fn an_illegal_transition_is_ignored_and_logged() {
        let (log, _sink, path) = crate::log::testing::log_to_file("engine-transition");
        let engine = MpvEngine::new(true, log).unwrap();
        assert_eq!(engine.core.state(), PlaybackState::Empty);
        assert!(!engine.core.transition(PlaybackState::Playing));
        assert_eq!(engine.core.state(), PlaybackState::Empty);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("ignored playback transition: Empty -> Playing"),
            "{text}"
        );
        drop(engine);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_queue_overflow_is_a_failure_and_pauses_playback() {
        let (log, _sink, path) = crate::log::testing::log_to_file("engine-overflow");
        let engine = MpvEngine::new(true, log).unwrap();
        assert!(engine.core.transition(PlaybackState::Loading));
        assert!(engine.core.transition(PlaybackState::Playing));
        engine.core.on_queue_overflow();
        assert_eq!(engine.core.state(), PlaybackState::Paused);
        assert_eq!(
            engine.core.get_flag("pause"),
            Some(true),
            "the player must be paused before the state says Paused"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("failure: event queue overflow"),
            "overflow needs a failure record, not a warning: {text}"
        );
        drop(engine);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn end_file_stop_and_quit_keep_the_current_state() {
        let engine = engine();
        assert!(engine.core.transition(PlaybackState::Loading));
        assert!(engine.core.transition(PlaybackState::Playing));

        let stop = mpv::EventEndFile {
            reason: mpv::END_FILE_REASON_STOP,
            error: 0,
        };
        engine.core.on_end_file(&stop as *const _ as *mut c_void);
        assert_eq!(engine.core.state(), PlaybackState::Playing);

        let quit = mpv::EventEndFile {
            reason: mpv::END_FILE_REASON_QUIT,
            error: 0,
        };
        engine.core.on_end_file(&quit as *const _ as *mut c_void);
        assert_eq!(engine.core.state(), PlaybackState::Playing);
    }

    #[test]
    fn a_render_join_times_out_and_keeps_the_core_alive() {
        let (log, _sink, path) = crate::log::testing::log_to_file("engine-render-timeout");
        let core = Arc::new(Core::new(true, quiet()).unwrap());
        let core_ref = Arc::downgrade(&core);
        let (done, done_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            thread::sleep(Duration::from_millis(500));
            let _ = done.send(());
        });
        let mut render = RenderThread {
            core: Some(core.clone()),
            stop: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(RenderWake::new()),
            done: done_rx,
            handle: Some(handle),
        };
        let started = Instant::now();
        assert!(!render.join(Duration::from_millis(20), &log));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the join must return at its deadline"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("render thread did not stop within"), "{text}");

        drop(render);
        drop(core);
        assert!(
            core_ref.upgrade().is_some(),
            "a detached render thread may still use the handle, so the core must stay alive"
        );
        let _ = std::fs::remove_file(path);
    }
}
