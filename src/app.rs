//! The Kachito screen: one file, one video surface, one transport bar.

use crate::engine::{
    MpvEngine, PlaybackState, PlayerEngine, Track, TrackKind, VIDEO_HEIGHT, VIDEO_WIDTH,
};
use crate::export::{ExportJob, ExportMode, ExportRequest, ExportState};
use crate::timeline::Timeline;
use rune::tok::{self, type_};
use rune::{
    AppSpec, Cx, Drag, El, FileDialogOptions, Phase, SurfaceFit, Tx, Waker, col, el, row, text,
};
use std::path::PathBuf;
use std::time::Duration;

const SUPPORTED: &[&str] = &["mkv", "mp4", "m4v", "mov", "webm"];
const SPEEDS: &[f32] = &[0.5, 1.0, 1.25, 1.5, 2.0];

#[derive(Clone, Debug, PartialEq)]
enum TracksState {
    Unloaded,
    Loaded(Vec<Track>),
    Failed(String),
}

fn tracks_of(tracks: &[Track], kind: TrackKind) -> impl Iterator<Item = &Track> {
    tracks.iter().filter(move |track| track.kind == kind)
}

struct App {
    engine: Option<Box<dyn PlayerEngine>>,
    path: Option<PathBuf>,
    pending_path: Option<PathBuf>,
    error: Option<String>,
    volume: f32,
    speed: f32,
    last_dir: Option<PathBuf>,
    tracks: TracksState,
    timeline: Timeline,
    export: Option<ExportJob>,
    export_mode: ExportMode,
    export_error: Option<String>,
}

impl App {
    fn new() -> Self {
        let mut app = Self {
            engine: None,
            path: None,
            pending_path: None,
            error: None,
            volume: 1.0,
            speed: 1.0,
            last_dir: None,
            tracks: TracksState::Unloaded,
            timeline: Timeline::new(),
            export: None,
            export_mode: ExportMode::Precise,
            export_error: None,
        };
        match MpvEngine::new(rune::is_headless()) {
            Ok(engine) => app.engine = Some(Box::new(engine)),
            Err(error) => app.error = Some(error),
        }
        if let Some(path) = rune::extra_args().first() {
            app.open(PathBuf::from(path));
        }
        if rune::is_headless() && app.pending_path.is_some() {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while app.engine.as_ref().is_some_and(|engine| {
                engine.frames() == 0 && !matches!(engine.state(), PlaybackState::Failed(_))
            }) && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            let loaded = app
                .engine
                .as_ref()
                .is_some_and(|engine| engine.state().is_loaded());
            if loaded {
                app.commit_pending_path();
            }
        }
        app
    }

    fn commit_pending_path(&mut self) {
        if let Some(path) = self.pending_path.take() {
            self.path = Some(path);
            self.tracks = TracksState::Unloaded;
            self.timeline = Timeline::new();
        }
    }

    fn sync_pending_path(&mut self, state: &PlaybackState) -> bool {
        if self.pending_path.is_some()
            && matches!(
                state,
                PlaybackState::Ready | PlaybackState::Playing | PlaybackState::Paused
            )
        {
            self.commit_pending_path();
            true
        } else if self.pending_path.is_some() && matches!(state, PlaybackState::Failed(_)) {
            self.pending_path = None;
            true
        } else {
            false
        }
    }

    fn load_tracks(&mut self, state: &PlaybackState) -> bool {
        if self.pending_path.is_some()
            || !state.is_loaded()
            || self.path.is_none()
            || !matches!(self.tracks, TracksState::Unloaded)
        {
            return false;
        }
        let Some(engine) = self.engine.as_ref() else {
            return false;
        };
        let result = engine.tracks();
        self.store_tracks(result);
        true
    }

    fn store_tracks(&mut self, result: Result<Vec<Track>, String>) {
        self.tracks = match result {
            Ok(tracks) => TracksState::Loaded(tracks),
            Err(error) => TracksState::Failed(error),
        };
    }

    fn open(&mut self, path: PathBuf) {
        if path.to_string_lossy().contains("://") {
            self.error = Some("Remote URLs are not supported yet.".into());
            return;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !SUPPORTED.contains(&extension.as_str()) {
            self.error = Some(format!("{} is not a supported video file.", path.display()));
            return;
        }
        if !path.is_file() {
            self.error = Some(format!("{} is not a readable video file.", path.display()));
            return;
        }
        let Some(engine) = self.engine.as_mut() else {
            self.error = Some("The playback engine is not available.".into());
            return;
        };
        match engine.open(&path) {
            Ok(()) => {
                self.last_dir = path.parent().map(|parent| parent.to_path_buf());
                self.pending_path = Some(path);
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn open_first(&mut self, files: &[PathBuf]) {
        if let Some(path) = files.first() {
            self.open(path.clone());
        }
    }

    fn toggle(&mut self) {
        if let Some(engine) = self.engine.as_mut()
            && engine.state().is_loaded()
        {
            engine.toggle();
        }
    }

    fn seek_fraction(&mut self, fraction: f32) {
        if let Some(engine) = self.engine.as_mut()
            && engine.state().is_loaded()
            && let Some(duration) = engine.duration()
        {
            engine.seek(f64::from(fraction.clamp(0.0, 1.0)) * duration);
        }
    }

    fn volume_delta(&mut self, delta: f32) {
        self.volume = (self.volume + delta).clamp(0.0, 1.0);
        if let Some(engine) = self.engine.as_mut() {
            engine.set_volume(self.volume);
        }
    }

    fn cycle_speed(&mut self) {
        let index = SPEEDS
            .iter()
            .position(|speed| (*speed - self.speed).abs() < 0.01)
            .unwrap_or(1);
        self.speed = SPEEDS[(index + 1) % SPEEDS.len()];
        if let Some(engine) = self.engine.as_mut() {
            engine.set_speed(self.speed);
        }
    }

    fn refresh_tracks(&mut self) {
        let tracks = self.engine.as_ref().map(|engine| engine.tracks());
        if let Some(result) = tracks {
            self.store_tracks(result);
        }
    }

    fn select_audio(&mut self, id: i64) {
        if let Some(engine) = self.engine.as_mut() {
            match engine.select_audio(id) {
                Ok(()) => self.refresh_tracks(),
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn select_subtitle(&mut self, id: Option<i64>) {
        if let Some(engine) = self.engine.as_mut() {
            match engine.select_subtitle(id) {
                Ok(()) => self.refresh_tracks(),
                Err(error) => self.error = Some(error),
            }
        }
    }

    fn seek_to(&mut self, time: f64) {
        if let Some(engine) = self.engine.as_mut()
            && engine.state().is_loaded()
        {
            engine.seek(time);
        }
        self.timeline.set_position(time);
    }

    fn set_in(&mut self, time: f64) {
        self.timeline.set_in(time);
    }

    fn set_out(&mut self, time: f64) {
        self.timeline.set_out(time);
    }

    fn selected_audio_index(&self) -> Option<usize> {
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

    fn start_export(&mut self, output: PathBuf, start: f64, end: f64, audio_index: Option<usize>) {
        let source = match &self.path {
            Some(path) => path.clone(),
            None => return,
        };
        let audio_count = match &self.tracks {
            TracksState::Loaded(tracks) => tracks_of(tracks, TrackKind::Audio).count(),
            TracksState::Unloaded | TracksState::Failed(_) => 0,
        };
        self.export = None;
        self.export_error = None;
        let request = match ExportRequest::new(
            source,
            start,
            end,
            audio_index,
            audio_count,
            self.export_mode,
            output,
        ) {
            Ok(request) => request,
            Err(error) => {
                self.export_error = Some(error.to_string());
                return;
            }
        };
        match ExportJob::start(request) {
            Ok(job) => self.export = Some(job),
            Err(error) => self.export_error = Some(error.to_string()),
        }
    }

    fn error_text(&self) -> Option<String> {
        self.error
            .clone()
            .or_else(|| self.export_error.clone())
            .or_else(|| {
                self.engine
                    .as_ref()
                    .and_then(|engine| match engine.state() {
                        PlaybackState::Failed(error) => Some(error),
                        _ => None,
                    })
            })
    }
}

fn state() -> App {
    App::new()
}

fn setup(app: &mut App, waker: Waker) {
    if let Some(engine) = app.engine.as_ref() {
        engine.set_waker(waker.clone());
        engine.surface().set_waker(waker);
    }
}

fn poll(app: &mut App, _: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    let state = match app.engine.as_ref() {
        Some(engine) => engine.state(),
        None => return false,
    };
    changed |= app.sync_pending_path(&state);
    let playback_ready = app.pending_path.is_none()
        && !matches!(&state, PlaybackState::Loading | PlaybackState::Failed(_));
    if playback_ready {
        if let Some(duration) = app.engine.as_ref().and_then(|engine| engine.duration())
            && (app.timeline.duration() - duration).abs() > 0.01
        {
            app.timeline.set_duration(duration);
            changed = true;
        }
        let position = app
            .engine
            .as_ref()
            .map(|engine| engine.position())
            .unwrap_or(0.0);
        if (app.timeline.position() - position).abs() > 0.01 {
            app.timeline.set_position(position);
            changed = true;
        }
        changed |= app.load_tracks(&state);
    }
    if let Some(job) = &app.export {
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

fn open_dialog(app: &mut App, tx: &mut Tx) {
    let options = FileDialogOptions {
        title: "Open video".into(),
        directory: app.last_dir.clone(),
        ..Default::default()
    }
    .filter("Video", SUPPORTED);
    tx.open_file(options, |app: &mut App, result, _| match result {
        Ok(Some(path)) => app.open(path),
        Ok(None) => {}
        Err(error) => app.error = Some(error.to_string()),
    });
}

fn export_dialog(app: &mut App, tx: &mut Tx) {
    let Some(source) = &app.path else {
        return;
    };
    let stem = source
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "clip".into());
    let options = FileDialogOptions {
        title: "Export clip".into(),
        directory: app.last_dir.clone(),
        file_name: Some(format!("{stem}-clip.mkv")),
        ..Default::default()
    }
    .filter("Matroska", &["mkv"]);
    let start = app.timeline.in_point();
    let end = app.timeline.out_point();
    let audio = app.selected_audio_index();
    tx.save_file(options, move |app: &mut App, result, _| match result {
        Ok(Some(path)) => app.start_export(path, start, end, audio),
        Ok(None) => {}
        Err(error) => app.export_error = Some(error.to_string()),
    });
}

fn button(label: impl Into<String>, accent: bool, theme: &tok::Theme) -> El {
    el().padding(8.0)
        .radius(8.0)
        .fill(if accent {
            tok::accent(0, true)
        } else {
            theme.card
        })
        .border(1.0, theme.card_border)
        .child(
            text(label)
                .font(type_::LABEL)
                .color(if accent { theme.knob } else { theme.text }),
        )
}

fn open_button(theme: &tok::Theme) -> El {
    button("Open", false, theme).on_press_tx(|app: &mut App, tx: &mut Tx| open_dialog(app, tx))
}

fn track_bar(app: &App, theme: &tok::Theme) -> El {
    let mut bar = row()
        .w_full()
        .padding(8.0)
        .gap(6.0)
        .center_y()
        .fill(theme.bg)
        .child(text("Audio").font(type_::LABEL).color(theme.text_secondary));
    match &app.tracks {
        TracksState::Unloaded => {
            bar = bar.child(
                text("Loading tracks")
                    .key("track-loading")
                    .font(type_::LABEL)
                    .color(theme.text_secondary),
            );
        }
        TracksState::Failed(error) => {
            bar = bar
                .child(
                    text(error.clone())
                        .key("track-error")
                        .font(type_::LABEL)
                        .color(tok::accent(2, true)),
                )
                .child(text("Subs").font(type_::LABEL).color(theme.text_secondary))
                .child(
                    button("Off", true, theme)
                        .key("subs-off")
                        .on_press(|app: &mut App| app.select_subtitle(None)),
                );
        }
        TracksState::Loaded(tracks) => {
            let audio: Vec<&Track> = tracks_of(tracks, TrackKind::Audio).collect();
            let subtitles: Vec<&Track> = tracks_of(tracks, TrackKind::Subtitle).collect();
            let any_subtitle = subtitles.iter().any(|track| track.selected);
            if audio.is_empty() {
                bar = bar.child(
                    text("No audio tracks")
                        .key("no-audio")
                        .font(type_::LABEL)
                        .color(theme.text_secondary),
                );
            }
            for track in audio {
                let id = track.id;
                let label = track.label();
                let selected = track.selected;
                bar = bar.child(
                    button(label, selected, theme)
                        .key(format!("audio-{id}"))
                        .on_press(move |app: &mut App| app.select_audio(id)),
                );
            }
            bar = bar
                .child(text("Subs").font(type_::LABEL).color(theme.text_secondary))
                .child(
                    button("Off", !any_subtitle, theme)
                        .key("subs-off")
                        .on_press(|app: &mut App| app.select_subtitle(None)),
                );
            if subtitles.is_empty() {
                bar = bar.child(
                    text("No subtitles")
                        .key("no-subtitles")
                        .font(type_::LABEL)
                        .color(theme.text_secondary),
                );
            }
            for track in subtitles {
                let id = track.id;
                let label = track.label();
                let selected = track.selected;
                bar = bar.child(
                    button(label, selected, theme)
                        .key(format!("sub-{id}"))
                        .on_press(move |app: &mut App| app.select_subtitle(Some(id))),
                );
            }
        }
    }
    bar
}

fn timeline_bar(app: &App, theme: &tok::Theme) -> El {
    let timeline = &app.timeline;
    let start = timeline.fraction_of(timeline.in_point()) as f32;
    let end = timeline.fraction_of(timeline.out_point()) as f32;
    let position = timeline.fraction_of(timeline.position()) as f32;
    let selection = (end - start).max(0.0);
    let accent = tok::accent(0, true);
    let label = format!(
        "{:.1}s  {} - {}",
        timeline.selection_duration(),
        format_clock(timeline.in_point()),
        format_clock(timeline.out_point())
    );

    let mut bar = row()
        .w_full()
        .padding(8.0)
        .gap(8.0)
        .center_y()
        .fill(theme.card)
        .child(text(label).font(type_::LABEL).color(theme.text_secondary))
        .child(
            el().key("timeline")
                .grow(1.0)
                .h(30.0)
                .on_drag(|app: &mut App, drag: &mut Drag, _: &mut Tx| {
                    if drag.w <= 0.0 {
                        return;
                    }
                    let time = app.timeline.time_at_fraction(f64::from(drag.x / drag.w));
                    if matches!(drag.phase, Phase::Down) {
                        let near_in = (time - app.timeline.in_point()).abs() <= 0.75;
                        let near_out = (time - app.timeline.out_point()).abs() <= 0.75;
                        drag.data = if near_in {
                            1.0
                        } else if near_out {
                            2.0
                        } else {
                            0.0
                        };
                    }
                    match drag.data as i32 {
                        1 => app.set_in(time),
                        2 => app.set_out(time),
                        _ => app.seek_to(time),
                    }
                })
                .child(
                    col()
                        .w_full()
                        .h(30.0)
                        .gap(2.0)
                        .child(
                            row()
                                .w_full()
                                .h(8.0)
                                .radius(4.0)
                                .fill(theme.control_track)
                                .child(el().grow(start.max(0.001)).h(8.0))
                                .child(el().grow(selection.max(0.001)).h(8.0).fill(accent))
                                .child(el().grow((1.0 - end).max(0.001)).h(8.0)),
                        )
                        .child(
                            row()
                                .w_full()
                                .h(20.0)
                                .child(el().grow(position.max(0.001)))
                                .child(el().w(2.0).h(20.0).fill(accent))
                                .child(el().grow((1.0 - position).max(0.001))),
                        ),
                ),
        )
        .child(button("-", false, theme).on_press(|app: &mut App| app.timeline.zoom_out()))
        .child(button("+", false, theme).on_press(|app: &mut App| app.timeline.zoom_in()));

    let export_state = app.export.as_ref().map(ExportJob::state);
    if let Some(state) = &export_state {
        match state {
            ExportState::Running { fraction } => {
                bar = bar
                    .child(
                        text(format!("{}%", (*fraction * 100.0).round()))
                            .key("export-status")
                            .font(type_::LABEL)
                            .color(theme.text_secondary),
                    )
                    .child(
                        button("Cancel", false, theme)
                            .key("cancel-export")
                            .on_press(|app: &mut App| {
                                if let Some(job) = &app.export {
                                    job.cancel();
                                }
                            }),
                    );
            }
            ExportState::Finished => {
                bar = bar.child(
                    text("Exported")
                        .key("export-status")
                        .font(type_::LABEL)
                        .color(theme.text_secondary),
                );
            }
            ExportState::Cancelled => {
                bar = bar.child(
                    text("Cancelled")
                        .key("export-status")
                        .font(type_::LABEL)
                        .color(theme.text_secondary),
                );
            }
            ExportState::Failed(error) => {
                bar = bar.child(
                    text(error.clone())
                        .key("export-status")
                        .font(type_::LABEL)
                        .color(tok::accent(2, true)),
                );
            }
        }
    }

    let export_running = export_state
        .as_ref()
        .is_some_and(|state| matches!(state, ExportState::Running { .. }));
    if !export_running {
        bar = bar
            .child(
                button(app.export_mode.label(), false, theme)
                    .key("export-mode")
                    .on_press(|app: &mut App| app.export_mode = app.export_mode.next()),
            )
            .child(
                button("Export", true, theme)
                    .key("export")
                    .on_press_tx(|app: &mut App, tx: &mut Tx| export_dialog(app, tx)),
            );
    }

    bar
}

fn play_label(state: &PlaybackState) -> &'static str {
    match state {
        PlaybackState::Playing => "Pause",
        PlaybackState::Loading => "Loading",
        PlaybackState::Failed(_) => "Failed",
        _ => "Play",
    }
}

fn view(app: &App, cx: &mut Cx<App>) -> El {
    let theme = tok::theme(true);
    let accent = cx.accent();
    let error = app.error_text();
    let root = col().w_full().h_full().fill(theme.bg);

    let screen = if let Some(path) = &app.path {
        let engine = app.engine.as_ref().unwrap();
        let duration = engine.duration().unwrap_or(0.0);
        let position = engine.position();
        let playback = engine.state();
        let surface = engine.surface();
        let fraction = if duration > 0.0 {
            (position / duration).clamp(0.0, 1.0) as f32
        } else {
            0.0
        };
        let title = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Kachito".into());

        let mut top = row()
            .w_full()
            .padding(10.0)
            .gap(10.0)
            .center_y()
            .fill(theme.card)
            .child(text(title).font(type_::BODY).color(theme.text).grow(1.0));
        if let Some(error) = &error {
            top = top.child(
                text(error.clone())
                    .font(type_::LABEL)
                    .color(tok::accent(2, true)),
            );
        }
        let top = top.child(open_button(&theme));

        let seek = el()
            .key("seek")
            .grow(1.0)
            .h(18.0)
            .on_drag(|app: &mut App, drag: &mut Drag, _: &mut Tx| {
                if drag.w > 0.0 {
                    app.seek_fraction(drag.x / drag.w);
                }
            })
            .child(
                row()
                    .w_full()
                    .h(4.0)
                    .radius(2.0)
                    .fill(theme.control_track)
                    .child(
                        el().grow(fraction.max(0.001))
                            .h(4.0)
                            .radius(2.0)
                            .fill(accent),
                    )
                    .child(el().grow((1.0 - fraction).max(0.001)).h(4.0)),
            );

        let controls = row()
            .w_full()
            .padding(12.0)
            .gap(12.0)
            .center_y()
            .fill(theme.card)
            .child(
                button(play_label(&playback), true, &theme)
                    .key("play")
                    .on_press(|app: &mut App| app.toggle()),
            )
            .child(
                text(format_time(position, duration))
                    .font(type_::LABEL)
                    .color(theme.text_secondary),
            )
            .child(seek)
            .child(button("-", false, &theme).on_press(|app: &mut App| app.volume_delta(-0.1)))
            .child(
                text(format!("{}%", (app.volume * 100.0).round()))
                    .font(type_::LABEL)
                    .color(theme.text_secondary),
            )
            .child(button("+", false, &theme).on_press(|app: &mut App| app.volume_delta(0.1)))
            .child(
                button(format!("{}x", app.speed), false, &theme)
                    .on_press(|app: &mut App| app.cycle_speed()),
            );

        col()
            .w_full()
            .h_full()
            .fill(theme.bg)
            .child(top)
            .child(track_bar(app, &theme))
            .child(
                el().key("video")
                    .w(VIDEO_WIDTH as f32)
                    .h(VIDEO_HEIGHT as f32)
                    .native_surface(&surface, SurfaceFit::Fit),
            )
            .child(timeline_bar(app, &theme))
            .child(controls)
    } else {
        let mut screen = col()
            .w_full()
            .h_full()
            .center()
            .gap(12.0)
            .fill(theme.bg)
            .child(text("Kachito").font(type_::TITLE).color(theme.text))
            .child(
                text("Open a local MKV or MP4")
                    .font(type_::BODY)
                    .color(theme.text_secondary),
            )
            .child(open_button(&theme))
            .child(
                text("or drop a file here")
                    .font(type_::LABEL)
                    .color(theme.text_secondary),
            );
        if let Some(error) = &error {
            screen = screen.child(
                text(error.clone())
                    .font(type_::LABEL)
                    .color(tok::accent(2, true)),
            );
        }
        screen
    };

    root.child(screen)
        .on_drop_files(|app: &mut App, files, _: &mut Tx| app.open_first(files))
}

fn format_time(position: f64, duration: f64) -> String {
    format!("{} / {}", format_clock(position), format_clock(duration))
}

fn format_clock(seconds: f64) -> String {
    let seconds = seconds.max(0.0).round() as u64;
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

pub fn run() {
    rune::run(AppSpec {
        setup: Some(setup),
        poll: Some(poll),
        ..AppSpec::new(
            "Kachito",
            (VIDEO_WIDTH as f32, VIDEO_HEIGHT as f32 + 192.0),
            state,
            view,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};
    use std::sync::{Arc, Mutex};

    struct FakeLog {
        state: PlaybackState,
        tracks_result: Option<Result<Vec<Track>, String>>,
        tracks_reads: usize,
        last_seek: Option<f64>,
        volume: Option<f32>,
        speed: Option<f32>,
        opened: Option<PathBuf>,
        audio: Option<i64>,
        subtitle: Option<i64>,
    }

    impl Default for FakeLog {
        fn default() -> Self {
            Self {
                state: PlaybackState::Paused,
                tracks_result: None,
                tracks_reads: 0,
                last_seek: None,
                volume: None,
                speed: None,
                opened: None,
                audio: None,
                subtitle: None,
            }
        }
    }

    struct Fake {
        surface: rune::NativeSurface,
        log: Arc<Mutex<FakeLog>>,
    }

    impl PlayerEngine for Fake {
        fn open(&mut self, path: &std::path::Path) -> Result<(), String> {
            self.log.lock().unwrap().opened = Some(path.to_path_buf());
            Ok(())
        }
        fn play(&mut self) {
            self.log.lock().unwrap().state = PlaybackState::Playing;
        }
        fn pause(&mut self) {
            self.log.lock().unwrap().state = PlaybackState::Paused;
        }
        fn toggle(&mut self) {
            let mut log = self.log.lock().unwrap();
            log.state = if matches!(log.state, PlaybackState::Playing) {
                PlaybackState::Paused
            } else {
                PlaybackState::Playing
            };
        }
        fn seek(&mut self, seconds: f64) {
            self.log.lock().unwrap().last_seek = Some(seconds);
        }
        fn set_volume(&mut self, volume: f32) {
            self.log.lock().unwrap().volume = Some(volume);
        }
        fn set_speed(&mut self, speed: f32) {
            self.log.lock().unwrap().speed = Some(speed);
        }
        fn position(&self) -> f64 {
            0.0
        }
        fn duration(&self) -> Option<f64> {
            Some(10.0)
        }
        fn state(&self) -> PlaybackState {
            self.log.lock().unwrap().state.clone()
        }
        fn surface(&self) -> rune::NativeSurface {
            self.surface.clone()
        }
        fn frames(&self) -> u64 {
            0
        }
        fn tracks(&self) -> Result<Vec<Track>, String> {
            let mut log = self.log.lock().unwrap();
            log.tracks_reads += 1;
            if let Some(result) = &log.tracks_result {
                return result.clone();
            }
            Ok(vec![
                Track {
                    id: 1,
                    kind: TrackKind::Audio,
                    title: Some("Main".into()),
                    language: Some("eng".into()),
                    selected: log.audio.unwrap_or(1) == 1,
                },
                Track {
                    id: 2,
                    kind: TrackKind::Audio,
                    title: Some("Commentary".into()),
                    language: Some("eng".into()),
                    selected: log.audio == Some(2),
                },
                Track {
                    id: 10,
                    kind: TrackKind::Subtitle,
                    title: Some("English".into()),
                    language: Some("eng".into()),
                    selected: log.subtitle == Some(10),
                },
            ])
        }
        fn select_audio(&mut self, id: i64) -> Result<(), String> {
            self.log.lock().unwrap().audio = Some(id);
            Ok(())
        }
        fn select_subtitle(&mut self, id: Option<i64>) -> Result<(), String> {
            self.log.lock().unwrap().subtitle = id;
            Ok(())
        }
        fn set_waker(&self, _: Waker) {}
    }

    fn app_with_fake() -> (App, Arc<Mutex<FakeLog>>) {
        let log = Arc::new(Mutex::new(FakeLog::default()));
        let mut app = App {
            engine: Some(Box::new(Fake {
                surface: rune::NativeSurface::new(),
                log: log.clone(),
            })),
            path: Some(PathBuf::from("test.mkv")),
            pending_path: None,
            error: None,
            volume: 1.0,
            speed: 1.0,
            last_dir: None,
            tracks: TracksState::Unloaded,
            timeline: Timeline::new(),
            export: None,
            export_mode: ExportMode::Precise,
            export_error: None,
        };
        app.refresh_tracks();
        (app, log)
    }

    fn window() -> (f32, f32) {
        (VIDEO_WIDTH as f32, VIDEO_HEIGHT as f32 + 192.0)
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kachito-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_play_button_toggles_playback() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        let (x, y) = runtime.find("play").expect("the play button");
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().state, PlaybackState::Playing);
    }

    #[test]
    fn dropping_a_file_opens_it() {
        let dir = std::env::temp_dir().join(format!("kachito-app-drop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dropped = dir.join("dropped.mkv");
        std::fs::write(&dropped, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        app.path = None;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        let at = runtime.find("video").unwrap_or((10.0, 10.0));
        runtime.drop_files(vec![dropped.clone()], Some(at));
        runtime.frame(1.0 / 60.0);
        let state = runtime.state.engine.as_ref().unwrap().state();
        runtime.state.sync_pending_path(&state);
        assert_eq!(runtime.state.path, Some(dropped.clone()));
        assert_eq!(log.lock().unwrap().opened, Some(dropped));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn volume_speed_and_seek_reach_the_engine() {
        let (mut app, log) = app_with_fake();
        app.volume_delta(-0.5);
        assert_eq!(log.lock().unwrap().volume, Some(0.5));
        app.volume_delta(-5.0);
        assert_eq!(app.volume, 0.0);
        app.cycle_speed();
        assert_eq!(log.lock().unwrap().speed, Some(1.25));
        app.seek_fraction(0.5);
        assert_eq!(log.lock().unwrap().last_seek, Some(5.0));
    }

    #[test]
    fn a_non_video_file_is_rejected() {
        let (mut app, _) = app_with_fake();
        app.open(PathBuf::from("notes.txt"));
        assert!(app.error.is_some());
        assert!(app.path.is_some(), "the previous file stays open");
    }

    #[test]
    fn a_remote_url_is_rejected() {
        let (mut app, log) = app_with_fake();
        app.open(PathBuf::from("https://example.test/movie.mp4"));
        assert!(app.error.is_some());
        assert_eq!(log.lock().unwrap().opened, None);
    }

    #[test]
    fn opening_a_missing_file_keeps_the_previous_session() {
        let (mut app, log) = app_with_fake();
        app.timeline.set_duration(12.0);
        app.timeline.set_in(3.0);
        app.timeline.set_out(8.0);
        let old_path = app.path.clone();
        let old_tracks = app.tracks.clone();
        let old_timeline = app.timeline.clone();

        app.open(PathBuf::from("missing.mkv"));
        assert_eq!(app.path, old_path);
        assert_eq!(app.tracks, old_tracks);
        assert_eq!(app.timeline, old_timeline);
        assert!(app.pending_path.is_none());
        assert!(app.error_text().is_some());
        assert_eq!(log.lock().unwrap().opened, None);
    }

    #[test]
    fn a_failed_load_keeps_the_previous_session() {
        let dir = temp_dir("corrupt");
        let corrupt = dir.join("corrupt.mkv");
        std::fs::write(&corrupt, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        app.timeline.set_duration(12.0);
        app.timeline.set_in(3.0);
        app.timeline.set_out(8.0);
        let old_path = app.path.clone();
        let old_tracks = app.tracks.clone();
        let old_timeline = app.timeline.clone();
        log.lock().unwrap().state = PlaybackState::Loading;

        app.open(corrupt);
        assert_eq!(app.path, old_path);
        assert_eq!(app.tracks, old_tracks);
        assert_eq!(app.timeline, old_timeline);
        assert!(app.pending_path.is_some());

        log.lock().unwrap().state = PlaybackState::Failed("corrupt".into());
        let state = app.engine.as_ref().unwrap().state();
        assert!(app.sync_pending_path(&state));
        assert_eq!(app.path, old_path);
        assert_eq!(app.tracks, old_tracks);
        assert_eq!(app.timeline, old_timeline);
        assert!(app.pending_path.is_none());
        assert_eq!(app.error_text().as_deref(), Some("corrupt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_valid_open_commits_only_when_the_engine_is_ready() {
        let dir = temp_dir("next");
        let next = dir.join("next.mkv");
        std::fs::write(&next, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        app.timeline.set_duration(12.0);
        app.timeline.set_in(3.0);
        app.timeline.set_out(8.0);
        let old_path = app.path.clone();
        log.lock().unwrap().state = PlaybackState::Loading;

        app.open(next.clone());
        assert_eq!(app.path, old_path);
        assert!(app.pending_path.is_some());

        let state = app.engine.as_ref().unwrap().state();
        assert!(!app.sync_pending_path(&state));
        assert_eq!(app.path, old_path);

        log.lock().unwrap().state = PlaybackState::Paused;
        let state = app.engine.as_ref().unwrap().state();
        assert!(app.sync_pending_path(&state));
        assert_eq!(app.path, Some(next));
        assert_eq!(app.tracks, TracksState::Unloaded);
        assert_eq!(app.timeline, Timeline::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_trackless_file_reads_the_track_list_once() {
        let (mut app, log) = app_with_fake();
        app.tracks = TracksState::Unloaded;
        {
            let mut log = log.lock().unwrap();
            log.tracks_result = Some(Ok(Vec::new()));
            log.tracks_reads = 0;
        }
        let state = app.engine.as_ref().unwrap().state();
        assert!(app.load_tracks(&state));
        assert_eq!(app.tracks, TracksState::Loaded(Vec::new()));
        assert_eq!(log.lock().unwrap().tracks_reads, 1);
        assert!(!app.load_tracks(&state));
        assert_eq!(log.lock().unwrap().tracks_reads, 1);
    }

    #[test]
    fn a_track_list_failure_is_visible_and_distinct() {
        let (mut app, log) = app_with_fake();
        app.tracks = TracksState::Unloaded;
        {
            let mut log = log.lock().unwrap();
            log.tracks_result = Some(Err("track list unavailable".into()));
            log.tracks_reads = 0;
        }
        let state = app.engine.as_ref().unwrap().state();
        assert!(app.load_tracks(&state));
        assert_eq!(
            app.tracks,
            TracksState::Failed("track list unavailable".into())
        );
        assert_eq!(log.lock().unwrap().tracks_reads, 1);
        assert!(!app.load_tracks(&state));
        assert_eq!(log.lock().unwrap().tracks_reads, 1);
    }

    #[test]
    fn the_track_bar_distinguishes_loading_empty_and_failure() {
        for (tracks, key) in [
            (TracksState::Unloaded, "track-loading"),
            (TracksState::Loaded(Vec::new()), "no-audio"),
            (
                TracksState::Failed("track list unavailable".into()),
                "track-error",
            ),
        ] {
            let (mut app, _) = app_with_fake();
            app.tracks = tracks;
            let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
            runtime.frame(0.0);
            assert!(runtime.find(key).is_some(), "{key}");
        }
    }

    #[test]
    fn the_play_button_label_follows_the_playback_state() {
        assert_eq!(play_label(&PlaybackState::Playing), "Pause");
        assert_eq!(play_label(&PlaybackState::Loading), "Loading");
        assert_eq!(
            play_label(&PlaybackState::Failed("failed".into())),
            "Failed"
        );
        for state in [
            PlaybackState::Empty,
            PlaybackState::Paused,
            PlaybackState::Ended,
        ] {
            assert_eq!(play_label(&state), "Play");
        }
    }

    #[test]
    fn audio_and_subtitle_buttons_pick_tracks() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);

        let (x, y) = runtime.find("audio-2").expect("the commentary track");
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().audio, Some(2));

        let (x, y) = runtime.find("sub-10").expect("the subtitle track");
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(2.0 / 60.0);
        assert_eq!(log.lock().unwrap().subtitle, Some(10));

        let (x, y) = runtime.find("subs-off").expect("the off button");
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(3.0 / 60.0);
        assert_eq!(log.lock().unwrap().subtitle, None);
    }

    #[test]
    fn the_export_bar_renders_each_export_state() {
        let states = [
            ExportState::Running { fraction: 0.5 },
            ExportState::Finished,
            ExportState::Cancelled,
            ExportState::Failed("export failed".into()),
        ];
        for state in states {
            let (mut app, _) = app_with_fake();
            app.export = Some(ExportJob::with_state(state.clone()));
            let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
            runtime.frame(0.0);
            assert!(runtime.find("export-status").is_some(), "{state:?}");
            if matches!(state, ExportState::Running { .. }) {
                assert!(runtime.find("cancel-export").is_some(), "{state:?}");
                assert!(runtime.find("export").is_none(), "{state:?}");
            } else {
                assert!(runtime.find("cancel-export").is_none(), "{state:?}");
                assert!(runtime.find("export").is_some(), "{state:?}");
            }
        }
    }
}
