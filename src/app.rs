//! The Kachito screen: one file, one fitted stage, one floating transport.

use crate::engine::{MpvEngine, PlaybackState, Track, TrackKind, VIDEO_HEIGHT, VIDEO_WIDTH};
use crate::export::{ExportMode, ExportState};
use crate::log::{RunLog, StderrSink};
use crate::session::{PlaybackSession, SUPPORTED, TracksState, tracks_of};
use crate::ui::{self, Palette, icons};
use rune::{
    AppSpec, Color, Cx, Drag, El, FileDialogOptions, Key, KeyEvent, LiveRegion, Phase, ScrollSpec,
    SurfaceFit, Transition, Tx, Waker, col, el, icon, row, text,
};
use std::path::PathBuf;
use std::sync::Arc;

const SPEEDS: &[f32] = &[0.5, 1.0, 1.25, 1.5, 2.0];
const HIDE_AFTER: f64 = 2.5;
const SKIP: f64 = 10.0;
const SCRUB_THRESHOLD: f32 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Audio,
    Subtitles,
    More,
}

struct App {
    session: Option<PlaybackSession>,
    error: Option<String>,
    volume: f32,
    last_volume: f32,
    speed: f32,
    last_dir: Option<PathBuf>,
    export_mode: ExportMode,
    menu: Option<Menu>,
    export_open: bool,
    transport_visible: bool,
    transport_hovered: bool,
    activity: bool,
    last_activity: f64,
    drop_hover: bool,
    title_source: Option<PathBuf>,
}

impl App {
    fn new() -> Self {
        let log = Arc::new(RunLog::from_env(Arc::new(StderrSink)));
        let mut app = match MpvEngine::new(rune::is_headless(), log.clone()) {
            Ok(engine) => Self::with_session(Some(PlaybackSession::new(Box::new(engine), log))),
            Err(error) => {
                let mut app = Self::with_session(None);
                app.error = Some(error);
                app
            }
        };
        if let Some(path) = rune::extra_args().first() {
            app.open(PathBuf::from(path));
        }
        app
    }

    fn with_session(session: Option<PlaybackSession>) -> Self {
        Self {
            session,
            error: None,
            volume: 1.0,
            last_volume: 1.0,
            speed: 1.0,
            last_dir: None,
            export_mode: ExportMode::Precise,
            menu: None,
            export_open: false,
            transport_visible: true,
            transport_hovered: false,
            activity: false,
            last_activity: 0.0,
            drop_hover: false,
            title_source: None,
        }
    }

    fn open(&mut self, path: PathBuf) {
        let Some(session) = &mut self.session else {
            self.error = Some("The playback engine is not available.".into());
            return;
        };
        let parent = path.parent().map(|parent| parent.to_path_buf());
        match session.open(path) {
            Ok(()) => {
                self.last_dir = parent;
                self.error = None;
                self.menu = None;
                self.touch();
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn open_first(&mut self, files: &[PathBuf]) {
        if let Some(path) = files.first() {
            self.open(path.clone());
        }
    }

    fn touch(&mut self) {
        self.activity = true;
        self.transport_visible = true;
    }

    fn toggle(&mut self) {
        if let Some(session) = &mut self.session {
            session.toggle();
        }
        self.touch();
    }

    fn seek_fraction(&mut self, fraction: f32) {
        let Some(session) = &mut self.session else {
            return;
        };
        let Some(duration) = session.duration() else {
            return;
        };
        let time = f64::from(fraction.clamp(0.0, 1.0)) * duration;
        session.seek(time);
        session.timeline_mut().set_position(time);
        self.touch();
    }

    fn preview_fraction(&mut self, fraction: f32) {
        let Some(duration) = self.session.as_ref().and_then(|session| session.duration()) else {
            return;
        };
        self.preview_to(f64::from(fraction.clamp(0.0, 1.0)) * duration);
    }

    fn preview_to(&mut self, time: f64) {
        if let Some(session) = &mut self.session {
            session.timeline_mut().set_position(time);
            session.seek_preview(time);
        }
        self.touch();
    }

    fn begin_scrub(&mut self) {
        if let Some(session) = &mut self.session {
            session.set_scrubbing(true);
        }
        self.touch();
    }

    fn seek_to(&mut self, time: f64) {
        if let Some(session) = &mut self.session {
            session.seek(time);
            session.timeline_mut().set_position(time);
        }
        self.touch();
    }

    fn skip(&mut self, delta: f64) {
        let Some(session) = &mut self.session else {
            return;
        };
        let Some(duration) = session.duration() else {
            return;
        };
        let time = (session.timeline().position() + delta).clamp(0.0, duration);
        session.seek(time);
        session.timeline_mut().set_position(time);
        self.touch();
    }

    fn volume_delta(&mut self, delta: f32) {
        self.set_volume(self.volume + delta);
    }

    fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if self.volume > 0.0 {
            self.last_volume = self.volume;
        }
        if let Some(session) = &mut self.session {
            session.set_volume(self.volume);
        }
        self.touch();
    }

    fn toggle_mute(&mut self) {
        if self.volume > 0.0 {
            self.last_volume = self.volume;
            self.set_volume(0.0);
        } else {
            let restore = if self.last_volume > 0.0 {
                self.last_volume
            } else {
                0.5
            };
            self.set_volume(restore);
        }
    }

    fn set_speed(&mut self, speed: f32) {
        self.speed = speed;
        if let Some(session) = &mut self.session {
            session.set_speed(self.speed);
        }
    }

    fn select_audio(&mut self, id: i64) {
        if let Some(session) = &mut self.session
            && let Err(error) = session.select_audio(id)
        {
            self.error = Some(error);
        }
        self.menu = None;
    }

    fn select_subtitle(&mut self, id: Option<i64>) {
        if let Some(session) = &mut self.session
            && let Err(error) = session.select_subtitle(id)
        {
            self.error = Some(error);
        }
        self.menu = None;
    }

    fn toggle_menu(&mut self, menu: Menu) {
        self.menu = (self.menu != Some(menu)).then_some(menu);
        self.touch();
    }

    fn export_ready(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.source().is_some() && session.duration().is_some())
    }

    fn toggle_export(&mut self) {
        if self.export_open {
            self.export_open = false;
            self.touch();
            return;
        }
        if !self.export_ready() {
            return;
        }
        self.export_open = true;
        self.menu = None;
        self.touch();
    }

    fn set_in(&mut self, time: f64) {
        if let Some(session) = &mut self.session {
            session.timeline_mut().set_in(time);
        }
    }

    fn set_out(&mut self, time: f64) {
        if let Some(session) = &mut self.session {
            session.timeline_mut().set_out(time);
        }
    }

    fn zoom_in(&mut self) {
        if let Some(session) = &mut self.session {
            session.timeline_mut().zoom_in();
        }
    }

    fn zoom_out(&mut self) {
        if let Some(session) = &mut self.session {
            session.timeline_mut().zoom_out();
        }
    }

    fn start_export(&mut self, output: PathBuf) {
        if let Some(session) = &mut self.session {
            session.start_export(output, self.export_mode);
        }
    }

    fn cancel_export(&self) {
        if let Some(session) = &self.session {
            session.cancel_export();
        }
    }

    fn set_export_error(&mut self, error: String) {
        if let Some(session) = &mut self.session {
            session.set_export_error(error);
        }
    }

    fn error_text(&self) -> Option<String> {
        self.error
            .clone()
            .or_else(|| self.session.as_ref().and_then(|session| session.error()))
    }

    fn export_state(&self) -> Option<ExportState> {
        self.session
            .as_ref()
            .and_then(|session| session.export_state())
    }

    fn has_source(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.source().is_some())
    }

    fn has_audio(&self) -> bool {
        matches!(
            self.session.as_ref().map(|session| session.tracks()),
            Some(TracksState::Loaded(tracks)) if tracks_of(tracks, TrackKind::Audio).next().is_some()
        )
    }

    fn has_subtitles(&self) -> bool {
        matches!(
            self.session.as_ref().map(|session| session.tracks()),
            Some(TracksState::Loaded(tracks)) if tracks_of(tracks, TrackKind::Subtitle).next().is_some()
        )
    }

    fn subtitles_on(&self) -> bool {
        matches!(
            self.session.as_ref().map(|session| session.tracks()),
            Some(TracksState::Loaded(tracks))
                if tracks_of(tracks, TrackKind::Subtitle).any(|track| track.selected)
        )
    }

    fn playing(&self) -> bool {
        matches!(
            self.session.as_ref().map(|session| session.state()),
            Some(PlaybackState::Playing)
        )
    }
}

fn state() -> App {
    App::new()
}

fn setup(app: &mut App, waker: Waker) {
    if let Some(session) = &app.session {
        session.set_waker(waker);
    }
}

fn poll(app: &mut App, t: f64, tx: &mut Tx) -> bool {
    let mut changed = false;
    if let Some(session) = &mut app.session {
        changed |= session.poll(tx);
        let source = session.source().map(|path| path.to_path_buf());
        if app.title_source != source {
            tx.set_title(format!("{} — Kachito", file_title(source.as_deref())));
            app.title_source = source;
            app.export_open = false;
            changed = true;
        }
    }
    if app.activity {
        app.activity = false;
        app.last_activity = t;
        changed = true;
    }
    if app.playing() && app.menu.is_none() && !app.export_open && !app.transport_hovered {
        if app.transport_visible && t - app.last_activity > HIDE_AFTER {
            app.transport_visible = false;
            changed = true;
        }
    } else {
        app.last_activity = t;
        if !app.transport_visible {
            app.transport_visible = true;
            changed = true;
        }
    }
    changed
}

fn on_key(app: &mut App, key: &KeyEvent, tx: &mut Tx) -> bool {
    if app.menu.is_some() {
        return false;
    }
    let handled = if key.is_command('o') {
        open_dialog(app, tx);
        true
    } else {
        match &key.key {
            Key::Text(value) if value == " " && !key.mods.cmd && !key.mods.ctrl => {
                app.toggle();
                true
            }
            Key::Text(value)
                if value.eq_ignore_ascii_case("m") && !key.mods.cmd && !key.mods.ctrl =>
            {
                app.toggle_mute();
                true
            }
            Key::Text(value)
                if value.eq_ignore_ascii_case("f") && !key.mods.cmd && !key.mods.ctrl =>
            {
                tx.toggle_full_screen();
                true
            }
            Key::Text(value)
                if value.eq_ignore_ascii_case("e") && !key.mods.cmd && !key.mods.ctrl =>
            {
                app.toggle_export();
                true
            }
            Key::Left => {
                app.skip(-SKIP);
                true
            }
            Key::Right => {
                app.skip(SKIP);
                true
            }
            Key::Up => {
                app.volume_delta(0.05);
                true
            }
            Key::Down => {
                app.volume_delta(-0.05);
                true
            }
            Key::Escape if app.export_open => {
                app.toggle_export();
                true
            }
            _ => false,
        }
    };
    app.touch();
    handled
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
    let Some(session) = &app.session else {
        return;
    };
    let Some(source) = session.source() else {
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
    tx.save_file(options, move |app: &mut App, result, _| match result {
        Ok(Some(path)) => app.start_export(path),
        Ok(None) => {}
        Err(error) => app.set_export_error(error.to_string()),
    });
}

fn view(app: &App, _: &mut Cx<App>) -> El {
    let theme = ui::palette();
    let error = app.error_text();
    let mut root = col()
        .key("root")
        .w_full()
        .h_full()
        .fill(theme.bg)
        .on_pointer_move(|app: &mut App, _| app.touch())
        .on_drop_files(|app: &mut App, files, _| app.open_first(files))
        .accepts_extensions(SUPPORTED)
        .on_drop_hover(|app: &mut App, over, _| {
            app.drop_hover = over;
            app.touch();
        });
    if let Some(session) = &app.session
        && session.source().is_some()
    {
        let mut stage = video_stage(session, &theme);
        if app.export_open {
            stage = stage.child(export_tray(app, session, &theme));
        } else if app.transport_visible {
            stage = stage.child(transport(app, session, &theme));
        }
        if let Some(error) = &error {
            stage = stage.child(error_banner(error, &theme));
        }
        root = root.child(top_bar(app, &theme)).child(stage);
    } else {
        root = root
            .child(top_bar(app, &theme))
            .child(empty_screen(app, &theme, error.as_deref()));
    }
    if let Some(menu) = menu_overlay(app, &theme) {
        root = root.child(menu);
    }
    root
}

fn file_title(source: Option<&std::path::Path>) -> String {
    source
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Kachito".into())
}

fn top_bar(app: &App, theme: &Palette) -> El {
    let title = file_title(app.session.as_ref().and_then(|session| session.source()));
    let mut bar = row()
        .key("top-bar")
        .w_full()
        .h(ui::TOP_BAR_H)
        .px(12.0)
        .gap(8.0)
        .center_y()
        .fill(theme.surface)
        .border_sides([0.0, 0.0, 1.0, 0.0], theme.border)
        .child(el().size(6.0, 6.0).radius(3.0).fill(theme.accent))
        .child(
            text(title)
                .font(ui::BODY_STRONG)
                .color(theme.text)
                .truncate()
                .grow(1.0),
        );
    bar = bar
        .child(export_status(app, theme))
        .child(open_button(theme))
        .child(audio_button(app, theme))
        .child(subtitle_button(app, theme))
        .child(export_button(app, theme));
    bar
}

fn open_button(theme: &Palette) -> El {
    ui::label_button(Some(&icons::OPEN), "Open", false, theme)
        .key("open")
        .on_press_tx(|app: &mut App, tx: &mut Tx| open_dialog(app, tx))
}

fn audio_button(app: &App, theme: &Palette) -> El {
    if !app.has_source() {
        return ui::label_button(Some(&icons::AUDIO), "Audio", false, theme)
            .key("audio-menu")
            .disabled(true)
            .dim(0.35);
    }
    let button = ui::label_button(Some(&icons::AUDIO), "Audio", false, theme)
        .key("audio-menu")
        .expanded(app.menu == Some(Menu::Audio))
        .on_press(|app: &mut App| app.toggle_menu(Menu::Audio));
    if app.has_audio() {
        button
    } else {
        button.dim(0.55)
    }
}

fn subtitle_button(app: &App, theme: &Palette) -> El {
    if !app.has_source() {
        return ui::label_button(Some(&icons::SUBTITLES), "Subtitles", false, theme)
            .key("subs-menu")
            .disabled(true)
            .dim(0.35);
    }
    let mut button = ui::label_button(Some(&icons::SUBTITLES), "Subtitles", false, theme)
        .key("subs-menu")
        .expanded(app.menu == Some(Menu::Subtitles))
        .on_press(|app: &mut App| app.toggle_menu(Menu::Subtitles));
    if app.subtitles_on() {
        button = button.child(el().size(5.0, 5.0).radius(3.0).fill(theme.accent));
    }
    if app.has_subtitles() {
        button
    } else {
        button.dim(0.55)
    }
}

fn export_button(app: &App, theme: &Palette) -> El {
    if !app.export_ready() {
        return ui::label_button(Some(&icons::EXPORT), "Export", false, theme)
            .key("export-toggle")
            .disabled(true)
            .dim(0.35);
    }
    ui::label_button(Some(&icons::EXPORT), "Export", app.export_open, theme)
        .key("export-toggle")
        .expanded(app.export_open)
        .checked(app.export_open)
        .on_press(|app: &mut App| app.toggle_export())
}

fn export_status(app: &App, theme: &Palette) -> El {
    let mut slot = row().w(64.0).justify_end();
    if let Some(state) = app.export_state() {
        let (label, color) = match state {
            ExportState::Running { fraction } => {
                (format!("{}%", (fraction * 100.0).round()), theme.text_muted)
            }
            ExportState::Finished => ("Exported".into(), theme.text_muted),
            ExportState::Cancelled => ("Cancelled".into(), theme.text_muted),
            ExportState::Failed(_) => ("Failed".into(), theme.danger),
        };
        slot = slot.child(
            text(label)
                .key("export-status")
                .font(ui::TIME)
                .color(color)
                .truncate(),
        );
    }
    slot
}

fn video_stage(session: &PlaybackSession, theme: &Palette) -> El {
    let surface = session.surface();
    el().key("stage")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .fill(theme.stage)
        .center()
        .child(
            el().key("video")
                .aspect_fit(VIDEO_WIDTH as f32 / VIDEO_HEIGHT as f32)
                .native_surface(&surface, SurfaceFit::Fit)
                .on_double_press_tx(|app: &mut App, tx: &mut Tx| {
                    app.touch();
                    tx.toggle_full_screen();
                }),
        )
}

fn empty_screen(app: &App, theme: &Palette, error: Option<&str>) -> El {
    let border = if app.drop_hover {
        theme.accent
    } else {
        theme.border_strong
    };
    let fill = if app.drop_hover {
        theme.accent_soft
    } else {
        Color::TRANSPARENT
    };
    let zone = col()
        .key("drop-zone")
        .w(440.0)
        .padding(28.0)
        .gap(12.0)
        .radius(ui::RADIUS_LG)
        .center()
        .border(1.0, border)
        .border_dash(5.0, 5.0)
        .fill(fill)
        .child(icon(&icons::FILM, 30.0).color(theme.accent).a11y_hidden())
        .child(text("Open a video").font(ui::TITLE).color(theme.text))
        .child(
            text("MKV, MP4, M4V, MOV or WebM")
                .font(ui::LABEL)
                .color(theme.text_muted),
        )
        .child(
            ui::primary_button("Open video", theme)
                .key("open-empty")
                .on_press_tx(|app: &mut App, tx: &mut Tx| open_dialog(app, tx)),
        )
        .child(
            text("or drop a file anywhere")
                .font(ui::TINY)
                .color(theme.text_faint),
        );
    let mut stage = el()
        .key("empty")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .fill(theme.stage)
        .center()
        .child(zone);
    if let Some(error) = error {
        stage = stage.child(error_banner(error, theme));
    }
    stage
}

fn error_banner(error: &str, theme: &Palette) -> El {
    row()
        .key("error-banner")
        .pin(Some(0.0), Some(12.0), Some(0.0), None)
        .w_full()
        .justify_center()
        .child(
            row()
                .key("error-banner-card")
                .w(640.0)
                .min_h(38.0)
                .padding(10.0)
                .gap(8.0)
                .center_y()
                .radius(9.0)
                .fill(theme.danger_surface)
                .border(1.0, theme.danger.alpha(0.4))
                .child(icon(&icons::ALERT, 18.0).color(theme.danger).a11y_hidden())
                .child(
                    text(error.to_string())
                        .font(ui::LABEL)
                        .color(theme.danger)
                        .live_region(LiveRegion::Assertive)
                        .wrap()
                        .grow(1.0),
                ),
        )
}

fn transport(app: &App, session: &PlaybackSession, theme: &Palette) -> El {
    let duration = session.duration().unwrap_or(0.0);
    let position = session.timeline().position();
    let fraction = if duration > 0.0 {
        (position / duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let card = col()
        .key("transport")
        .w_full()
        .max_w(ui::TRANSPORT_MAX_W)
        .padding(12.0)
        .gap(8.0)
        .radius(ui::RADIUS_LG)
        .fill(theme.surface)
        .border(1.0, theme.border_strong)
        .shadow(14.0, 30.0, theme.shadow)
        .transition(
            Transition::offset(0.0, 8.0)
                .combined(Transition::opacity())
                .springs(ui::TRAY_MOTION),
        )
        .on_hover_change(|app: &mut App, inside: bool, _: &mut Tx| {
            app.transport_hovered = inside;
            if inside {
                app.touch();
            }
        })
        .child(seek_bar(app, fraction, duration, theme))
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(play_button(session, theme))
                .child(
                    ui::icon_button(&icons::BACK_10, "Back 10 seconds", theme)
                        .key("back-10")
                        .on_press(|app: &mut App| app.skip(-SKIP)),
                )
                .child(
                    ui::icon_button(&icons::FORWARD_10, "Forward 10 seconds", theme)
                        .key("forward-10")
                        .on_press(|app: &mut App| app.skip(SKIP)),
                )
                .child(
                    text(format_time(position, duration))
                        .font(ui::TIME)
                        .color(theme.text_muted),
                )
                .child(el().grow(1.0))
                .child(volume_control(app, theme))
                .child(
                    ui::icon_button(&icons::MORE, "More playback controls", theme)
                        .key("more")
                        .expanded(app.menu == Some(Menu::More))
                        .on_press(|app: &mut App| app.toggle_menu(Menu::More)),
                ),
        );
    row()
        .key("transport-wrap")
        .pin(Some(0.0), None, Some(0.0), Some(16.0))
        .w_full()
        .justify_center()
        .child(card)
}

fn seek_bar(app: &App, fraction: f32, duration: f64, theme: &Palette) -> El {
    let seeking = app
        .session
        .as_ref()
        .is_some_and(|session| session.is_seeking());
    el().key("seek")
        .w_full()
        .h(20.0)
        .on_drag(|app: &mut App, drag: &mut Drag, _: &mut Tx| {
            if drag.w <= 0.0 {
                return;
            }
            match drag.phase {
                Phase::Down => app.begin_scrub(),
                Phase::Move if drag.dx.abs() > SCRUB_THRESHOLD => {
                    app.preview_fraction(drag.x / drag.w);
                }
                Phase::Move => {}
                Phase::Up => app.seek_fraction(drag.x / drag.w),
            }
        })
        .role("slider")
        .label("Seek")
        .range_value(app_position(app), 0.0, duration.max(0.001))
        .child(
            row()
                .w_full()
                .h(10.0)
                .center_y()
                .child(
                    el().grow(fraction.max(0.001))
                        .h(4.0)
                        .radius(2.0)
                        .fill(theme.accent),
                )
                .child(
                    el().key("seek-handle")
                        .w(10.0)
                        .h(10.0)
                        .radius(5.0)
                        .fill(if seeking {
                            theme.accent.alpha(0.55)
                        } else {
                            theme.accent
                        }),
                )
                .child(
                    el().grow((1.0 - fraction).max(0.001))
                        .h(4.0)
                        .radius(2.0)
                        .fill(theme.track),
                ),
        )
}

fn app_position(app: &App) -> f64 {
    app.session
        .as_ref()
        .map(|session| session.timeline().position())
        .unwrap_or(0.0)
}

fn play_button(session: &PlaybackSession, theme: &Palette) -> El {
    let state = session.state();
    let loaded = state.is_loaded();
    let playing = matches!(state, PlaybackState::Playing);
    let glyph = if playing { &icons::PAUSE } else { &icons::PLAY };
    let mut button = el()
        .key("play")
        .size(44.0, 44.0)
        .radius(22.0)
        .center()
        .press_scale(0.96)
        .focusable(loaded)
        .focus_ring(2.0, 2.0, theme.focus)
        .role("button")
        .label(play_label(&state))
        .child(icon(glyph, 20.0).color(if loaded {
            theme.accent_ink
        } else {
            theme.text_faint
        }));
    if loaded {
        button = button
            .fill(theme.accent)
            .hover(theme.accent)
            .press(theme.accent)
            .on_press(|app: &mut App| app.toggle());
    } else {
        button = button.fill(theme.surface_raised).dim(0.7);
    }
    button
}

fn volume_control(app: &App, theme: &Palette) -> El {
    let muted = app.volume <= 0.001;
    row()
        .gap(4.0)
        .center_y()
        .child(
            ui::icon_button(
                if muted { &icons::MUTE } else { &icons::VOLUME },
                "Mute",
                theme,
            )
            .key("mute")
            .on_press(|app: &mut App| app.toggle_mute()),
        )
        .child(
            el().key("volume")
                .w(80.0)
                .h(18.0)
                .on_drag(|app: &mut App, drag: &mut Drag, _: &mut Tx| {
                    if drag.w > 0.0 {
                        app.set_volume(drag.x / drag.w);
                    }
                })
                .role("slider")
                .label("Volume")
                .range_value(app.volume as f64, 0.0, 1.0)
                .child(
                    row()
                        .w_full()
                        .h(8.0)
                        .center_y()
                        .child(
                            el().grow(app.volume.max(0.001))
                                .h(3.0)
                                .radius(2.0)
                                .fill(theme.accent),
                        )
                        .child(el().w(8.0).h(8.0).radius(4.0).fill(theme.accent))
                        .child(
                            el().grow((1.0 - app.volume).max(0.001))
                                .h(3.0)
                                .radius(2.0)
                                .fill(theme.track),
                        ),
                ),
        )
}

fn menu_overlay(app: &App, theme: &Palette) -> Option<El> {
    let menu = app.menu?;
    let panel = match menu {
        Menu::Audio => menu_panel(
            "audio-menu-panel",
            "Audio",
            "audio-menu",
            audio_rows(app, theme),
            280.0,
            theme,
        ),
        Menu::Subtitles => menu_panel(
            "subs-menu-panel",
            "Subtitles",
            "subs-menu",
            subtitle_rows(app, theme),
            280.0,
            theme,
        ),
        Menu::More => menu_panel(
            "more-menu-panel",
            "Player",
            "more",
            more_rows(app, theme),
            300.0,
            theme,
        ),
    };
    Some(if menu == Menu::More {
        col()
            .key("menu-scrim")
            .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .w_full()
            .h_full()
            .justify_end()
            .center_y()
            .pb(110.0)
            .fill(theme.scrim)
            .child(panel)
    } else {
        el().key("menu-scrim")
            .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .fill(theme.scrim)
            .child(panel.pin(None, Some(ui::TOP_BAR_H + 8.0), Some(16.0), None))
    })
}

fn menu_panel(key: &str, title: &str, trigger: &str, body: El, width: f32, theme: &Palette) -> El {
    col()
        .key(key)
        .w(width)
        .padding(6.0)
        .gap(2.0)
        .radius(ui::RADIUS_LG)
        .fill(theme.surface_raised)
        .border(1.0, theme.border_strong)
        .shadow(16.0, 40.0, theme.shadow)
        .modal(true, |app: &mut App| app.menu = None)
        .focus_return(trigger)
        .role("dialog")
        .label(title)
        .child(
            row()
                .w_full()
                .h(40.0)
                .px(8.0)
                .gap(8.0)
                .center_y()
                .child(
                    text(title)
                        .font(ui::BODY_STRONG)
                        .color(theme.text)
                        .grow(1.0),
                )
                .child(
                    ui::small_icon_button(&icons::CLOSE, "Close menu", theme)
                        .key("menu-close")
                        .on_press(|app: &mut App| app.menu = None),
                ),
        )
        .child(body)
}

fn audio_rows(app: &App, theme: &Palette) -> El {
    let Some(session) = &app.session else {
        return col()
            .w_full()
            .child(ui::menu_empty("No audio tracks", theme).key("no-audio"));
    };
    match session.tracks() {
        TracksState::Unloaded => col()
            .w_full()
            .child(ui::menu_empty("Loading tracks…", theme).key("track-loading")),
        TracksState::Failed(error) => col()
            .w_full()
            .child(ui::menu_empty(error.clone(), theme).key("track-error")),
        TracksState::Loaded(tracks) => {
            let audio: Vec<&Track> = tracks_of(tracks, TrackKind::Audio).collect();
            if audio.is_empty() {
                return col()
                    .w_full()
                    .child(ui::menu_empty("No audio tracks", theme).key("no-audio"));
            }
            let any_selected = audio.iter().any(|track| track.selected);
            let mut list = col().w_full().gap(1.0).role("menu").focus_group();
            for (index, track) in audio.into_iter().enumerate() {
                let id = track.id;
                let label = track.label();
                let selected = track.selected;
                let mut row = ui::menu_row(label, selected, theme)
                    .key(format!("audio-{id}"))
                    .on_press(move |app: &mut App| app.select_audio(id));
                if selected || (index == 0 && !any_selected) {
                    row = row.autofocus();
                }
                list = list.child(row);
            }
            list
        }
    }
}

fn subtitle_rows(app: &App, theme: &Palette) -> El {
    let tracks = app.session.as_ref().map(|session| session.tracks());
    let any_selected = matches!(
        tracks,
        Some(TracksState::Loaded(tracks))
            if tracks_of(tracks, TrackKind::Subtitle).any(|track| track.selected)
    );
    let mut off = ui::menu_row("Off", !any_selected, theme)
        .key("subs-off")
        .on_press(|app: &mut App| app.select_subtitle(None));
    if !any_selected {
        off = off.autofocus();
    }
    let mut list = col()
        .w_full()
        .gap(1.0)
        .role("menu")
        .focus_group()
        .child(off);
    match tracks {
        Some(TracksState::Unloaded) | None => {
            list = list.child(ui::menu_empty("Loading tracks…", theme).key("track-loading"));
        }
        Some(TracksState::Failed(error)) => {
            list = list.child(ui::menu_empty(error.clone(), theme).key("track-error"));
        }
        Some(TracksState::Loaded(tracks)) => {
            let subtitles: Vec<&Track> = tracks_of(tracks, TrackKind::Subtitle).collect();
            if subtitles.is_empty() {
                list = list.child(ui::menu_empty("No subtitle tracks", theme).key("no-subtitles"));
            }
            for track in subtitles {
                let id = track.id;
                let label = track.label();
                let selected = track.selected;
                list = list.child(
                    ui::menu_row(label, selected, theme)
                        .key(format!("sub-{id}"))
                        .on_press(move |app: &mut App| app.select_subtitle(Some(id))),
                );
            }
        }
    }
    list
}

fn more_rows(app: &App, theme: &Palette) -> El {
    let speeds = row().w_full().gap(2.0).children(SPEEDS.iter().map(|speed| {
        let value = *speed;
        ui::segment(format_speed(value), (app.speed - value).abs() < 0.01, theme)
            .key(format!("speed-{value}"))
            .on_press(move |app: &mut App| app.set_speed(value))
    }));
    let content = col()
        .w_full()
        .role("menu")
        .focus_group()
        .gap(2.0)
        .child(ui::menu_heading("Speed", theme))
        .child(el().w_full().px(4.0).child(speeds))
        .child(ui::menu_separator(theme))
        .child(ui::menu_heading("Audio", theme))
        .child(audio_rows(app, theme))
        .child(ui::menu_separator(theme))
        .child(ui::menu_heading("Subtitles", theme))
        .child(subtitle_rows(app, theme))
        .child(ui::menu_separator(theme))
        .child(
            ui::menu_action("Fullscreen", theme)
                .key("fullscreen")
                .on_press_tx(|app: &mut App, tx: &mut Tx| {
                    app.menu = None;
                    tx.toggle_full_screen();
                }),
        );
    col()
        .w_full()
        .max_h(360.0)
        .min_h(0.0)
        .scroll(ScrollSpec {
            stick_bottom: false,
        })
        .child(content)
}

fn export_tray(app: &App, session: &PlaybackSession, theme: &Palette) -> El {
    let timeline = session.timeline();
    let state = session.export_state();
    let running = matches!(&state, Some(ExportState::Running { .. }));
    let mut precise = ui::segment(
        ExportMode::Precise.label(),
        app.export_mode == ExportMode::Precise,
        theme,
    )
    .key("mode-precise");
    let mut fast = ui::segment(
        ExportMode::Fast.label(),
        app.export_mode == ExportMode::Fast,
        theme,
    )
    .key("mode-fast");
    if running {
        precise = precise.disabled(true).dim(0.5);
        fast = fast.disabled(true).dim(0.5);
    } else {
        precise = precise.on_press(|app: &mut App| app.export_mode = ExportMode::Precise);
        fast = fast.on_press(|app: &mut App| app.export_mode = ExportMode::Fast);
    }
    let mut footer = row().w_full().gap(8.0).center_y().child(
        row()
            .gap(2.0)
            .padding(2.0)
            .radius(8.0)
            .fill(theme.stage)
            .child(precise)
            .child(fast),
    );
    footer = footer.child(el().grow(1.0));
    match &state {
        Some(ExportState::Running { fraction }) => {
            footer = footer
                .child(
                    el().w(160.0).h(4.0).radius(2.0).fill(theme.track).child(
                        el().w((160.0 * fraction.clamp(0.0, 1.0)).max(2.0))
                            .h(4.0)
                            .radius(2.0)
                            .fill(theme.accent),
                    ),
                )
                .child(
                    text(format!("{}%", (fraction * 100.0).round()))
                        .key("export-progress")
                        .font(ui::TIME)
                        .color(theme.text_muted),
                )
                .child(
                    ui::ghost_button("Cancel", theme)
                        .key("cancel-export")
                        .on_press(|app: &mut App| app.cancel_export()),
                );
        }
        Some(ExportState::Finished) => {
            footer = footer.child(
                text("Exported")
                    .key("export-tray-status")
                    .font(ui::LABEL)
                    .color(theme.text_muted),
            );
        }
        Some(ExportState::Cancelled) => {
            footer = footer.child(
                text("Cancelled")
                    .key("export-tray-status")
                    .font(ui::LABEL)
                    .color(theme.text_muted),
            );
        }
        Some(ExportState::Failed(error)) => {
            footer = footer.child(
                text(error.clone())
                    .key("export-tray-status")
                    .font(ui::LABEL)
                    .color(theme.danger)
                    .truncate(),
            );
        }
        None => {}
    }
    if !running {
        footer = footer.child(
            ui::primary_button("Export clip", theme)
                .key("export")
                .on_press_tx(|app: &mut App, tx: &mut Tx| export_dialog(app, tx)),
        );
    }
    let card = col()
        .key("export-tray")
        .w_full()
        .max_w(ui::EXPORT_MAX_W)
        .padding(14.0)
        .gap(10.0)
        .radius(ui::RADIUS_LG)
        .fill(theme.surface)
        .border(1.0, theme.border_strong)
        .shadow(16.0, 36.0, theme.shadow)
        .transition(
            Transition::offset(0.0, 8.0)
                .combined(Transition::opacity())
                .springs(ui::TRAY_MOTION),
        )
        .child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(
                    text("Export clip")
                        .font(ui::BODY_STRONG)
                        .color(theme.text)
                        .grow(1.0),
                )
                .child(
                    text(format!(
                        "{} · {}s",
                        format_clock(timeline.selection_duration()),
                        timeline.selection_duration().round()
                    ))
                    .font(ui::TIME)
                    .color(theme.text_muted),
                )
                .child(
                    ui::small_icon_button(&icons::CLOSE, "Close export", theme)
                        .key("export-close")
                        .on_press(|app: &mut App| app.toggle_export()),
                ),
        )
        .child(range_bar(&timeline, theme))
        .child(footer);
    row()
        .key("export-wrap")
        .pin(Some(0.0), None, Some(0.0), Some(16.0))
        .w_full()
        .justify_center()
        .child(card)
}

fn range_bar(timeline: &crate::timeline::Timeline, theme: &Palette) -> El {
    let start = timeline.fraction_of(timeline.in_point()) as f32;
    let end = timeline.fraction_of(timeline.out_point()) as f32;
    let position = timeline.fraction_of(timeline.position()) as f32;
    let selection = (end - start).max(0.0);
    let bar = el()
        .key("range")
        .w_full()
        .h(40.0)
        .on_drag(|app: &mut App, drag: &mut Drag, _: &mut Tx| {
            if drag.w <= 0.0 {
                return;
            }
            let Some(session) = &app.session else {
                return;
            };
            let time = session
                .timeline()
                .time_at_fraction(f64::from(drag.x / drag.w));
            if matches!(drag.phase, Phase::Down) {
                let in_x = session
                    .timeline()
                    .fraction_of(session.timeline().in_point()) as f32
                    * drag.w;
                let out_x = session
                    .timeline()
                    .fraction_of(session.timeline().out_point()) as f32
                    * drag.w;
                let in_distance = (drag.x - in_x).abs();
                let out_distance = (drag.x - out_x).abs();
                drag.data = if in_distance <= 12.0 && in_distance <= out_distance {
                    1.0
                } else if out_distance <= 12.0 {
                    2.0
                } else {
                    0.0
                };
            }
            match drag.data as i32 {
                1 => app.set_in(time),
                2 => app.set_out(time),
                _ if matches!(drag.phase, Phase::Up) => app.seek_to(time),
                _ if matches!(drag.phase, Phase::Down) => {}
                _ => app.preview_to(time),
            }
        })
        .role("slider")
        .label("Clip range")
        .range_value(timeline.position(), 0.0, timeline.duration().max(0.001))
        .child(
            col()
                .w_full()
                .gap(2.0)
                .child(
                    row()
                        .w_full()
                        .h(16.0)
                        .center_y()
                        .child(
                            el().grow(start.max(0.001))
                                .h(10.0)
                                .radius(5.0)
                                .fill(theme.track),
                        )
                        .child(el().w(4.0).h(18.0).radius(2.0).fill(theme.accent))
                        .child(
                            el().grow(selection.max(0.001))
                                .h(10.0)
                                .fill(theme.accent_soft),
                        )
                        .child(el().w(4.0).h(18.0).radius(2.0).fill(theme.accent))
                        .child(
                            el().grow((1.0 - end).max(0.001))
                                .h(10.0)
                                .radius(5.0)
                                .fill(theme.track),
                        ),
                )
                .child(
                    row()
                        .w_full()
                        .h(20.0)
                        .center_y()
                        .child(el().grow(position.max(0.001)))
                        .child(el().w(2.0).h(20.0).fill(theme.text))
                        .child(el().grow((1.0 - position).max(0.001))),
                ),
        );
    col().w_full().gap(2.0).child(bar).child(
        row()
            .w_full()
            .gap(8.0)
            .center_y()
            .child(
                text(format_clock(timeline.in_point()))
                    .font(ui::TIME)
                    .color(theme.text_muted),
            )
            .child(el().grow(1.0))
            .child(
                text(format!("{}s", timeline.selection_duration().round()))
                    .font(ui::TIME)
                    .color(theme.text),
            )
            .child(el().grow(1.0))
            .child(
                text(format_clock(timeline.out_point()))
                    .font(ui::TIME)
                    .color(theme.text_muted),
            )
            .child(
                ui::small_icon_button(&icons::MINUS, "Zoom out", theme)
                    .key("zoom-out")
                    .on_press(|app: &mut App| app.zoom_out()),
            )
            .child(
                ui::small_icon_button(&icons::PLUS, "Zoom in", theme)
                    .key("zoom-in")
                    .on_press(|app: &mut App| app.zoom_in()),
            ),
    )
}

fn format_speed(speed: f32) -> String {
    if (speed - speed.round()).abs() < 0.01 {
        format!("{}×", speed.round() as i32)
    } else {
        format!("{speed}×")
    }
}

fn play_label(state: &PlaybackState) -> &'static str {
    match state {
        PlaybackState::Playing => "Pause",
        PlaybackState::Loading => "Loading",
        PlaybackState::Failed(_) => "Failed",
        _ => "Play",
    }
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
        min_size: Some((960.0, 600.0)),
        families: (ui::sans_family(), "ui-monospace"),
        setup: Some(setup),
        poll: Some(poll),
        on_key: Some(on_key),
        ..AppSpec::new("Kachito", (1280.0, 800.0), state, view)
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::PlayerEngine;
    use crate::export::ExportJob;
    use crate::session::Clock;
    use crate::timeline::{DEFAULT_FRAME, Timeline};
    use rune::{Env, Mods, Runtime};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    struct FakeLog {
        state: PlaybackState,
        tracks_result: Option<Result<Vec<Track>, String>>,
        tracks_reads: usize,
        last_seek: Option<f64>,
        last_preview_seek: Option<f64>,
        preview_seeks: usize,
        seeking: bool,
        volume: Option<f32>,
        speed: Option<f32>,
        opened: Option<PathBuf>,
        audio: Option<i64>,
        subtitle: Option<i64>,
        frame_duration: f64,
        position: f64,
        duration: f64,
        command_failure: Option<String>,
        open_error: Option<String>,
    }

    impl Default for FakeLog {
        fn default() -> Self {
            Self {
                state: PlaybackState::Paused,
                tracks_result: None,
                tracks_reads: 0,
                last_seek: None,
                last_preview_seek: None,
                preview_seeks: 0,
                seeking: false,
                volume: None,
                speed: None,
                opened: None,
                audio: None,
                subtitle: None,
                frame_duration: 1.0 / 60.0,
                position: 0.0,
                duration: 10.0,
                command_failure: None,
                open_error: None,
            }
        }
    }

    struct Fake {
        surface: rune::NativeSurface,
        log: Arc<Mutex<FakeLog>>,
    }

    impl PlayerEngine for Fake {
        fn open(&mut self, path: &std::path::Path) -> Result<(), String> {
            let mut log = self.log.lock().unwrap();
            log.opened = Some(path.to_path_buf());
            match log.open_error.clone() {
                Some(error) => Err(error),
                None => Ok(()),
            }
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
        fn seek_preview(&mut self, seconds: f64) {
            let mut log = self.log.lock().unwrap();
            log.last_preview_seek = Some(seconds);
            log.preview_seeks += 1;
        }
        fn is_seeking(&self) -> bool {
            self.log.lock().unwrap().seeking
        }
        fn set_volume(&mut self, volume: f32) {
            self.log.lock().unwrap().volume = Some(volume);
        }
        fn set_speed(&mut self, speed: f32) {
            self.log.lock().unwrap().speed = Some(speed);
        }
        fn position(&self) -> f64 {
            self.log.lock().unwrap().position
        }
        fn duration(&self) -> Option<f64> {
            Some(self.log.lock().unwrap().duration)
        }
        fn state(&self) -> PlaybackState {
            self.log.lock().unwrap().state.clone()
        }
        fn frame_duration(&self) -> Option<f64> {
            Some(self.log.lock().unwrap().frame_duration)
        }
        fn current_path(&self) -> Option<PathBuf> {
            self.log.lock().unwrap().opened.clone()
        }
        fn surface(&self) -> rune::NativeSurface {
            self.surface.clone()
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
        fn take_command_failure(&mut self) -> Option<String> {
            self.log.lock().unwrap().command_failure.take()
        }
    }

    struct ManualClock {
        now: Mutex<Instant>,
    }

    impl ManualClock {
        fn new() -> Self {
            Self {
                now: Mutex::new(Instant::now()),
            }
        }

        fn advance(&self, duration: Duration) {
            let mut now = self.now.lock().unwrap();
            *now += duration;
        }
    }

    impl Clock for ManualClock {
        fn now(&self) -> Instant {
            *self.now.lock().unwrap()
        }
    }

    fn app_with_fake() -> (App, Arc<Mutex<FakeLog>>) {
        let log = Arc::new(Mutex::new(FakeLog::default()));
        let engine = Fake {
            surface: rune::NativeSurface::new(),
            log: log.clone(),
        };
        let tracks = engine.tracks().expect("the fake track list");
        let session = PlaybackSession::with_source(
            Box::new(engine),
            PathBuf::from("test.mkv"),
            TracksState::Loaded(tracks),
        );
        (App::with_session(Some(session)), log)
    }

    fn app_with_fake_and_clock() -> (App, Arc<Mutex<FakeLog>>, Arc<ManualClock>) {
        let log = Arc::new(Mutex::new(FakeLog::default()));
        let clock = Arc::new(ManualClock::new());
        let engine = Fake {
            surface: rune::NativeSurface::new(),
            log: log.clone(),
        };
        let tracks = engine.tracks().expect("the fake track list");
        let session = PlaybackSession::with_source_and_clock(
            Box::new(engine),
            PathBuf::from("test.mkv"),
            TracksState::Loaded(tracks),
            clock.clone(),
        );
        (App::with_session(Some(session)), log, clock)
    }

    fn session(app: &App) -> &PlaybackSession {
        app.session.as_ref().expect("a playback session")
    }

    fn session_mut(app: &mut App) -> &mut PlaybackSession {
        app.session.as_mut().expect("a playback session")
    }

    fn window() -> (f32, f32) {
        (1280.0, 800.0)
    }

    fn click(runtime: &mut Runtime<App>, key: &str) {
        let (x, y) = runtime.find(key).unwrap_or_else(|| panic!("{key}"));
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(1.0 / 60.0);
    }

    fn rect(runtime: &Runtime<App>, key: &str) -> [f32; 4] {
        runtime
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("{key}"))
            .rect
    }

    fn shows_text(runtime: &Runtime<App>, needle: &str) -> bool {
        runtime.elements().into_iter().any(|element| {
            element
                .text
                .as_deref()
                .is_some_and(|text| text.contains(needle))
        })
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kachito-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_stuck_seek_clears_at_five_seconds_and_restores_the_engine_position() {
        let (mut app, log, clock) = app_with_fake_and_clock();
        let session = session_mut(&mut app);
        session.timeline_mut().set_duration(10.0, 1.0 / 60.0);
        session.timeline_mut().set_position(5.0);
        {
            let mut log = log.lock().unwrap();
            log.seeking = true;
            log.position = 2.0;
        }

        session.seek(5.0);
        let mut tx = Tx::default();
        session.poll(&mut tx);
        assert!(session.is_scrubbing());
        assert!(format!("{tx:?}").contains("16ms"), "{tx:?}");

        clock.advance(Duration::from_millis(4_999));
        let mut tx = Tx::default();
        session.poll(&mut tx);
        assert!(
            session.is_scrubbing(),
            "the seek cleared before the deadline"
        );
        assert!(format!("{tx:?}").contains("16ms"), "{tx:?}");

        clock.advance(Duration::from_millis(1));
        let mut tx = Tx::default();
        session.poll(&mut tx);
        assert!(
            !session.is_scrubbing(),
            "the stuck seek stayed pending past the deadline"
        );
        assert_eq!(session.timeline().position(), 2.0);
        assert!(!format!("{tx:?}").contains("16ms"), "{tx:?}");
        let notice = session.error().expect("one notice");
        assert!(notice.contains("seek"), "{notice}");

        let mut tx = Tx::default();
        session.poll(&mut tx);
        assert_eq!(session.error().as_deref(), Some(notice.as_str()));
    }

    #[test]
    fn a_stuck_seek_expires_after_the_engine_fails() {
        let (mut app, log, clock) = app_with_fake_and_clock();
        let session = session_mut(&mut app);
        session.timeline_mut().set_duration(10.0, 1.0 / 60.0);
        session.timeline_mut().set_position(5.0);
        {
            let mut log = log.lock().unwrap();
            log.seeking = true;
            log.position = 2.0;
        }
        session.seek(5.0);
        log.lock().unwrap().state = PlaybackState::Failed("the decoder stopped".into());

        let mut tx = Tx::default();
        session.poll(&mut tx);
        assert!(session.is_scrubbing());
        assert!(
            format!("{tx:?}").contains("5s"),
            "the deadline has no wake: {tx:?}"
        );

        clock.advance(Duration::from_secs(5));
        let mut tx = Tx::default();
        session.poll(&mut tx);

        assert!(
            !session.is_scrubbing(),
            "the failed engine kept the seek pending"
        );
        assert_eq!(session.timeline().position(), 2.0);
        let notice = session.error().expect("one notice");
        assert!(notice.contains("did not finish in time"), "{notice}");
    }

    #[test]
    fn a_stuck_seek_expires_after_the_engine_goes_empty() {
        let (mut app, log, clock) = app_with_fake_and_clock();
        let session = session_mut(&mut app);
        session.timeline_mut().set_duration(10.0, 1.0 / 60.0);
        session.timeline_mut().set_position(5.0);
        {
            let mut log = log.lock().unwrap();
            log.seeking = true;
            log.position = 2.0;
        }
        session.seek(5.0);
        {
            let mut log = log.lock().unwrap();
            log.seeking = false;
            log.position = 0.0;
            log.state = PlaybackState::Empty;
        }

        clock.advance(Duration::from_millis(4_999));
        let mut tx = Tx::default();
        session.poll(&mut tx);
        assert!(
            session.is_scrubbing(),
            "the empty engine cleared the seek before the deadline"
        );

        clock.advance(Duration::from_millis(1));
        session.poll(&mut tx);
        assert!(!session.is_scrubbing());
        assert_eq!(session.timeline().position(), 0.0);
        let notice = session.error().expect("one notice");
        assert!(notice.contains("did not finish in time"), "{notice}");
    }

    #[test]
    fn a_seek_deadline_does_not_consume_the_command_notice() {
        let (mut app, log, clock) = app_with_fake_and_clock();
        let session = session_mut(&mut app);
        session.timeline_mut().set_duration(10.0, 1.0 / 60.0);
        session.timeline_mut().set_position(5.0);
        {
            let mut log = log.lock().unwrap();
            log.seeking = true;
            log.position = 2.0;
            log.command_failure = Some("set volume 40: rejected".into());
        }
        session.seek(5.0);

        clock.advance(Duration::from_secs(5));
        let mut tx = Tx::default();
        session.poll(&mut tx);
        let notice = session.error().expect("one notice");
        assert!(notice.contains("did not finish in time"), "{notice}");

        log.lock().unwrap().command_failure = Some("set speed 2: rejected".into());
        session.poll(&mut tx);
        let notice = session.error().expect("one notice");
        assert!(
            notice.contains("set speed 2"),
            "the deadline consumed the command notice: {notice}"
        );
    }

    #[test]
    fn a_seek_deadline_is_logged_once() {
        let (log, sink, path) = crate::log::testing::log_to_file("seek-deadline");
        let fake = Arc::new(Mutex::new(FakeLog::default()));
        let clock = Arc::new(ManualClock::new());
        let engine = Fake {
            surface: rune::NativeSurface::new(),
            log: fake.clone(),
        };
        let tracks = engine.tracks().expect("the fake track list");
        let mut session = PlaybackSession::with_source_clock_and_log(
            Box::new(engine),
            PathBuf::from("test.mkv"),
            TracksState::Loaded(tracks),
            log,
            clock.clone(),
        );
        session.timeline_mut().set_duration(10.0, 1.0 / 60.0);
        {
            let mut fake = fake.lock().unwrap();
            fake.seeking = true;
            fake.position = 2.0;
        }
        session.seek(5.0);
        clock.advance(Duration::from_secs(5));
        let mut tx = Tx::default();
        session.poll(&mut tx);
        session.poll(&mut tx);

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("notice: seek deadline").count(), 1, "{text}");
        assert_eq!(sink.lines(), text.lines().collect::<Vec<_>>());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn selecting_a_track_updates_the_cached_list_without_refetching() {
        let (app, log) = app_with_fake();
        assert_eq!(log.lock().unwrap().tracks_reads, 1);
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);

        click(&mut runtime, "audio-menu");
        click(&mut runtime, "audio-2");
        let tracks = match session(&runtime.state).tracks() {
            TracksState::Loaded(tracks) => tracks,
            other => panic!("{other:?}"),
        };
        assert!(!tracks.iter().find(|track| track.id == 1).unwrap().selected);
        assert!(tracks.iter().find(|track| track.id == 2).unwrap().selected);
        assert_eq!(log.lock().unwrap().tracks_reads, 1);

        click(&mut runtime, "subs-menu");
        click(&mut runtime, "sub-10");
        let tracks = match session(&runtime.state).tracks() {
            TracksState::Loaded(tracks) => tracks,
            other => panic!("{other:?}"),
        };
        assert!(tracks.iter().find(|track| track.id == 10).unwrap().selected);
        assert_eq!(log.lock().unwrap().tracks_reads, 1);

        click(&mut runtime, "subs-menu");
        click(&mut runtime, "subs-off");
        let tracks = match session(&runtime.state).tracks() {
            TracksState::Loaded(tracks) => tracks,
            other => panic!("{other:?}"),
        };
        assert!(
            !tracks
                .iter()
                .any(|track| track.selected && track.kind == TrackKind::Subtitle)
        );
        assert_eq!(log.lock().unwrap().tracks_reads, 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_source_path_opens() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = temp_dir("non-utf8-open");
        let source = dir.join(OsString::from_vec(b"clip-\xff.mkv".to_vec()));
        std::fs::write(&source, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        app.open(source.clone());
        assert_eq!(app.error_text(), None);
        assert!(session(&app).is_awaiting_source());
        assert_eq!(
            log.lock().unwrap().opened.as_deref(),
            Some(source.as_path())
        );

        let state = session(&app).state();
        assert!(session_mut(&mut app).confirm_open(&state));
        assert_eq!(session(&app).source(), Some(source.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_first_rejected_command_raises_one_notice_per_file() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        assert!(!shows_text(&runtime, "The player could not apply"));

        log.lock().unwrap().command_failure = Some("set volume 40: property unavailable".into());
        runtime.frame(1.0 / 60.0);
        assert!(
            shows_text(&runtime, "set volume 40"),
            "the first rejection raises the notice"
        );

        log.lock().unwrap().command_failure =
            Some("seek 5.000 absolute+exact: invalid parameter".into());
        runtime.frame(1.0 / 60.0);
        assert!(
            shows_text(&runtime, "set volume 40"),
            "the first notice stays on screen"
        );
        assert!(
            !shows_text(&runtime, "seek 5.000"),
            "a second rejection is quiet"
        );
    }

    #[test]
    fn the_newest_notice_wins() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);

        log.lock().unwrap().command_failure = Some("set volume 40: rejected".into());
        runtime.frame(1.0 / 60.0);
        assert!(shows_text(&runtime, "set volume 40"));

        runtime.state.set_export_error("the export failed".into());
        runtime.frame(1.0 / 60.0);
        assert!(shows_text(&runtime, "the export failed"));
        assert!(!shows_text(&runtime, "set volume 40"));

        log.lock().unwrap().state = PlaybackState::Failed("the engine failed".into());
        runtime.frame(1.0 / 60.0);
        assert!(shows_text(&runtime, "the engine failed"));
        assert!(!shows_text(&runtime, "the export failed"));

        log.lock().unwrap().state = PlaybackState::Paused;
        runtime
            .state
            .set_export_error("the newer export failed".into());
        runtime.frame(1.0 / 60.0);
        assert!(shows_text(&runtime, "the newer export failed"));
        assert!(!shows_text(&runtime, "the engine failed"));
    }

    #[test]
    fn a_rejected_load_does_not_consume_the_command_notice() {
        let dir = temp_dir("rejected-load-notice");
        let corrupt = dir.join("corrupt.mkv");
        std::fs::write(&corrupt, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        {
            let mut log = log.lock().unwrap();
            log.state = PlaybackState::Loading;
            log.open_error = Some("loadfile rejected".into());
            log.command_failure = Some("loadfile corrupt.mkv: loading failed".into());
        }
        assert!(session_mut(&mut app).open(corrupt).is_err());
        log.lock().unwrap().state = PlaybackState::Failed("loadfile rejected".into());

        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        assert!(shows_text(&runtime, "loadfile rejected"));
        assert!(!shows_text(&runtime, "The player could not apply"));

        log.lock().unwrap().command_failure = Some("set volume 40: rejected".into());
        runtime.frame(1.0 / 60.0);
        assert!(
            shows_text(&runtime, "set volume 40"),
            "the later rejection gets the one notice"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_successful_open_clears_the_command_notice() {
        let dir = temp_dir("clear-command-notice");
        let next = dir.join("next.mkv");
        std::fs::write(&next, b"not a video").unwrap();
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        log.lock().unwrap().command_failure = Some("set pause yes: rejected".into());
        runtime.frame(1.0 / 60.0);
        assert!(runtime.find("error-banner-card").is_some());

        runtime.state.open(next);
        runtime.frame(1.0 / 60.0);
        assert_eq!(runtime.state.error_text(), None);
        assert!(
            runtime.find("error-banner-card").is_none(),
            "the successful open clears the notice"
        );

        log.lock().unwrap().command_failure = Some("seek 1.000 absolute+exact: rejected".into());
        runtime.frame(1.0 / 60.0);
        assert!(
            runtime.find("error-banner-card").is_some(),
            "the next file gets its own one notice"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rejected_command_is_logged_by_the_session() {
        let (log, sink, path) = crate::log::testing::log_to_file("session-notice");
        let fake = Arc::new(Mutex::new(FakeLog::default()));
        let engine = Fake {
            surface: rune::NativeSurface::new(),
            log: fake.clone(),
        };
        let tracks = engine.tracks().expect("the fake track list");
        let session = PlaybackSession::with_source_and_log(
            Box::new(engine),
            PathBuf::from("test.mkv"),
            TracksState::Loaded(tracks),
            log,
        );
        let app = App::with_session(Some(session));
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        fake.lock().unwrap().command_failure = Some("set speed 0: invalid parameter".into());
        runtime.frame(1.0 / 60.0);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("notice: rejected command: set speed 0: invalid parameter"),
            "{text}"
        );
        assert_eq!(sink.lines(), text.lines().collect::<Vec<_>>());
        let _ = std::fs::remove_file(path);
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
        let dir = temp_dir("drop");
        let dropped = dir.join("dropped.mkv");
        std::fs::write(&dropped, b"not a video").unwrap();
        let log = Arc::new(Mutex::new(FakeLog::default()));
        let playback = PlaybackSession::new(
            Box::new(Fake {
                surface: rune::NativeSurface::new(),
                log: log.clone(),
            }),
            Arc::new(RunLog::disabled()),
        );
        let app = App::with_session(Some(playback));
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        let at = runtime.find("video").unwrap_or((10.0, 10.0));
        runtime.drop_files(vec![dropped.clone()], Some(at));
        runtime.frame(1.0 / 60.0);
        let state = session(&runtime.state).state();
        assert!(session_mut(&mut runtime.state).confirm_open(&state));
        assert_eq!(session(&runtime.state).source(), Some(dropped.as_path()));
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
        app.set_speed(1.25);
        assert_eq!(log.lock().unwrap().speed, Some(1.25));
        app.seek_fraction(0.5);
        assert_eq!(log.lock().unwrap().last_seek, Some(5.0));
    }

    #[test]
    fn a_non_video_file_is_rejected() {
        let (mut app, log) = app_with_fake();
        app.open(PathBuf::from("notes.txt"));
        assert!(app.error.is_some());
        assert!(
            session(&app).source().is_some(),
            "the previous file stays open"
        );
        assert_eq!(log.lock().unwrap().opened, None);
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
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(12.0, DEFAULT_FRAME);
        session_mut(&mut app).timeline_mut().set_in(3.0);
        session_mut(&mut app).timeline_mut().set_out(8.0);
        let old_path = session(&app).source().map(|path| path.to_path_buf());
        let old_tracks = session(&app).tracks().clone();
        let old_timeline = session(&app).timeline().clone();

        app.open(PathBuf::from("missing.mkv"));
        assert_eq!(session(&app).source(), old_path.as_deref());
        assert_eq!(session(&app).tracks(), &old_tracks);
        assert_eq!(session(&app).timeline(), &old_timeline);
        assert!(!session(&app).is_awaiting_source());
        assert!(app.error_text().is_some());
        assert_eq!(log.lock().unwrap().opened, None);
    }

    #[test]
    fn a_failed_load_keeps_the_previous_session() {
        let dir = temp_dir("corrupt");
        let corrupt = dir.join("corrupt.mkv");
        std::fs::write(&corrupt, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(12.0, DEFAULT_FRAME);
        session_mut(&mut app).timeline_mut().set_in(3.0);
        session_mut(&mut app).timeline_mut().set_out(8.0);
        let old_path = session(&app).source().map(|path| path.to_path_buf());
        let old_tracks = session(&app).tracks().clone();
        let old_timeline = session(&app).timeline().clone();
        log.lock().unwrap().state = PlaybackState::Loading;

        app.open(corrupt);
        assert_eq!(session(&app).source(), old_path.as_deref());
        assert_eq!(session(&app).tracks(), &old_tracks);
        assert_eq!(session(&app).timeline(), &old_timeline);
        assert!(session(&app).is_awaiting_source());

        log.lock().unwrap().state = PlaybackState::Failed("corrupt".into());
        let state = session(&app).state();
        assert!(session_mut(&mut app).confirm_open(&state));
        assert_eq!(session(&app).source(), old_path.as_deref());
        assert_eq!(session(&app).tracks(), &old_tracks);
        assert_eq!(session(&app).timeline(), &old_timeline);
        assert!(!session(&app).is_awaiting_source());
        assert_eq!(app.error_text().as_deref(), Some("corrupt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_valid_open_commits_only_when_the_engine_is_ready() {
        let dir = temp_dir("next");
        let next = dir.join("next.mkv");
        std::fs::write(&next, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(12.0, DEFAULT_FRAME);
        session_mut(&mut app).timeline_mut().set_in(3.0);
        session_mut(&mut app).timeline_mut().set_out(8.0);
        let old_path = session(&app).source().map(|path| path.to_path_buf());
        log.lock().unwrap().state = PlaybackState::Loading;

        app.open(next.clone());
        assert_eq!(session(&app).source(), old_path.as_deref());
        assert!(session(&app).is_awaiting_source());

        let state = session(&app).state();
        assert!(!session_mut(&mut app).confirm_open(&state));
        assert_eq!(session(&app).source(), old_path.as_deref());

        log.lock().unwrap().state = PlaybackState::Paused;
        let state = session(&app).state();
        assert!(session_mut(&mut app).confirm_open(&state));
        assert_eq!(session(&app).source(), Some(next.as_path()));
        assert_eq!(session(&app).tracks(), &TracksState::Unloaded);
        assert_eq!(session(&app).timeline(), &Timeline::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_successful_source_swap_clears_the_old_export() {
        let dir = temp_dir("swap");
        let next = dir.join("next.mkv");
        std::fs::write(&next, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        session_mut(&mut app).set_export(Some(ExportJob::with_state(ExportState::Finished)));
        session_mut(&mut app).set_export_error("old export error".into());
        log.lock().unwrap().state = PlaybackState::Loading;

        app.open(next);
        log.lock().unwrap().state = PlaybackState::Paused;
        let state = session(&app).state();
        assert!(session_mut(&mut app).confirm_open(&state));
        assert_eq!(session(&app).export_state(), None);
        assert_eq!(session(&app).export_error(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_open_error_wins_over_a_stale_export_error() {
        let dir = temp_dir("stale-export-error");
        let corrupt = dir.join("corrupt.mkv");
        std::fs::write(&corrupt, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        session_mut(&mut app).set_export_error("old export error".into());
        log.lock().unwrap().state = PlaybackState::Loading;

        app.open(corrupt);
        log.lock().unwrap().state = PlaybackState::Failed("corrupt".into());
        let state = session(&app).state();
        assert!(session_mut(&mut app).confirm_open(&state));
        assert_eq!(app.error_text().as_deref(), Some("corrupt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_app_poll_commits_a_ready_source_and_clears_the_export() {
        let dir = temp_dir("poll-ready");
        let next = dir.join("next.mkv");
        std::fs::write(&next, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        session_mut(&mut app).set_export(Some(ExportJob::with_state(ExportState::Running {
            fraction: 0.5,
        })));
        let old_path = session(&app).source().map(|path| path.to_path_buf());
        log.lock().unwrap().state = PlaybackState::Loading;
        app.open(next.clone());

        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        assert_eq!(session(&runtime.state).source(), old_path.as_deref());

        log.lock().unwrap().state = PlaybackState::Paused;
        runtime.frame(1.0 / 60.0);
        assert_eq!(session(&runtime.state).source(), Some(next.as_path()));
        assert_eq!(session(&runtime.state).export_state(), None);
        assert!(runtime.find("export-status").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_app_poll_keeps_the_previous_session_on_a_failed_open() {
        let dir = temp_dir("poll-failed");
        let corrupt = dir.join("corrupt.mkv");
        std::fs::write(&corrupt, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(12.0, DEFAULT_FRAME);
        session_mut(&mut app).timeline_mut().set_in(3.0);
        session_mut(&mut app).timeline_mut().set_out(8.0);
        let old_path = session(&app).source().map(|path| path.to_path_buf());
        let old_timeline = session(&app).timeline().clone();
        log.lock().unwrap().state = PlaybackState::Loading;
        app.open(corrupt);

        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        log.lock().unwrap().state = PlaybackState::Failed("corrupt".into());
        runtime.frame(1.0 / 60.0);
        assert_eq!(session(&runtime.state).source(), old_path.as_deref());
        assert_eq!(session(&runtime.state).timeline(), &old_timeline);
        assert!(!session(&runtime.state).is_awaiting_source());
        assert_eq!(runtime.state.error_text().as_deref(), Some("corrupt"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_trackless_file_reads_the_track_list_once() {
        let (mut app, log) = app_with_fake();
        session_mut(&mut app).set_tracks(TracksState::Unloaded);
        {
            let mut log = log.lock().unwrap();
            log.tracks_result = Some(Ok(Vec::new()));
            log.tracks_reads = 0;
        }
        let state = session(&app).state();
        assert!(session_mut(&mut app).load_tracks(&state));
        assert_eq!(session(&app).tracks(), &TracksState::Loaded(Vec::new()));
        assert_eq!(log.lock().unwrap().tracks_reads, 1);
        assert!(!session_mut(&mut app).load_tracks(&state));
        assert_eq!(log.lock().unwrap().tracks_reads, 1);
    }

    #[test]
    fn a_track_list_failure_is_visible_and_distinct() {
        let (mut app, log) = app_with_fake();
        session_mut(&mut app).set_tracks(TracksState::Unloaded);
        {
            let mut log = log.lock().unwrap();
            log.tracks_result = Some(Err("track list unavailable".into()));
            log.tracks_reads = 0;
        }
        let state = session(&app).state();
        assert!(session_mut(&mut app).load_tracks(&state));
        assert_eq!(
            session(&app).tracks(),
            &TracksState::Failed("track list unavailable".into())
        );
        assert_eq!(log.lock().unwrap().tracks_reads, 1);
        assert!(!session_mut(&mut app).load_tracks(&state));
        assert_eq!(log.lock().unwrap().tracks_reads, 1);
    }

    #[test]
    fn the_track_menu_distinguishes_loading_empty_and_failure() {
        for (tracks, key) in [
            (TracksState::Unloaded, "track-loading"),
            (TracksState::Loaded(Vec::new()), "no-audio"),
            (
                TracksState::Failed("track list unavailable".into()),
                "track-error",
            ),
        ] {
            let (mut app, _) = app_with_fake();
            session_mut(&mut app).set_tracks(tracks);
            let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
            runtime.frame(0.0);
            click(&mut runtime, "audio-menu");
            assert!(runtime.find(key).is_some(), "{key}");
        }
    }

    #[test]
    fn the_subtitle_menu_distinguishes_loading_empty_and_failure() {
        for (tracks, key) in [
            (TracksState::Unloaded, "track-loading"),
            (TracksState::Loaded(Vec::new()), "no-subtitles"),
            (
                TracksState::Failed("track list unavailable".into()),
                "track-error",
            ),
        ] {
            let (mut app, _) = app_with_fake();
            session_mut(&mut app).set_tracks(tracks);
            let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
            runtime.frame(0.0);
            click(&mut runtime, "subs-menu");
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
    fn the_session_uses_the_source_frame_duration() {
        let (app, log) = app_with_fake();
        log.lock().unwrap().frame_duration = 1.0 / 24.0;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        let timeline = session(&runtime.state).timeline();
        assert!((timeline.frame_duration() - 1.0 / 24.0).abs() < 1e-9);
        session_mut(&mut runtime.state).timeline_mut().set_out(0.01);
        assert!(session(&runtime.state).timeline().out_point() >= 1.0 / 24.0);
    }

    #[test]
    fn audio_and_subtitle_menus_pick_tracks() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);

        click(&mut runtime, "audio-menu");
        click(&mut runtime, "audio-2");
        assert_eq!(log.lock().unwrap().audio, Some(2));

        click(&mut runtime, "subs-menu");
        click(&mut runtime, "sub-10");
        assert_eq!(log.lock().unwrap().subtitle, Some(10));

        click(&mut runtime, "subs-menu");
        click(&mut runtime, "subs-off");
        assert_eq!(log.lock().unwrap().subtitle, None);
    }

    #[test]
    fn the_export_tray_renders_each_export_state() {
        let states = [
            ExportState::Running { fraction: 0.5 },
            ExportState::Finished,
            ExportState::Cancelled,
            ExportState::Failed("export failed".into()),
        ];
        for state in states {
            let (mut app, _) = app_with_fake();
            session_mut(&mut app).set_export(Some(ExportJob::with_state(state.clone())));
            let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
            runtime.frame(0.0);
            assert!(runtime.find("export-tray").is_none(), "{state:?}");
            assert!(runtime.find("export-status").is_some(), "{state:?}");
            click(&mut runtime, "export-toggle");
            runtime.frame(0.2);
            assert!(runtime.find("export-tray").is_some(), "{state:?}");
            if matches!(state, ExportState::Running { .. }) {
                assert!(runtime.find("cancel-export").is_some(), "{state:?}");
                assert!(runtime.find("export").is_none(), "{state:?}");
            } else {
                assert!(runtime.find("cancel-export").is_none(), "{state:?}");
                assert!(runtime.find("export").is_some(), "{state:?}");
            }
        }
    }

    #[test]
    fn the_shell_fits_both_window_sizes() {
        for size in [(1280.0, 800.0), (960.0, 600.0)] {
            let (app, log) = app_with_fake();
            let mut runtime = Runtime::new(app, view, size, Env::new(true, 1.0));
            runtime.frame(0.0);

            let video = rect(&runtime, "video");
            let transport = rect(&runtime, "transport");
            assert!(video[0] >= 0.0, "{size:?} {video:?}");
            assert!(video[1] >= 48.0, "{size:?} {video:?}");
            assert!(video[0] + video[2] <= size.0 + 0.1, "{size:?} {video:?}");
            assert!(video[1] + video[3] <= size.1 + 0.1, "{size:?} {video:?}");
            assert!(
                (video[2] / video[3] - 16.0 / 9.0).abs() < 0.01,
                "{size:?} {video:?}"
            );
            let centre_x = video[0] + video[2] / 2.0;
            let centre_y = video[1] + video[3] / 2.0;
            assert!((centre_x - size.0 / 2.0).abs() < 0.5, "{size:?}");
            assert!(
                (centre_y - (48.0 + size.1) / 2.0).abs() < 0.5,
                "{size:?} {centre_y}"
            );
            assert!(
                transport[1] + transport[3] <= size.1,
                "{size:?} {transport:?}"
            );

            let (x, y) = runtime.find("play").expect("the play button");
            runtime.pointer_down(x, y);
            runtime.pointer_up(x, y);
            runtime.frame(1.0 / 60.0);
            assert_eq!(log.lock().unwrap().state, PlaybackState::Playing);
        }
    }

    #[test]
    fn the_overflow_changes_speed_and_fullscreen_closes_it() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        click(&mut runtime, "more");
        click(&mut runtime, "speed-1.5");
        assert_eq!(log.lock().unwrap().speed, Some(1.5));
        assert!(runtime.find("more-menu-panel").is_some());
        click(&mut runtime, "fullscreen");
        assert!(runtime.find("more-menu-panel").is_none());
    }

    #[test]
    fn the_transport_hides_while_playing_and_returns_on_pointer_move() {
        let (app, log) = app_with_fake();
        log.lock().unwrap().state = PlaybackState::Playing;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        let (x, y) = runtime.find("play").expect("the play button");
        assert!(runtime.find("play").is_some());
        runtime.frame(3.0);
        assert!(runtime.find("play").is_none());

        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(3.1);
        assert_eq!(log.lock().unwrap().state, PlaybackState::Playing);
        assert!(runtime.find("play").is_some());

        runtime.pointer_move(10.0, 100.0);
        runtime.frame(3.2);
        assert!(runtime.find("play").is_some());
    }

    #[test]
    fn keyboard_use_reveals_the_transport() {
        let (app, log) = app_with_fake();
        log.lock().unwrap().state = PlaybackState::Playing;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.on_key = Some(on_key);
        runtime.frame(0.0);
        runtime.frame(3.0);
        assert!(runtime.find("play").is_none());

        runtime.key(KeyEvent {
            key: Key::Tab,
            mods: Default::default(),
        });
        runtime.frame(3.1);
        assert!(runtime.find("play").is_some());
        runtime.frame(3.0);
        assert!(runtime.find("play").is_none());
    }

    #[test]
    fn escape_closes_the_export_tray() {
        let (app, _) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.on_key = Some(on_key);
        runtime.frame(0.0);
        click(&mut runtime, "export-toggle");
        assert!(runtime.find("export-tray").is_some());
        runtime.key(KeyEvent {
            key: Key::Escape,
            mods: Default::default(),
        });
        runtime.frame(0.5);
        assert!(runtime.find("export-tray").is_none());
        assert!(runtime.find("transport").is_some());
    }

    #[test]
    fn keyboard_controls_playback_and_volume() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.on_key = Some(on_key);
        runtime.frame(0.0);
        runtime.key(KeyEvent {
            key: Key::Text(" ".into()),
            mods: Default::default(),
        });
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().state, PlaybackState::Playing);
        runtime.key(KeyEvent {
            key: Key::Right,
            mods: Default::default(),
        });
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().last_seek, Some(10.0));
        runtime.key(KeyEvent {
            key: Key::Text("m".into()),
            mods: Default::default(),
        });
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().volume, Some(0.0));
        runtime.key(KeyEvent {
            key: Key::Up,
            mods: Default::default(),
        });
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().volume, Some(0.05));
        runtime.key(KeyEvent {
            key: Key::Down,
            mods: Default::default(),
        });
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().volume, Some(0.0));

        runtime.key(KeyEvent {
            key: Key::Text("e".into()),
            mods: Default::default(),
        });
        runtime.frame(1.0 / 60.0);
        assert!(runtime.find("export-tray").is_some());
        runtime.key(KeyEvent {
            key: Key::Escape,
            mods: Default::default(),
        });
        runtime.frame(0.5);
        assert!(runtime.find("export-tray").is_none());
    }

    #[test]
    fn dragging_the_seek_bar_previews_and_then_seeks_exactly() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);

        let [x, y, w, h] = rect(&runtime, "seek");
        let y = y + h / 2.0;
        runtime.pointer_down(x + w * 0.1, y);
        runtime.frame(1.0 / 60.0);
        runtime.pointer_move(x + w * 0.5, y);
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().last_preview_seek, Some(5.0));

        runtime.pointer_up(x + w * 0.5, y);
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().last_seek, Some(5.0));
    }

    #[test]
    fn the_seek_handle_dims_while_the_engine_seeks() {
        let (mut app, log) = app_with_fake();
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(10.0, 1.0 / 60.0);
        log.lock().unwrap().seeking = true;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);

        let fill = |runtime: &Runtime<App>| {
            runtime
                .elements()
                .into_iter()
                .find(|element| element.key.as_deref() == Some("seek-handle"))
                .expect("the seek handle")
                .fill
        };
        let palette = ui::palette();
        assert_eq!(fill(&runtime), palette.accent.alpha(0.55));

        log.lock().unwrap().seeking = false;
        runtime.pointer_move(10.0, 10.0);
        for _ in 0..60 {
            runtime.frame(1.0 / 60.0);
        }
        assert!(fill(&runtime).a > 0.99);
    }

    #[test]
    fn the_export_range_body_previews_and_then_seeks_exactly() {
        let (mut app, log) = app_with_fake();
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(10.0, 1.0 / 60.0);
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        click(&mut runtime, "export-toggle");
        runtime.frame(0.2);

        let [x, y, w, h] = rect(&runtime, "range");
        let y = y + h / 2.0;
        runtime.pointer_down(x + w * 0.2, y);
        runtime.frame(1.0 / 60.0);
        runtime.pointer_move(x + w * 0.6, y);
        runtime.frame(1.0 / 60.0);
        runtime.pointer_up(x + w * 0.6, y);
        runtime.frame(1.0 / 60.0);

        let preview = log.lock().unwrap().last_preview_seek.expect("a preview");
        assert!((preview - 6.0).abs() < 1e-4);
        let exact = log.lock().unwrap().last_seek.expect("an exact seek");
        assert!((exact - 6.0).abs() < 1e-4);
    }

    #[test]
    fn the_seek_bar_follows_the_timeline_not_the_engine_during_a_scrub() {
        let (mut app, log) = app_with_fake();
        log.lock().unwrap().position = 0.0;
        let session = session_mut(&mut app);
        session.timeline_mut().set_duration(10.0, 1.0 / 60.0);
        session.timeline_mut().set_position(5.0);
        session.set_scrubbing(true);

        assert_eq!(app_position(&app), 5.0);
    }

    #[test]
    fn a_scrub_keeps_the_timeline_position_while_the_engine_lags() {
        let (mut app, log) = app_with_fake();
        log.lock().unwrap().position = 0.0;
        let session = session_mut(&mut app);
        session.timeline_mut().set_duration(10.0, 1.0 / 60.0);
        session.timeline_mut().set_position(5.0);
        session.set_scrubbing(true);

        let mut tx = Tx::default();
        session.poll(&mut tx);
        assert_eq!(session.timeline().position(), 5.0);

        session.seek(5.0);
        session.poll(&mut tx);
        assert_eq!(
            session.timeline().position(),
            5.0,
            "the preview position must survive the exact seek"
        );

        log.lock().unwrap().position = 5.0;
        session.poll(&mut tx);
        assert_eq!(session.timeline().position(), 5.0);

        log.lock().unwrap().position = 0.0;
        session.poll(&mut tx);
        assert_eq!(
            session.timeline().position(),
            0.0,
            "sync resumes after the exact seek lands"
        );
    }

    #[test]
    fn preview_requests_keep_the_latest_target_while_the_engine_seeks() {
        let (mut app, log) = app_with_fake();
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(10.0, 1.0 / 60.0);
        log.lock().unwrap().seeking = true;

        app.preview_fraction(0.2);
        app.preview_fraction(0.4);
        app.preview_fraction(0.6);
        assert_eq!(log.lock().unwrap().preview_seeks, 0);
        assert!((session(&app).timeline().position() - 6.0).abs() < 1e-4);

        log.lock().unwrap().seeking = false;
        let mut tx = Tx::default();
        session_mut(&mut app).poll(&mut tx);
        assert_eq!(log.lock().unwrap().preview_seeks, 1);
        let preview = log.lock().unwrap().last_preview_seek.expect("a preview");
        assert!((preview - 6.0).abs() < 1e-4);
    }

    #[test]
    fn a_release_onto_the_landed_frame_skips_the_exact_seek() {
        let (mut app, log) = app_with_fake();
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(10.0, 1.0 / 60.0);
        log.lock().unwrap().position = 5.0;
        session_mut(&mut app).set_scrubbing(true);

        session_mut(&mut app).seek(5.0);
        assert_eq!(log.lock().unwrap().last_seek, None);

        let mut tx = Tx::default();
        session_mut(&mut app).poll(&mut tx);
        assert!((session(&app).timeline().position() - 5.0).abs() < 1e-4);
    }

    #[test]
    fn a_busy_engine_keeps_the_release_seek_pending() {
        let (mut app, log) = app_with_fake();
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(10.0, 1.0 / 60.0);
        {
            let mut log = log.lock().unwrap();
            log.position = 5.0;
            log.seeking = true;
        }
        let session = session_mut(&mut app);
        session.set_scrubbing(true);
        session.seek(5.0);
        assert!(session.is_scrubbing());

        let mut tx = Tx::default();
        session.poll(&mut tx);
        assert!(
            session.is_scrubbing(),
            "the busy engine cleared the release seek"
        );

        log.lock().unwrap().seeking = false;
        session.poll(&mut tx);
        assert!(!session.is_scrubbing());
        assert!((session.timeline().position() - 5.0).abs() < 1e-4);
    }

    #[test]
    fn a_click_on_the_seek_bar_does_not_preview() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);

        click(&mut runtime, "seek");

        assert_eq!(log.lock().unwrap().last_preview_seek, None);
        assert_eq!(log.lock().unwrap().last_seek, Some(5.0));
    }

    #[test]
    fn a_click_on_the_export_range_body_does_not_preview() {
        let (mut app, log) = app_with_fake();
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(10.0, 1.0 / 60.0);
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        click(&mut runtime, "export-toggle");
        runtime.frame(0.2);

        click(&mut runtime, "range");

        assert_eq!(log.lock().unwrap().last_preview_seek, None);
        assert_eq!(log.lock().unwrap().last_seek, Some(5.0));
    }

    #[test]
    fn a_clicked_slider_does_not_eat_the_global_arrows() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.on_key = Some(on_key);
        runtime.frame(0.0);
        click(&mut runtime, "seek");
        assert_eq!(log.lock().unwrap().last_seek, Some(5.0));
        runtime.key(KeyEvent {
            key: Key::Right,
            mods: Default::default(),
        });
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().last_seek, Some(10.0));

        click(&mut runtime, "volume");
        assert_eq!(log.lock().unwrap().volume, Some(0.5));
        runtime.key(KeyEvent {
            key: Key::Right,
            mods: Default::default(),
        });
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().last_seek, Some(10.0));
        assert_eq!(log.lock().unwrap().volume, Some(0.5));
    }

    #[test]
    fn the_empty_screen_keeps_the_permanent_bar() {
        let app = App::with_session(None);
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        for key in ["open", "audio-menu", "subs-menu", "export-toggle"] {
            assert!(runtime.find(key).is_some(), "{key}");
        }
        assert!(runtime.find("open-empty").is_some());
        click(&mut runtime, "audio-menu");
        assert!(runtime.find("audio-menu-panel").is_none());
    }

    #[test]
    fn a_long_error_banner_wraps_instead_of_truncating() {
        let (mut app, _) = app_with_fake();
        app.error = Some(
            "The playback engine could not open this file because the container is damaged.              Choose a different file or re-download the clip."
                .repeat(4),
        );
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        let card = rect(&runtime, "error-banner-card");
        assert!(card[3] > 38.0, "{card:?}");
    }

    #[test]
    fn mute_returns_to_the_previous_volume() {
        let (mut app, log) = app_with_fake();
        app.set_volume(0.4);
        app.toggle_mute();
        assert_eq!(app.volume, 0.0);
        assert_eq!(log.lock().unwrap().volume, Some(0.0));
        app.toggle_mute();
        assert_eq!(app.volume, 0.4);
        assert_eq!(log.lock().unwrap().volume, Some(0.4));
    }

    #[test]
    fn skip_clamps_to_the_file() {
        let (mut app, log) = app_with_fake();
        log.lock().unwrap().position = 9.0;
        app.skip(10.0);
        assert_eq!(log.lock().unwrap().last_seek, Some(10.0));
        log.lock().unwrap().position = 0.5;
        app.skip(-10.0);
        assert_eq!(log.lock().unwrap().last_seek, Some(0.0));
    }

    #[test]
    fn click_outside_closes_a_track_menu() {
        let (app, _) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        click(&mut runtime, "audio-menu");
        assert!(runtime.find("audio-menu-panel").is_some());
        runtime.pointer_down(10.0, 300.0);
        runtime.pointer_up(10.0, 300.0);
        runtime.frame(0.2);
        assert!(runtime.find("audio-menu-panel").is_none());
    }

    #[test]
    fn escape_closes_a_track_menu() {
        let (app, _) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        click(&mut runtime, "audio-menu");
        assert!(runtime.find("audio-menu-panel").is_some());
        runtime.key(KeyEvent {
            key: Key::Escape,
            mods: Default::default(),
        });
        runtime.frame(0.2);
        assert!(runtime.find("audio-menu-panel").is_none());
        let focused = runtime
            .elements()
            .into_iter()
            .find(|element| element.focused)
            .and_then(|element| element.key);
        assert_eq!(focused.as_deref(), Some("audio-menu"));
    }
    #[test]
    fn fullscreen_and_open_shortcuts_do_not_disturb_playback() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.on_key = Some(on_key);
        runtime.frame(0.0);
        runtime.key(KeyEvent {
            key: Key::Text("f".into()),
            mods: Default::default(),
        });
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().state, PlaybackState::Paused);
        runtime.key(KeyEvent {
            key: Key::Command('o'),
            mods: Mods {
                cmd: true,
                ctrl: true,
                ..Default::default()
            },
        });
        runtime.frame(1.0 / 60.0);
        assert_eq!(log.lock().unwrap().state, PlaybackState::Paused);
    }
    #[test]
    fn the_export_mode_can_change_before_a_run() {
        let (app, _) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        click(&mut runtime, "export-toggle");
        runtime.frame(0.2);
        assert_eq!(runtime.state.export_mode, ExportMode::Precise);
        click(&mut runtime, "mode-fast");
        runtime.frame(0.1);
        assert_eq!(runtime.state.export_mode, ExportMode::Fast);
    }

    #[test]
    fn the_out_handle_follows_the_pointer() {
        let (app, log) = app_with_fake();
        log.lock().unwrap().duration = 3600.0;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        click(&mut runtime, "export-toggle");
        runtime.frame(0.2);
        let range = rect(&runtime, "range");
        let y = range[1] + range[3] * 0.5;
        let from_x = range[0] + range[2] - 2.0;
        let to_x = range[0] + range[2] * 0.5;
        runtime.pointer_down(from_x, y);
        runtime.pointer_move(to_x, y);
        runtime.pointer_up(to_x, y);
        runtime.frame(0.1);
        let out = runtime
            .state
            .session
            .as_ref()
            .unwrap()
            .timeline()
            .out_point();
        assert!((out - 1800.0).abs() < 30.0, "{out}");
    }

    #[test]
    fn a_new_source_closes_the_export_tray() {
        let dir = temp_dir("new-source-tray");
        let next = dir.join("next.mkv");
        std::fs::write(&next, b"not a video").unwrap();
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        click(&mut runtime, "export-toggle");
        runtime.frame(0.2);
        assert!(runtime.find("export-tray").is_some());
        runtime.state.open(next);
        log.lock().unwrap().state = PlaybackState::Loading;
        runtime.frame(0.1);
        log.lock().unwrap().state = PlaybackState::Paused;
        runtime.frame(0.2);
        assert!(runtime.find("export-tray").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
