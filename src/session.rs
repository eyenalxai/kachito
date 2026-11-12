//! One open video and everything that belongs to it.

use crate::engine::{PlaybackState, PlayerEngine, Track, TrackKind};
use crate::export::{AudioSelection, ExportJob, ExportMode, ExportRequest, ExportState};
use crate::timeline::Timeline;
use rune::Tx;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub(crate) const SUPPORTED: &[&str] = &["mkv", "mp4", "m4v", "mov", "webm"];

#[derive(Clone, Debug, PartialEq)]
pub enum TracksState {
    Unloaded,
    Loaded(Vec<Track>),
    Failed(String),
}

enum SourceSlot {
    Empty,
    Pending {
        path: PathBuf,
        previous: Option<PathBuf>,
    },
    Open {
        path: PathBuf,
    },
}

impl SourceSlot {
    fn begin(&mut self, path: PathBuf) {
        let previous = self.active().map(Path::to_path_buf);
        *self = Self::Pending { path, previous };
    }

    fn confirm(&mut self, path: &Path) -> bool {
        let pending = match self {
            Self::Pending { path: pending, .. } => pending.clone(),
            _ => return false,
        };
        if pending.as_path() != path {
            return false;
        }
        *self = Self::Open { path: pending };
        true
    }

    fn fail(&mut self) -> bool {
        let previous = match self {
            Self::Pending { previous, .. } => previous.take(),
            _ => return false,
        };
        *self = previous.map_or(Self::Empty, |path| Self::Open { path });
        true
    }

    /// The open file, or the previous source a pending open keeps active.
    fn active(&self) -> Option<&Path> {
        match self {
            Self::Empty => None,
            Self::Pending { previous, .. } => previous.as_deref(),
            Self::Open { path } => Some(path),
        }
    }

    fn is_pending(&self) -> bool {
        matches!(self, Self::Pending { .. })
    }
}

pub struct PlaybackSession {
    engine: Box<dyn PlayerEngine>,
    source: SourceSlot,
    tracks: TracksState,
    timeline: Timeline,
    export: Option<ExportJob>,
    failure: Option<String>,
    last_state: PlaybackState,
}

impl PlaybackSession {
    pub fn new(engine: Box<dyn PlayerEngine>) -> Self {
        Self {
            engine,
            source: SourceSlot::Empty,
            tracks: TracksState::Unloaded,
            timeline: Timeline::new(),
            export: None,
            failure: None,
            last_state: PlaybackState::Empty,
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
        let pending = std::path::absolute(&path).unwrap_or_else(|_| path.clone());
        self.engine.open(&path)?;
        self.source.begin(pending);
        self.failure = None;
        self.last_state = PlaybackState::Loading;
        Ok(())
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.active()
    }

    #[cfg(test)]
    pub(crate) fn has_pending_source(&self) -> bool {
        self.source.is_pending()
    }

    pub fn state(&self) -> PlaybackState {
        self.engine.state()
    }

    pub fn duration(&self) -> Option<f64> {
        self.engine.duration()
    }

    pub fn position(&self) -> f64 {
        self.engine.position()
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

    pub fn set_waker(&self, waker: rune::Waker) {
        self.engine.set_waker(waker);
    }

    pub fn toggle(&mut self) {
        if self.engine.state().is_loaded() {
            self.engine.toggle();
        }
    }

    pub fn seek(&mut self, seconds: f64) {
        if self.engine.state().is_loaded() {
            self.engine.seek(seconds);
        }
    }

    pub fn seek_fraction(&mut self, fraction: f32) {
        if self.engine.state().is_loaded()
            && let Some(duration) = self.engine.duration()
        {
            self.engine
                .seek(f64::from(fraction.clamp(0.0, 1.0)) * duration);
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
        self.refresh_tracks();
        Ok(())
    }

    pub fn select_subtitle(&mut self, id: Option<i64>) -> Result<(), String> {
        self.engine.select_subtitle(id)?;
        self.refresh_tracks();
        Ok(())
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

    pub fn start_export(&mut self, output: PathBuf, mode: ExportMode) -> Result<(), String> {
        let Some(source) = self.source.active().map(Path::to_path_buf) else {
            return Ok(());
        };
        let start = self.timeline.in_point();
        let end = self.timeline.out_point();
        let audio_index = self.selected_audio_index();
        let audio_count = match &self.tracks {
            TracksState::Loaded(tracks) => tracks_of(tracks, TrackKind::Audio).count(),
            TracksState::Unloaded | TracksState::Failed(_) => 0,
        };
        self.export = None;
        let request = ExportRequest::new(
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
        )
        .map_err(|error| error.to_string())?;
        self.export = Some(ExportJob::start(request).map_err(|error| error.to_string())?);
        Ok(())
    }

    pub fn cancel_export(&self) {
        if let Some(job) = &self.export {
            job.cancel();
        }
    }

    pub fn poll(&mut self, tx: &mut Tx) -> bool {
        let state = self.engine.state();
        if self.last_state != state
            && let PlaybackState::Failed(error) = &state
        {
            self.failure = Some(error.clone());
        }
        self.last_state = state.clone();
        let mut changed = self.confirm_open(&state);
        let playback_ready = !self.source.is_pending()
            && !matches!(&state, PlaybackState::Loading | PlaybackState::Failed(_));
        if playback_ready {
            if let Some(frame_duration) = self.engine.frame_duration()
                && let Some(duration) = self.engine.duration()
                && ((self.timeline.duration() - duration).abs() > 0.01
                    || (self.timeline.frame_duration() - frame_duration).abs() > 1e-9)
            {
                self.timeline.set_duration(duration, frame_duration);
                changed = true;
            }
            let position = self.engine.position();
            if (self.timeline.position() - position).abs() > 0.01 {
                self.timeline.set_position(position);
                changed = true;
            }
            changed |= self.load_tracks(&state);
        }
        if let Some(job) = &self.export {
            if matches!(job.state(), ExportState::Running { .. }) {
                tx.wake_after(Duration::from_millis(100));
            }
            changed = true;
        }
        if matches!(&state, PlaybackState::Playing) {
            tx.wake_after(Duration::from_millis(100));
            changed = true;
        }
        changed
    }

    pub fn take_failure(&mut self) -> Option<String> {
        self.failure.take()
    }

    pub(crate) fn confirm_open(&mut self, state: &PlaybackState) -> bool {
        // Only a pending open consumes an engine state. A settled slot has nothing
        // to confirm, and skipping the path read keeps the poll off the property.
        if !self.source.is_pending() {
            return false;
        }
        if matches!(state, PlaybackState::Failed(_)) {
            return self.source.fail();
        }
        if !state.is_loaded() {
            return false;
        }
        let Some(current) = self.engine.current_path() else {
            return false;
        };
        if !self.source.confirm(&current) {
            return false;
        }
        self.clear_source_data();
        true
    }

    pub(crate) fn load_tracks(&mut self, state: &PlaybackState) -> bool {
        if self.source.is_pending()
            || !state.is_loaded()
            || self.source.active().is_none()
            || !matches!(self.tracks, TracksState::Unloaded)
        {
            return false;
        }
        self.read_tracks();
        true
    }

    fn clear_source_data(&mut self) {
        self.tracks = TracksState::Unloaded;
        self.timeline = Timeline::new();
        self.export = None;
    }

    fn read_tracks(&mut self) {
        let result = self.engine.tracks();
        self.store_tracks(result);
    }

    fn refresh_tracks(&mut self) {
        self.read_tracks();
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
        let last_state = engine.state();
        Self {
            engine,
            source: SourceSlot::Open { path: source },
            tracks,
            timeline: Timeline::new(),
            export: None,
            failure: None,
            last_state,
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

pub(crate) fn tracks_of(tracks: &[Track], kind: TrackKind) -> impl Iterator<Item = &Track> {
    tracks.iter().filter(move |track| track.kind == kind)
}
