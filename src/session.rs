//! One open video and everything that belongs to it.

use crate::engine::{PlaybackSnapshot, PlaybackState, PlayerEngine, Track, TrackKind};
use crate::export::{AudioSelection, ExportJob, ExportMode, ExportRequest, ExportState};
use crate::log::RunLog;
use crate::timeline::Timeline;
use rune::Tx;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) const SUPPORTED: &[&str] = &["mkv", "mp4", "m4v", "mov", "webm"];

const SEEK_DEADLINE: Duration = Duration::from_secs(5);

/// Resume attempts for the automatic pause. A rejected write is retried on
/// the next poll, but a broken engine must not hold the pause forever.
const STARTUP_RESUME_ATTEMPTS: u8 = 5;

pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Clone, Copy, Debug)]
struct PendingSeek {
    target: f64,
    requested_at: Instant,
}

#[derive(Clone, Copy, Debug)]
struct PendingPreview {
    target: f64,
    requested_at: Instant,
    issued: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TracksState {
    Unloaded,
    Loaded(Vec<Track>),
    Failed(String),
}

#[derive(Clone, Debug)]
struct Notice {
    message: String,
    at: u64,
}

#[derive(Debug, Default)]
struct Notices {
    command: Option<Notice>,
    export: Option<Notice>,
    engine: Option<Notice>,
    seek: Option<Notice>,
    command_shown: bool,
    clock: u64,
    changed: bool,
}

impl Notices {
    fn current(&self) -> Option<String> {
        [
            self.command.as_ref(),
            self.export.as_ref(),
            self.engine.as_ref(),
            self.seek.as_ref(),
        ]
        .into_iter()
        .flatten()
        .max_by_key(|notice| notice.at)
        .map(|notice| notice.message.clone())
    }

    fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    fn record_export(&mut self, message: String) {
        self.export = Some(self.new(message));
        self.changed = true;
    }

    fn record_seek(&mut self, message: String) {
        self.seek = Some(self.new(message));
    }

    fn record_command(&mut self, failure: &str) -> bool {
        if self.command_shown {
            return false;
        }
        let message = format!("The player could not apply that command: {failure}");
        self.command = Some(self.new(message));
        true
    }

    fn observe_engine(&mut self, state: &PlaybackState) -> bool {
        let PlaybackState::Failed(message) = state else {
            self.engine = None;
            return false;
        };
        if self
            .engine
            .as_ref()
            .is_some_and(|notice| notice.message == *message)
        {
            return false;
        }
        let message = message.clone();
        self.engine = Some(self.new(message));
        true
    }

    /// The command notice is raised only once it is the one on screen.
    fn resolve(&mut self) {
        if self.command_shown {
            return;
        }
        let Some(command) = self.command.as_ref() else {
            return;
        };
        let command_at = command.at;
        let other = self
            .export
            .as_ref()
            .map(|notice| notice.at)
            .max(self.engine.as_ref().map(|notice| notice.at))
            .max(self.seek.as_ref().map(|notice| notice.at));
        if other.is_none_or(|other| command_at > other) {
            self.command_shown = true;
        }
    }

    fn reset_command(&mut self) {
        self.command = None;
        self.command_shown = false;
        self.changed = true;
    }

    fn reset_engine(&mut self) {
        self.engine = None;
    }

    fn new(&mut self, message: String) -> Notice {
        self.clock += 1;
        Notice {
            message,
            at: self.clock,
        }
    }
}

pub struct PlaybackSession {
    engine: Box<dyn PlayerEngine>,
    log: Arc<RunLog>,
    clock: Arc<dyn Clock>,
    source: Option<PathBuf>,
    awaiting_source: Option<PathBuf>,
    tracks: TracksState,
    timeline: Timeline,
    scrubbing: bool,
    pending_seek: Option<PendingSeek>,
    pending_preview: Option<PendingPreview>,
    /// True while the engine holds a pause this session set for the first
    /// file, before its first frame. A user toggle relinquishes it, and the
    /// first frame clears it.
    startup_pause: bool,
    /// True once the startup phase ended: the engine reported its first frame
    /// start or the user toggled playback. Only an open before this point
    /// holds the startup pause, so a later open leaves the playback pause
    /// where the user put it.
    startup_complete: bool,
    /// Rejected resume writes since the automatic pause was set.
    startup_resume_attempts: u8,
    export: Option<ExportJob>,
    notices: Notices,
    /// The last snapshot this session reported. The view reads it directly.
    snapshot: PlaybackSnapshot,
    reported_state: PlaybackState,
    reported_seeking: bool,
    reported_export: Option<ExportState>,
    reported_tracks: Option<Result<Vec<Track>, String>>,
}

impl PlaybackSession {
    pub fn new(engine: Box<dyn PlayerEngine>, log: Arc<RunLog>) -> Self {
        Self::with_clock(engine, log, Arc::new(SystemClock))
    }

    pub fn with_clock(
        engine: Box<dyn PlayerEngine>,
        log: Arc<RunLog>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let snapshot = engine.snapshot();
        Self {
            engine,
            log,
            clock,
            source: None,
            awaiting_source: None,
            tracks: TracksState::Unloaded,
            timeline: Timeline::new(),
            scrubbing: false,
            pending_seek: None,
            pending_preview: None,
            startup_pause: false,
            startup_complete: false,
            startup_resume_attempts: 0,
            export: None,
            notices: Notices::default(),
            reported_state: snapshot.state.clone(),
            reported_seeking: snapshot.seeking,
            reported_export: None,
            reported_tracks: None,
            snapshot,
        }
    }

    pub fn open(&mut self, path: PathBuf) -> Result<(), String> {
        if path.to_string_lossy().contains("://") {
            return Err("Remote URLs are not supported yet.".into());
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !SUPPORTED.contains(&extension.as_str()) {
            return Err(format!("{} is not a supported video file.", path.display()));
        }
        if !path.is_file() {
            return Err(format!("{} is not a readable video file.", path.display()));
        }
        let awaiting = std::path::absolute(&path).unwrap_or_else(|_| path.clone());
        // Pause before the load so picture and sound start together. This is
        // only for a file opened during the startup phase; once a frame has
        // started or the user has toggled, a later open must leave the
        // playback pause alone.
        let automatic = !self.startup_complete;
        let paused = if automatic {
            let paused = self.engine.set_paused(true);
            if let Err(error) = &paused {
                self.log
                    .warn(format!("startup pause was not applied: {error}"));
            }
            // An automatic pause is not a user command, so its rejection is
            // not a notice. The run log keeps the engine's property failure.
            self.engine.take_command_failure();
            paused.is_ok()
        } else {
            false
        };
        let opened = self.engine.open(&path);
        // A rejected loadfile is the engine failure, not a command notice.
        self.engine.take_command_failure();
        if let Err(error) = &opened {
            self.log.failure(format!("load failed: {error}"));
        }
        opened?;
        self.startup_pause = paused;
        self.startup_resume_attempts = 0;
        self.snapshot = self.engine.snapshot();
        self.notices.reset_command();
        self.notices.reset_engine();
        self.awaiting_source = Some(awaiting);
        Ok(())
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn is_awaiting_source(&self) -> bool {
        self.awaiting_source.is_some()
    }

    /// Takes the engine's latest snapshot without running a poll, so tests can
    /// drive the engine directly and then read the state the next poll sees.
    #[cfg(test)]
    pub(crate) fn refresh_snapshot(&mut self) {
        self.snapshot = self.engine.snapshot();
    }

    #[cfg(test)]
    pub(crate) fn is_scrubbing(&self) -> bool {
        self.scrubbing
    }

    pub fn state(&self) -> PlaybackState {
        self.snapshot.state.clone()
    }

    pub fn duration(&self) -> Option<f64> {
        self.snapshot.duration
    }

    pub fn position(&self) -> f64 {
        self.snapshot.position
    }

    pub fn is_seeking(&self) -> bool {
        self.snapshot.seeking
    }

    pub fn surface(&self) -> rune::NativeSurface {
        self.engine.surface()
    }

    pub fn tracks(&self) -> &TracksState {
        &self.tracks
    }

    pub fn timeline(&self) -> &Timeline {
        &self.timeline
    }

    pub fn timeline_mut(&mut self) -> &mut Timeline {
        &mut self.timeline
    }

    pub fn export_state(&self) -> Option<ExportState> {
        self.export.as_ref().map(ExportJob::state)
    }

    #[cfg(test)]
    pub(crate) fn export_error(&self) -> Option<&str> {
        self.notices
            .export
            .as_ref()
            .map(|notice| notice.message.as_str())
    }

    pub fn error(&self) -> Option<String> {
        self.notices.current()
    }

    pub fn set_waker(&self, waker: rune::Waker) {
        self.engine.set_waker(waker);
    }

    pub fn toggle(&mut self) {
        // Only a toggle that changes playback hands the pause to the user.
        // On an empty screen or while a file is loading there is nothing to
        // toggle, so the startup phase stays open for the file that comes
        // next.
        if self.engine.snapshot().state.is_loaded() {
            // A user toggle owns the pause from here; the first frame must
            // not clear what the user chose. The startup phase is over, so
            // neither the first frame nor a later open may resume over the
            // user's choice.
            self.startup_pause = false;
            self.startup_complete = true;
            self.engine.toggle();
            self.snapshot = self.engine.snapshot();
        }
    }

    pub fn seek(&mut self, seconds: f64) {
        if !seconds.is_finite() {
            return;
        }
        let engine = self.engine.snapshot();
        let Some(seconds) = self.clamp_target(seconds, &engine.state, engine.duration) else {
            self.scrubbing = false;
            self.pending_seek = None;
            self.pending_preview = None;
            return;
        };
        let from_preview = self.scrubbing;
        self.pending_preview = None;
        let settled = from_preview
            && matches!(engine.state, PlaybackState::Paused | PlaybackState::Playing)
            && !engine.seeking
            && (engine.position - seconds).abs() <= seek_tolerance(&engine);
        if settled {
            self.scrubbing = false;
            self.pending_seek = None;
            return;
        }
        self.scrubbing = true;
        self.pending_seek = Some(PendingSeek {
            target: seconds,
            requested_at: self.clock.now(),
        });
        self.engine.seek(seconds);
        self.snapshot = self.engine.snapshot();
    }

    pub fn seek_preview(&mut self, seconds: f64) {
        if !seconds.is_finite() {
            return;
        }
        let engine = self.engine.snapshot();
        let Some(seconds) = self.clamp_target(seconds, &engine.state, engine.duration) else {
            return;
        };
        // A new drag supersedes a final seek that has not landed.
        self.scrubbing = true;
        self.pending_seek = None;
        self.pending_preview = Some(PendingPreview {
            target: seconds,
            requested_at: self.clock.now(),
            issued: false,
        });
        if !engine.seeking {
            self.issue_preview();
        }
        self.snapshot = self.engine.snapshot();
    }

    fn clamp_target(
        &self,
        seconds: f64,
        state: &PlaybackState,
        duration: Option<f64>,
    ) -> Option<f64> {
        if !state.is_loaded() {
            return None;
        }
        Some(match duration {
            Some(duration) => seconds.clamp(0.0, duration),
            None => seconds.max(0.0),
        })
    }

    fn issue_preview(&mut self) {
        let Some(preview) = self.pending_preview.as_mut() else {
            return;
        };
        if preview.issued {
            return;
        }
        let target = preview.target;
        preview.issued = true;
        self.engine.seek_preview(target);
    }

    pub fn set_scrubbing(&mut self, scrubbing: bool) {
        self.scrubbing = scrubbing;
        self.pending_seek = None;
        self.pending_preview = None;
    }

    pub fn seek_fraction(&mut self, fraction: f32) {
        if let Some(duration) = self.engine.snapshot().duration {
            self.seek(f64::from(fraction.clamp(0.0, 1.0)) * duration);
        }
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.engine.set_volume(volume);
    }

    pub fn set_speed(&mut self, speed: f32) {
        self.engine.set_speed(speed);
    }

    pub fn select_audio(&mut self, id: i64) -> Result<(), String> {
        self.engine.select_audio(id)?;
        self.mark_selected(TrackKind::Audio, Some(id));
        Ok(())
    }

    pub fn select_subtitle(&mut self, id: Option<i64>) -> Result<(), String> {
        self.engine.select_subtitle(id)?;
        self.mark_selected(TrackKind::Subtitle, id);
        Ok(())
    }

    fn mark_selected(&mut self, kind: TrackKind, id: Option<i64>) {
        let TracksState::Loaded(tracks) = &mut self.tracks else {
            return;
        };
        for track in tracks.iter_mut().filter(|track| track.kind == kind) {
            track.selected = Some(track.id) == id;
        }
        // The next poll must agree with the cached list instead of reporting
        // the same optimistic selection to the view again.
        self.reported_tracks = Some(Ok(tracks.clone()));
    }

    pub fn selected_audio_index(&self) -> Option<usize> {
        let TracksState::Loaded(tracks) = &self.tracks else {
            return None;
        };
        let audio: Vec<&Track> = tracks_of(tracks, TrackKind::Audio).collect();
        if audio.is_empty() {
            None
        } else {
            Some(audio.iter().position(|track| track.selected).unwrap_or(0))
        }
    }

    pub fn start_export(&mut self, output: PathBuf, mode: ExportMode) {
        let Some(source) = self.source.clone() else {
            return;
        };
        let start = self.timeline.in_point();
        let end = self.timeline.out_point();
        let audio_index = self.selected_audio_index();
        let audio_count = match &self.tracks {
            TracksState::Loaded(tracks) => tracks_of(tracks, TrackKind::Audio).count(),
            TracksState::Unloaded | TracksState::Failed(_) => 0,
        };
        self.export = None;
        self.notices.export = None;
        let request = match ExportRequest::new(
            source,
            start,
            end,
            AudioSelection {
                index: audio_index,
                count: audio_count,
            },
            self.timeline.frame_duration(),
            mode,
            output,
        ) {
            Ok(request) => request,
            Err(error) => {
                self.set_export_error(error.to_string());
                return;
            }
        };
        match ExportJob::start(request) {
            Ok(job) => self.export = Some(job),
            Err(error) => self.set_export_error(error.to_string()),
        }
    }

    pub fn cancel_export(&self) {
        if let Some(job) = &self.export {
            job.cancel();
        }
    }

    pub fn set_export_error(&mut self, error: String) {
        self.notices.record_export(error);
    }

    /// Runs one UI poll against the engine snapshot. `clock_visible` is true
    /// only while the transport (or another visible clock) is on screen, so a
    /// hidden transport updates the timeline without rebuilding the view.
    pub fn poll(&mut self, tx: &mut Tx, clock_visible: bool) -> bool {
        let previous_notice = self.error();
        let snapshot = self.engine.snapshot();
        let state = snapshot.state.clone();

        let mut changed = self.clear_startup_pause();
        changed |= self.notices.take_changed();
        changed |= self.notices.observe_engine(&state);
        changed |= self.observe_command_failure();
        changed |= self.confirm_open(&state);
        if self.reported_state != state {
            self.reported_state = state.clone();
            changed = true;
        }
        if self.reported_seeking != snapshot.seeking {
            self.reported_seeking = snapshot.seeking;
            changed = true;
        }

        let playback_ready = self.awaiting_source.is_none()
            && !matches!(&state, PlaybackState::Loading | PlaybackState::Failed(_));
        let export_running = matches!(self.export_state(), Some(ExportState::Running { .. }));
        if playback_ready
            && let Some(frame_duration) = snapshot.frame_duration
            && let Some(duration) = snapshot.duration
            && ((self.timeline.duration() - duration).abs() > 0.01
                || (self.timeline.frame_duration() - frame_duration).abs() > 1e-9)
        {
            self.timeline.set_duration(duration, frame_duration);
            // The timeline itself must stay current, but its duration is only
            // on screen in the transport clock or the running export tray.
            changed |= clock_visible || export_running;
        }
        changed |= self.resolve_pending_seek(&snapshot, playback_ready);
        changed |= self.resolve_pending_preview(&snapshot, playback_ready);
        self.notices.resolve();
        changed |= self.error() != previous_notice;
        if playback_ready {
            if self.scrubbing {
                if !snapshot.seeking {
                    self.issue_preview();
                }
                tx.wake_after(Duration::from_millis(16));
                changed = true;
            }
            if !self.scrubbing && (self.timeline.position() - snapshot.position).abs() > 0.01 {
                self.timeline.set_position(snapshot.position);
                // A hidden clock still keeps the timeline current, but it does
                // not ask the view to rebuild for a value it does not show.
                changed |= clock_visible;
            }
            if snapshot.tracks.is_some() {
                changed |= self.sync_tracks(snapshot.tracks.as_ref());
            } else {
                changed |= self.load_tracks(&state);
            }
        }
        if let Some(job) = &self.export {
            let export = job.state();
            let export_changed = self.reported_export.as_ref() != Some(&export);
            if matches!(export, ExportState::Running { .. }) {
                tx.wake_after(Duration::from_millis(100));
            }
            changed |= export_changed;
            self.reported_export = Some(export);
        } else if self.reported_export.take().is_some() {
            changed = true;
        }
        if matches!(&state, PlaybackState::Playing) && clock_visible {
            tx.wake_after(Duration::from_millis(100));
        }
        if let Some(pending) = self.pending_seek {
            let elapsed = self
                .clock
                .now()
                .saturating_duration_since(pending.requested_at);
            tx.wake_after(SEEK_DEADLINE.saturating_sub(elapsed));
        }
        if let Some(preview) = self.pending_preview {
            let elapsed = self
                .clock
                .now()
                .saturating_duration_since(preview.requested_at);
            tx.wake_after(SEEK_DEADLINE.saturating_sub(elapsed));
        }
        if self.startup_pause && self.startup_complete {
            tx.wake_after(Duration::from_millis(100));
        }
        self.snapshot = snapshot;
        changed
    }

    fn clear_startup_pause(&mut self) -> bool {
        // Consume the signal even when a user took the pause over, so a stale
        // first frame cannot clear the next file's startup pause.
        if self.engine.take_frame_start() {
            self.startup_complete = true;
        }
        if !self.startup_pause || !self.startup_complete {
            return false;
        }
        // The flag stays until the write succeeds, so a rejected resume is
        // retried instead of leaving the automatic pause behind.
        match self.engine.set_paused(false) {
            Ok(()) => {
                self.startup_pause = false;
                true
            }
            Err(error) => {
                self.startup_resume_attempts += 1;
                if self.startup_resume_attempts >= STARTUP_RESUME_ATTEMPTS {
                    self.startup_pause = false;
                    self.log.warn(format!(
                        "automatic pause was not cleared after {STARTUP_RESUME_ATTEMPTS} attempts: {error}"
                    ));
                } else {
                    self.log
                        .warn(format!("automatic pause was not cleared: {error}"));
                }
                false
            }
        }
    }

    fn resolve_pending_seek(&mut self, snapshot: &PlaybackSnapshot, playback_ready: bool) -> bool {
        let Some(pending) = self.pending_seek else {
            return false;
        };
        let landed = playback_ready
            && snapshot.state.is_loaded()
            && !snapshot.seeking
            && (snapshot.position - pending.target).abs() <= seek_tolerance(snapshot);
        if landed {
            self.pending_seek = None;
            self.scrubbing = false;
            return false;
        }
        if self
            .clock
            .now()
            .saturating_duration_since(pending.requested_at)
            < SEEK_DEADLINE
        {
            return false;
        }
        self.expire_seek(pending.target, snapshot.position)
    }

    fn resolve_pending_preview(
        &mut self,
        snapshot: &PlaybackSnapshot,
        playback_ready: bool,
    ) -> bool {
        let Some(preview) = self.pending_preview else {
            return false;
        };
        if preview.issued {
            let landed = playback_ready
                && snapshot.state.is_loaded()
                && !snapshot.seeking
                && (snapshot.position - preview.target).abs() <= seek_tolerance(snapshot);
            if landed {
                self.pending_preview = None;
                return false;
            }
        }
        if self
            .clock
            .now()
            .saturating_duration_since(preview.requested_at)
            < SEEK_DEADLINE
        {
            return false;
        }
        self.expire_seek(preview.target, snapshot.position)
    }

    fn expire_seek(&mut self, target: f64, position: f64) -> bool {
        self.pending_seek = None;
        self.pending_preview = None;
        self.scrubbing = false;
        self.timeline.set_position(position);
        self.notices.record_seek(
            "The seek did not finish in time. Playback continues from the current position.".into(),
        );
        self.log.warn(format!(
            "notice: seek deadline: target {:.3}s, position {:.3}s",
            target, position
        ));
        true
    }

    fn observe_command_failure(&mut self) -> bool {
        let Some(failure) = self.engine.take_command_failure() else {
            return false;
        };
        if !self.notices.record_command(&failure) {
            return false;
        }
        self.log
            .info(format!("notice: rejected command: {failure}"));
        true
    }

    pub(crate) fn confirm_open(&mut self, state: &PlaybackState) -> bool {
        self.notices.observe_engine(state);
        if self.awaiting_source.is_none() {
            return false;
        }
        let current = self.engine.current_path();
        if current.as_deref() != self.awaiting_source.as_deref() {
            return false;
        }
        if matches!(state, PlaybackState::Playing | PlaybackState::Paused) {
            self.commit_source();
            true
        } else if matches!(state, PlaybackState::Failed(_)) {
            self.awaiting_source = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn load_tracks(&mut self, state: &PlaybackState) -> bool {
        if self.awaiting_source.is_some()
            || !state.is_loaded()
            || self.source.is_none()
            || !matches!(self.tracks, TracksState::Unloaded)
        {
            return false;
        }
        self.read_tracks();
        true
    }

    /// Applies a track list observed in the engine snapshot. A selection the
    /// session already applied optimistically is not reported again.
    fn sync_tracks(&mut self, tracks: Option<&Result<Vec<Track>, String>>) -> bool {
        let Some(tracks) = tracks else {
            return false;
        };
        if self.awaiting_source.is_some() || self.source.is_none() {
            return false;
        }
        if self.reported_tracks.as_ref() == Some(tracks) {
            return false;
        }
        self.reported_tracks = Some(tracks.clone());
        self.store_tracks(tracks.clone());
        true
    }

    fn commit_source(&mut self) {
        if let Some(path) = self.awaiting_source.take() {
            self.source = Some(path);
            self.tracks = TracksState::Unloaded;
            self.reported_tracks = None;
            self.timeline = Timeline::new();
            self.scrubbing = false;
            self.pending_seek = None;
            self.pending_preview = None;
            self.export = None;
            self.notices.export = None;
            self.notices.seek = None;
        }
    }

    fn read_tracks(&mut self) {
        let result = self.engine.tracks();
        self.store_tracks(result);
    }

    fn store_tracks(&mut self, result: Result<Vec<Track>, String>) {
        self.tracks = match result {
            Ok(tracks) => TracksState::Loaded(tracks),
            Err(error) => TracksState::Failed(error),
        };
    }

    #[cfg(test)]
    pub(crate) fn with_source(
        engine: Box<dyn PlayerEngine>,
        source: PathBuf,
        tracks: TracksState,
    ) -> Self {
        Self::with_source_and_log(engine, source, tracks, Arc::new(RunLog::disabled()))
    }

    #[cfg(test)]
    pub(crate) fn with_source_and_log(
        engine: Box<dyn PlayerEngine>,
        source: PathBuf,
        tracks: TracksState,
        log: Arc<RunLog>,
    ) -> Self {
        Self::with_source_clock_and_log(engine, source, tracks, log, Arc::new(SystemClock))
    }

    #[cfg(test)]
    pub(crate) fn with_source_and_clock(
        engine: Box<dyn PlayerEngine>,
        source: PathBuf,
        tracks: TracksState,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self::with_source_clock_and_log(engine, source, tracks, Arc::new(RunLog::disabled()), clock)
    }

    #[cfg(test)]
    pub(crate) fn with_source_clock_and_log(
        engine: Box<dyn PlayerEngine>,
        source: PathBuf,
        tracks: TracksState,
        log: Arc<RunLog>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let snapshot = engine.snapshot();
        Self {
            engine,
            log,
            clock,
            source: Some(source),
            awaiting_source: None,
            tracks,
            timeline: Timeline::new(),
            scrubbing: false,
            pending_seek: None,
            pending_preview: None,
            startup_pause: false,
            startup_complete: false,
            startup_resume_attempts: 0,
            export: None,
            notices: Notices::default(),
            reported_state: snapshot.state.clone(),
            reported_seeking: snapshot.seeking,
            reported_export: None,
            reported_tracks: None,
            snapshot,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_tracks(&mut self, tracks: TracksState) {
        self.tracks = tracks;
    }

    #[cfg(test)]
    pub(crate) fn set_export(&mut self, export: Option<ExportJob>) {
        self.export = export;
    }
}

fn seek_tolerance(snapshot: &PlaybackSnapshot) -> f64 {
    snapshot.frame_duration.unwrap_or(0.05)
}

pub(crate) fn tracks_of(tracks: &[Track], kind: TrackKind) -> impl Iterator<Item = &Track> {
    tracks.iter().filter(move |track| track.kind == kind)
}
