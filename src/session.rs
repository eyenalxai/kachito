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

pub struct PlaybackSession {
    engine: Box<dyn PlayerEngine>,
    source: Option<PathBuf>,
    awaiting_source: Option<PathBuf>,
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
            source: None,
            awaiting_source: None,
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
        let awaiting = std::path::absolute(&path).unwrap_or_else(|_| path.clone());
        self.engine.open(&path)?;
        self.awaiting_source = Some(awaiting);
        self.failure = None;
        self.last_state = PlaybackState::Loading;
        Ok(())
    }

    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    #[cfg(test)]
    pub(crate) fn is_awaiting_source(&self) -> bool {
        self.awaiting_source.is_some()
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
        let Some(source) = self.source.clone() else {
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
        let playback_ready = self.awaiting_source.is_none()
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

    fn commit_source(&mut self) {
        if let Some(path) = self.awaiting_source.take() {
            self.source = Some(path);
            self.tracks = TracksState::Unloaded;
            self.timeline = Timeline::new();
            self.export = None;
        }
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
            source: Some(source),
            awaiting_source: None,
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
