//! The Kachito screen: one file, one fitted stage, one floating transport.

use crate::engine::{MpvEngine, PlaybackState, Track, TrackKind, VIDEO_HEIGHT, VIDEO_WIDTH};
use crate::export::ExportState;
use crate::log::{RunLog, StderrSink};
use crate::session::{PlaybackSession, SUPPORTED, TracksState, tracks_of};
use crate::toast::{Toast, Toasts, Tone};
use crate::ui::{self, Palette, icons};
use rune::{
    AppSpec, Color, Cx, Drag, El, FileDialogOptions, Key, KeyEvent, LiveRegion, Phase, SurfaceFit,
    Transition, Tx, Waker, col, el, icon, row, text,
};
use std::path::PathBuf;
use std::sync::Arc;

const SPEEDS: &[f32] = &[0.5, 1.0, 1.25, 1.5, 2.0];
const HIDE_AFTER: f64 = 3.0;
const SKIP: f64 = 10.0;
const SCRUB_THRESHOLD: f32 = 2.0;
/// The toast stack's inset from the window's right and bottom edges.
const TOAST_INSET: f32 = 16.0;
/// The bottom clearance above the transport and the export tray.
const TOAST_TRANSPORT_CLEARANCE: f32 = 118.0;
const TOAST_EXPORT_CLEARANCE: f32 = 216.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Audio,
    Subtitles,
    Speed,
}

struct App {
    session: Option<PlaybackSession>,
    toasts: Toasts,
    /// The app clock at the last poll, so a handler can stamp a toast timer.
    now: f64,
    volume: f32,
    last_volume: f32,
    speed: f32,
    last_dir: Option<PathBuf>,
    menu: Option<Menu>,
    export_open: bool,
    top_visible: bool,
    top_hovered: bool,
    transport_visible: bool,
    transport_hovered: bool,
    viewport_h: f32,
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
                app.notify_error("Playback engine unavailable", error);
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
            toasts: Toasts::default(),
            now: 0.0,
            volume: 1.0,
            last_volume: 1.0,
            speed: 1.0,
            last_dir: None,
            menu: None,
            export_open: false,
            top_visible: true,
            top_hovered: false,
            transport_visible: true,
            transport_hovered: false,
            viewport_h: 800.0,
            activity: false,
            last_activity: 0.0,
            drop_hover: false,
            title_source: None,
        }
    }

    /// Raises one toast.
    fn notify(
        &mut self,
        title: impl Into<String>,
        detail: Option<String>,
        tone: Tone,
        persistent: bool,
    ) {
        self.toasts.push(title, detail, tone, persistent, self.now);
    }

    fn notify_error(&mut self, title: impl Into<String>, detail: impl Into<String>) {
        self.notify(title, Some(detail.into()), Tone::Danger, true);
    }

    fn dismiss_toast(&mut self, id: u64) {
        self.toasts.dismiss(id);
    }

    fn open(&mut self, path: PathBuf) {
        let Some(session) = &mut self.session else {
            self.notify_error(
                "Playback engine unavailable",
                "The playback engine is not available.",
            );
            return;
        };
        let parent = path.parent().map(|parent| parent.to_path_buf());
        let opened = session.open(path);
        match opened {
            Ok(()) => {
                self.last_dir = parent;
                self.toasts.clear();
                self.menu = None;
                self.top_visible = true;
                self.touch();
            }
            Err(error) => {
                self.notify_error("Couldn't open the video", error);
            }
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

    fn pointer_moved(&mut self, y: f32) {
        let in_top = y <= ui::TOP_REVEAL;
        let in_bottom = self.viewport_h > 0.0 && y >= self.viewport_h - ui::BOTTOM_REVEAL;
        self.top_visible = in_top;
        self.top_hovered = in_top;
        self.transport_visible = in_bottom;
        self.transport_hovered = in_bottom;
        self.activity = true;
    }

    fn pointer_left(&mut self) {
        self.top_visible = false;
        self.top_hovered = false;
        self.transport_visible = false;
        self.transport_hovered = false;
        self.activity = true;
    }

    fn set_viewport(&mut self, height: f32) {
        self.viewport_h = height;
    }

    fn is_seeking(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.is_seeking())
    }

    fn transport_forced(&self) -> bool {
        !self.playing() || self.menu.is_some() || self.is_seeking()
    }

    fn transport_shown(&self) -> bool {
        !self.export_open && (self.transport_visible || self.transport_forced())
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
        let error = self
            .session
            .as_mut()
            .and_then(|session| session.select_audio(id).err());
        if let Some(error) = error {
            self.notify("Audio track not selected", Some(error), Tone::Danger, false);
        }
        self.menu = None;
    }

    fn select_subtitle(&mut self, id: Option<i64>) {
        let error = self
            .session
            .as_mut()
            .and_then(|session| session.select_subtitle(id).err());
        if let Some(error) = error {
            self.notify(
                "Subtitle track not selected",
                Some(error),
                Tone::Danger,
                false,
            );
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
            session.start_export(output);
        }
    }

    fn cancel_export(&self) {
        if let Some(session) = &self.session {
            session.cancel_export();
        }
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
    app.now = t;
    let mut changed = false;
    let timeline_visible = app.transport_shown() || app.export_open;
    let notices = if let Some(session) = &mut app.session {
        changed |= session.poll(tx, timeline_visible);
        let source = session.source().map(|path| path.to_path_buf());
        if app.title_source != source {
            tx.set_title(format!("{} — Kachito", file_title(source.as_deref())));
            app.title_source = source;
            app.export_open = false;
            changed = true;
        }
        session.take_notices()
    } else {
        Vec::new()
    };
    for notice in notices {
        changed |= app.toasts.push(
            notice.title,
            notice.detail,
            notice.tone,
            notice.persistent,
            app.now,
        );
    }
    if app.activity {
        app.activity = false;
        app.last_activity = t;
        changed = true;
    }
    let transport_held = app.transport_forced() || app.export_open;
    if transport_held {
        if !app.transport_visible {
            app.transport_visible = true;
            changed = true;
        }
    } else if app.transport_visible && !app.transport_hovered && t - app.last_activity > HIDE_AFTER
    {
        app.transport_visible = false;
        changed = true;
    }
    if app.top_visible
        && !app.top_hovered
        && app.menu.is_none()
        && !app.export_open
        && t - app.last_activity > HIDE_AFTER
    {
        app.top_visible = false;
        changed = true;
    }
    if app.toasts.prune(t) {
        changed = true;
    }
    if let Some(due) = app.toasts.next_deadline() {
        tx.wake_at(due);
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
                request_fullscreen(app, tx);
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
        Err(error) => {
            app.notify_error("Couldn't open the file dialog", error.to_string());
        }
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
        Err(error) => {
            app.notify_error("Couldn't save the clip", error.to_string());
        }
    });
}

fn view(app: &App, _: &mut Cx<App>) -> El {
    let theme = ui::palette();
    let mut root = el()
        .key("root")
        .w_full()
        .h_full()
        .fill(theme.bg)
        .on_layout(|app: &mut App, [_, _, _, height]| app.set_viewport(height))
        .on_pointer_move(|app: &mut App, pointer| app.pointer_moved(pointer.y))
        .on_hover_change(|app: &mut App, inside: bool, _: &mut Tx| {
            if !inside {
                app.pointer_left();
            }
        })
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
            stage = stage.child(export_tray(session, &theme));
        } else if app.transport_shown() {
            stage = stage.child(transport(app, session, &theme));
        }
        if app.top_visible {
            stage = stage.child(top_floats(app, &theme));
        }
        root = root.child(stage);
    } else {
        root = root.child(empty_screen(app, &theme));
    }
    if let Some(menu) = menu_overlay(app, &theme) {
        root = root.child(menu);
    }
    if let Some(toasts) = toast_stack(app, &theme) {
        root = root.child(toasts);
    }
    root
}

fn file_title(source: Option<&std::path::Path>) -> String {
    source
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Kachito".into())
}

fn top_floats(app: &App, theme: &Palette) -> El {
    let title = file_title(app.session.as_ref().and_then(|session| session.source()));
    row()
        .key("top")
        .pin(Some(0.0), Some(0.0), Some(0.0), None)
        .w_full()
        .px(24.0)
        .pt(16.0)
        .gap(12.0)
        .center_y()
        .transition(ui::top_transition())
        .child(
            row()
                .key("title-capsule")
                .max_w(360.0)
                .h(ui::TOP_CAPSULE_H)
                .px(12.0)
                .gap(8.0)
                .center_y()
                .radius(ui::RADIUS_TOP)
                .fill(theme.surface)
                .border(ui::SURFACE_BORDER, theme.border)
                .shadow(ui::SURFACE_SHADOW_Y, ui::SURFACE_SHADOW_BLUR, theme.shadow)
                .child(
                    icon(&icons::FILM, 15.0)
                        .color(theme.text_muted)
                        .a11y_hidden(),
                )
                .child(
                    text(title)
                        .font(ui::BODY_STRONG)
                        .color(theme.text)
                        // The 360 pt capsule less its 24 pt padding, 15 pt icon and
                        // 8 pt gap.
                        .max_w(313.0)
                        .truncate(),
                ),
        )
        .child(el().grow(1.0))
        .child(
            ui::soft_button(Some(&icons::OPEN), "Open", theme)
                .key("open")
                .on_press_tx(|app: &mut App, tx: &mut Tx| open_dialog(app, tx)),
        )
}

fn audio_button(app: &App, theme: &Palette) -> El {
    if !app.has_source() {
        return ui::icon_button(&icons::AUDIO, "Audio track", theme)
            .key("audio-menu")
            .disabled(true)
            .dim(0.35);
    }
    let button = ui::icon_button(&icons::AUDIO, "Audio track", theme)
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
        return ui::icon_button(&icons::SUBTITLES, "Subtitles", theme)
            .key("subs-menu")
            .disabled(true)
            .dim(0.35);
    }
    let mut button = ui::icon_button(&icons::SUBTITLES, "Subtitles", theme)
        .key("subs-menu")
        .expanded(app.menu == Some(Menu::Subtitles))
        .on_press(|app: &mut App| app.toggle_menu(Menu::Subtitles));
    if app.subtitles_on() {
        button = button.child(
            el().pin(None, Some(6.0), Some(6.0), None)
                .size(5.0, 5.0)
                .radius(3.0)
                .fill(theme.accent),
        );
    }
    if app.has_subtitles() {
        button
    } else {
        button.dim(0.55)
    }
}

fn export_button(app: &App, theme: &Palette) -> El {
    let mut button = row()
        .key("export-toggle")
        .h(32.0)
        .px(8.0)
        .gap(6.0)
        .center_y()
        .radius(8.0)
        .hover(theme.hover)
        .press(theme.press)
        .press_scale(0.96)
        .focusable(true)
        .focus_ring(2.0, 2.0, theme.focus)
        .role("button")
        .label("Export a clip")
        .expanded(app.export_open)
        .checked(app.export_open);
    if app.export_open {
        button = button.fill(theme.hover);
    }
    button = button
        .child(
            icon(&icons::EXPORT, 15.0)
                .color(if app.export_open {
                    theme.text
                } else {
                    theme.text_muted
                })
                .a11y_hidden(),
        )
        .child(text("Export").font(ui::META).color(if app.export_open {
            theme.text
        } else {
            theme.text_muted
        }));
    if app.export_ready() {
        button.on_press(|app: &mut App| app.toggle_export())
    } else {
        button.disabled(true).dim(0.35)
    }
}

fn export_percent(fraction: f32) -> String {
    format!("{}%", (fraction * 100.0).round())
}

fn video_stage(session: &PlaybackSession, theme: &Palette) -> El {
    let surface = session.surface();
    el().key("stage")
        .w_full()
        .h_full()
        .fill(theme.stage)
        .center()
        .child(
            el().key("video")
                .aspect_fit(VIDEO_WIDTH as f32 / VIDEO_HEIGHT as f32)
                .native_surface(&surface, SurfaceFit::Fit)
                .on_double_press_tx(request_fullscreen),
        )
}

fn empty_screen(app: &App, theme: &Palette) -> El {
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
    el().key("empty")
        .w_full()
        .h_full()
        .fill(theme.stage)
        .center()
        .child(zone)
}

/// The toast stack sits in the bottom-right corner, above the transport and the export
/// tray when one is on screen.
fn toast_bottom_inset(app: &App) -> f32 {
    if !app.has_source() {
        TOAST_INSET
    } else if app.export_open {
        TOAST_EXPORT_CLEARANCE
    } else if app.transport_shown() {
        TOAST_TRANSPORT_CLEARANCE
    } else {
        TOAST_INSET
    }
}

fn toast_stack(app: &App, theme: &Palette) -> Option<El> {
    if app.toasts.is_empty() {
        return None;
    }
    let mut stack = col()
        .key("toast-stack")
        .pin(None, None, Some(TOAST_INSET), Some(toast_bottom_inset(app)))
        .items_end()
        .gap(8.0)
        .role("region")
        .label("Notifications");
    for toast in app.toasts.entries() {
        stack = stack.child(toast_card(toast, theme));
    }
    Some(stack)
}

fn toast_card(toast: &Toast, theme: &Palette) -> El {
    let (glyph, ink, fill, border) = match toast.tone {
        Tone::Danger => (
            &icons::ALERT,
            theme.danger,
            theme.danger_surface,
            theme.danger.alpha(0.4),
        ),
        Tone::Success => (
            &icons::CHECK,
            theme.accent,
            theme.surface_raised,
            theme.accent.alpha(0.45),
        ),
        Tone::Neutral => (
            &icons::FILM,
            theme.text_muted,
            theme.surface_raised,
            theme.border_strong,
        ),
    };
    let id = toast.id;
    let mut body = col()
        .grow(1.0)
        .gap(2.0)
        .live_region(LiveRegion::Assertive)
        .child(
            text(toast.title.clone())
                .font(ui::TOAST_TITLE)
                .color(theme.text)
                .wrap(),
        );
    if let Some(detail) = &toast.detail {
        body = body.child(
            text(detail.clone())
                .font(ui::TOAST_DETAIL)
                .color(theme.text_muted)
                .wrap()
                .max_lines(3),
        );
    }
    let mut card = row()
        .key(format!("toast-{id}"))
        .w(360.0)
        .padding(8.0)
        .gap(10.0)
        .center_y()
        .radius(10.0)
        .fill(fill)
        .border(1.0, border)
        .shadow(14.0, 30.0, theme.shadow)
        .transition(
            Transition::offset(0.0, 8.0)
                .combined(Transition::opacity())
                .springs(ui::TRAY_MOTION),
        )
        .child(
            el().size(20.0, 20.0)
                .center()
                .child(icon(glyph, 18.0).color(ink).a11y_hidden()),
        )
        .child(body);
    if toast.persistent {
        card = card.child(
            ui::large_icon_button(&icons::CLOSE, "Dismiss notification", theme)
                .key(format!("toast-close-{id}"))
                .on_press(move |app: &mut App| app.dismiss_toast(id)),
        );
    }
    card
}

fn transport(app: &App, session: &PlaybackSession, theme: &Palette) -> El {
    let duration = session.duration().unwrap_or(0.0);
    let position = session.timeline().position();
    let fraction = if duration > 0.0 {
        (position / duration).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };
    let capsule = col()
        .key("transport")
        .w_full()
        .max_w(ui::TRANSPORT_MAX_W)
        .h(ui::TRANSPORT_H)
        .px(14.0)
        .py(10.0)
        .gap(6.0)
        .radius(ui::RADIUS_TRANSPORT)
        .fill(theme.surface)
        .border(ui::SURFACE_BORDER, theme.border)
        .shadow(ui::SURFACE_SHADOW_Y, ui::SURFACE_SHADOW_BLUR, theme.shadow)
        .child(
            row()
                .w_full()
                .h(16.0)
                .gap(12.0)
                .center_y()
                .child(
                    row().key("time-current").w(44.0).center_y().child(
                        text(format_clock(position))
                            .font(ui::TIME_SMALL)
                            .color(theme.text),
                    ),
                )
                .child(seek_bar(app, fraction, duration, theme).grow(1.0))
                .child(
                    row()
                        .key("time-duration")
                        .w(44.0)
                        .justify_end()
                        .center_y()
                        .child(
                            text(format_clock(duration))
                                .font(ui::TIME_SMALL)
                                .color(theme.text_faint),
                        ),
                ),
        )
        .child(
            row()
                .w_full()
                .h(44.0)
                .gap(6.0)
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
                .child(el().grow(1.0))
                .child(volume_control(app, theme))
                .child(audio_button(app, theme))
                .child(subtitle_button(app, theme))
                .child(speed_button(app, theme))
                .child(fullscreen_button(theme))
                .child(el().w(1.0).h(16.0).mx(4.0).fill(theme.border))
                .child(export_button(app, theme)),
        );
    row()
        .key("transport-wrap")
        .pin(Some(0.0), None, Some(0.0), Some(ui::TRANSPORT_INSET))
        .w_full()
        .px(16.0)
        .justify_center()
        .transition(ui::bottom_transition())
        .child(capsule)
}

fn seek_bar(app: &App, fraction: f32, duration: f64, theme: &Palette) -> El {
    let seeking = app.is_seeking();
    el().key("seek")
        .w_full()
        .h(16.0)
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
                        .h(3.0)
                        .radius(1.5)
                        .fill(theme.accent),
                )
                .child(
                    el().key("seek-handle")
                        .w(10.0)
                        .h(10.0)
                        .radius(5.0)
                        .fill(if seeking {
                            theme.text.alpha(0.55)
                        } else {
                            theme.text
                        }),
                )
                .child(
                    el().grow((1.0 - fraction).max(0.001))
                        .h(3.0)
                        .radius(1.5)
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
        .child(icon(glyph, 20.0).color(if loaded { theme.text } else { theme.text_faint }));
    if loaded {
        button = button
            .fill(theme.play)
            .hover(theme.play_hover)
            .press(theme.play_press)
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
                .w(72.0)
                .h(16.0)
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
                        .h(10.0)
                        .center_y()
                        .child(
                            el().grow(app.volume.max(0.001))
                                .h(3.0)
                                .radius(1.5)
                                .fill(theme.accent),
                        )
                        .child(el().w(7.0).h(7.0).radius(3.5).fill(theme.text))
                        .child(
                            el().grow((1.0 - app.volume).max(0.001))
                                .h(3.0)
                                .radius(1.5)
                                .fill(theme.track),
                        ),
                ),
        )
}

fn speed_button(app: &App, theme: &Palette) -> El {
    let selected = app.menu == Some(Menu::Speed);
    row()
        .key("speed")
        .w(44.0)
        .h(32.0)
        .center()
        .radius(8.0)
        .hover(theme.hover)
        .press(theme.press)
        .press_scale(0.96)
        .focusable(true)
        .focus_ring(2.0, 2.0, theme.focus)
        .role("button")
        .label("Playback speed")
        .expanded(selected)
        .on_press(|app: &mut App| app.toggle_menu(Menu::Speed))
        .child(
            text(format_speed(app.speed))
                .font(ui::TIME_SMALL)
                .color(if selected {
                    theme.text
                } else {
                    theme.text_muted
                }),
        )
}

fn fullscreen_button(theme: &Palette) -> El {
    ui::icon_button(&icons::FULLSCREEN, "Fullscreen", theme)
        .key("fullscreen")
        .on_press_tx(request_fullscreen)
}

fn request_fullscreen(app: &mut App, tx: &mut Tx) {
    app.touch();
    tx.toggle_full_screen();
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
        Menu::Speed => menu_panel(
            "speed-menu-panel",
            "Speed",
            "speed",
            speed_rows(app, theme),
            280.0,
            theme,
        ),
    };
    Some(
        el().key("menu-scrim")
            .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
            .fill(theme.scrim)
            .child(panel.pin(
                None,
                None,
                Some(16.0),
                Some(ui::TRANSPORT_INSET + ui::TRANSPORT_H + 8.0),
            )),
    )
}

fn menu_panel(key: &str, title: &str, trigger: &str, body: El, width: f32, theme: &Palette) -> El {
    col()
        .key(key)
        .w(width)
        .padding(6.0)
        .gap(2.0)
        .radius(ui::RADIUS_LG)
        .fill(theme.surface)
        .border(ui::SURFACE_BORDER, theme.border)
        .shadow(ui::SURFACE_SHADOW_Y, ui::SURFACE_SHADOW_BLUR, theme.shadow)
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
        TracksState::Failed(_) => col()
            .w_full()
            .child(ui::menu_empty("Tracks unavailable", theme).key("track-unavailable")),
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
        Some(TracksState::Failed(_)) => {
            list = list.child(ui::menu_empty("Tracks unavailable", theme).key("track-unavailable"));
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

fn speed_rows(app: &App, theme: &Palette) -> El {
    el().w_full()
        .px(4.0)
        .child(row().w_full().gap(2.0).children(SPEEDS.iter().map(|speed| {
            let value = *speed;
            ui::segment(format_speed(value), (app.speed - value).abs() < 0.01, theme)
                .key(format!("speed-{value}"))
                .grow(1.0)
                .on_press(move |app: &mut App| {
                    app.set_speed(value);
                    app.menu = None;
                })
        })))
}

fn export_tray(session: &PlaybackSession, theme: &Palette) -> El {
    let timeline = session.timeline();
    let state = session.export_state();
    let mut footer = row().w_full().gap(8.0).center_y().child(el().grow(1.0));
    if let Some(ExportState::Running { fraction }) = &state {
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
                text(export_percent(*fraction))
                    .key("export-progress")
                    .font(ui::TIME)
                    .color(theme.text_muted),
            )
            .child(
                ui::ghost_button("Cancel", theme)
                    .key("cancel-export")
                    .on_press(|app: &mut App| app.cancel_export()),
            );
    } else {
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
        .padding(16.0)
        .gap(10.0)
        .radius(ui::RADIUS_EXPORT)
        .fill(theme.surface)
        .border(ui::SURFACE_BORDER, theme.border)
        .shadow(ui::SURFACE_SHADOW_Y, ui::SURFACE_SHADOW_BLUR, theme.shadow)
        .transition(ui::bottom_transition())
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
        .child(range_bar(timeline, theme))
        .child(footer);
    row()
        .key("export-wrap")
        .pin(Some(0.0), None, Some(0.0), Some(16.0))
        .w_full()
        .px(16.0)
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
    use crate::engine::{PlaybackSnapshot, PlayerEngine};
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
        pause_error: Option<String>,
        frame_start: bool,
        calls: Vec<String>,
        /// Direct engine state getters are charged as property reads, so a
        /// view that bypasses the session's snapshot is visible to tests.
        property_reads: usize,
        snapshot_tracks: Option<Result<Vec<Track>, String>>,
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
                pause_error: None,
                frame_start: false,
                calls: Vec::new(),
                property_reads: 0,
                snapshot_tracks: None,
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
            log.calls.push("open".into());
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
        fn snapshot(&self) -> PlaybackSnapshot {
            let log = self.log.lock().unwrap();
            PlaybackSnapshot {
                state: log.state.clone(),
                position: log.position,
                duration: Some(log.duration),
                frame_duration: Some(log.frame_duration),
                seeking: log.seeking,
                paused: matches!(log.state, PlaybackState::Paused),
                eof_reached: matches!(log.state, PlaybackState::Ended),
                path: log.opened.clone(),
                tracks: log.snapshot_tracks.clone(),
            }
        }
        fn is_seeking(&self) -> bool {
            let mut log = self.log.lock().unwrap();
            log.property_reads += 1;
            log.seeking
        }
        fn set_volume(&mut self, volume: f32) {
            self.log.lock().unwrap().volume = Some(volume);
        }
        fn set_speed(&mut self, speed: f32) {
            self.log.lock().unwrap().speed = Some(speed);
        }
        fn position(&self) -> f64 {
            let mut log = self.log.lock().unwrap();
            log.property_reads += 1;
            log.position
        }
        fn duration(&self) -> Option<f64> {
            let mut log = self.log.lock().unwrap();
            log.property_reads += 1;
            Some(log.duration)
        }
        fn state(&self) -> PlaybackState {
            let mut log = self.log.lock().unwrap();
            log.property_reads += 1;
            log.state.clone()
        }
        fn frame_duration(&self) -> Option<f64> {
            let mut log = self.log.lock().unwrap();
            log.property_reads += 1;
            Some(log.frame_duration)
        }
        fn current_path(&self) -> Option<PathBuf> {
            let mut log = self.log.lock().unwrap();
            log.property_reads += 1;
            log.opened.clone()
        }
        fn surface(&self) -> rune::NativeSurface {
            self.surface.clone()
        }
        fn tracks(&self) -> Result<Vec<Track>, String> {
            let mut log = self.log.lock().unwrap();
            log.tracks_reads += 1;
            log.property_reads += 1;
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
        fn set_paused(&mut self, paused: bool) -> Result<(), String> {
            let mut log = self.log.lock().unwrap();
            log.calls.push(format!("pause:{paused}"));
            if let Some(error) = log.pause_error.clone() {
                return Err(error);
            }
            Ok(())
        }
        fn take_frame_start(&mut self) -> bool {
            std::mem::take(&mut self.log.lock().unwrap().frame_start)
        }
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

    fn notices(session: &mut PlaybackSession) -> Vec<String> {
        session
            .take_notices()
            .into_iter()
            .map(|notice| match notice.detail {
                Some(detail) => format!("{}: {detail}", notice.title),
                None => notice.title,
            })
            .collect()
    }

    fn only_notice(session: &mut PlaybackSession) -> String {
        let mut notices = notices(session);
        assert_eq!(notices.len(), 1, "{notices:?}");
        notices.remove(0)
    }

    fn toast_texts(app: &App) -> Vec<String> {
        app.toasts
            .entries()
            .iter()
            .map(|toast| match &toast.detail {
                Some(detail) => format!("{}: {detail}", toast.title),
                None => toast.title.clone(),
            })
            .collect()
    }

    fn toast_cards(runtime: &Runtime<App>) -> Vec<String> {
        let mut cards: Vec<String> = runtime
            .elements()
            .into_iter()
            .filter_map(|element| element.key)
            .filter(|key| key.starts_with("toast-") && !key.starts_with("toast-close"))
            .filter(|key| key != "toast-stack")
            .collect();
        cards.sort();
        cards
    }

    fn toast_fill(runtime: &Runtime<App>, key: &str) -> Color {
        runtime
            .elements()
            .into_iter()
            .find(|element| element.key.as_deref() == Some(key))
            .unwrap_or_else(|| panic!("{key}"))
            .fill
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kachito-app-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn settle(runtime: &mut Runtime<App>) {
        let mut stable = 0;
        for _ in 0..600 {
            let commits = runtime.stats.commits;
            runtime.frame(1.0 / 60.0);
            if runtime.stats.commits == commits {
                stable += 1;
                if stable == 5 {
                    return;
                }
            } else {
                stable = 0;
            }
        }
        panic!("the view never settled");
    }

    #[test]
    fn a_hidden_transport_does_not_request_the_clock_wake() {
        let (mut app, log) = app_with_fake();
        {
            let mut log = log.lock().unwrap();
            log.state = PlaybackState::Playing;
            log.position = 2.0;
        }
        let session = session_mut(&mut app);
        let mut tx = Tx::default();
        session.poll(&mut tx, true);

        let mut tx = Tx::default();
        let changed = session.poll(&mut tx, false);
        assert!(!changed, "a hidden clock reported a change");
        assert!(!format!("{tx:?}").contains("100ms"), "{tx:?}");
    }

    #[test]
    fn a_visible_clock_keeps_the_clock_wake() {
        let (mut app, log) = app_with_fake();
        {
            let mut log = log.lock().unwrap();
            log.state = PlaybackState::Playing;
            log.position = 2.0;
        }
        let session = session_mut(&mut app);
        let mut tx = Tx::default();
        session.poll(&mut tx, true);

        log.lock().unwrap().position = 3.0;
        let mut tx = Tx::default();
        let changed = session.poll(&mut tx, true);
        assert!(changed, "the visible clock did not report the new position");
        assert!(format!("{tx:?}").contains("100ms"), "{tx:?}");
        assert_eq!(session.timeline().position(), 3.0);
    }

    #[test]
    fn a_hidden_transport_updates_a_late_duration_without_a_rebuild() {
        let (mut app, log) = app_with_fake();
        log.lock().unwrap().state = PlaybackState::Paused;
        let session = session_mut(&mut app);

        let mut tx = Tx::default();
        assert!(
            !session.poll(&mut tx, false),
            "the hidden transport rebuilt for the first duration"
        );
        assert_eq!(session.timeline().duration(), 10.0);

        log.lock().unwrap().duration = 12.0;
        log.lock().unwrap().frame_duration = 1.0 / 24.0;
        let mut tx = Tx::default();
        assert!(
            !session.poll(&mut tx, false),
            "the hidden transport rebuilt for a late duration"
        );
        assert_eq!(session.timeline().duration(), 12.0);
        assert!((session.timeline().frame_duration() - 1.0 / 24.0).abs() < 1e-9);
        assert!(!format!("{tx:?}").contains("100ms"), "{tx:?}");
    }

    #[test]
    fn an_open_export_tray_rebuilds_when_the_duration_refreshes() {
        let (mut app, log) = app_with_fake();
        log.lock().unwrap().state = PlaybackState::Paused;
        app.title_source = Some(PathBuf::from("test.mkv"));
        app.transport_visible = false;
        app.export_open = true;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        settle(&mut runtime);
        assert!(
            runtime.find("export-tray").is_some(),
            "the export tray is not open"
        );

        let commits = runtime.stats.commits;
        log.lock().unwrap().duration = 12.0;
        runtime.frame(1.0 / 60.0);
        assert!(
            runtime.stats.commits > commits,
            "the open export tray missed the new duration"
        );
    }

    #[test]
    fn a_hidden_seek_transition_does_not_rebuild_the_view() {
        let (mut app, log) = app_with_fake();
        let session = session_mut(&mut app);
        let mut tx = Tx::default();
        session.poll(&mut tx, false);

        log.lock().unwrap().seeking = true;
        let mut tx = Tx::default();
        assert!(
            !session.poll(&mut tx, false),
            "the hidden seek handle rebuilt the view"
        );
        assert!(session.is_seeking(), "the snapshot missed the seek");
    }

    #[test]
    fn a_running_export_with_a_closed_tray_does_not_rebuild_for_a_clock_refresh() {
        let (mut app, log) = app_with_fake();
        let session = session_mut(&mut app);
        session.set_export(Some(ExportJob::with_state(ExportState::Running {
            fraction: 0.5,
        })));
        let mut tx = Tx::default();
        session.poll(&mut tx, false);

        log.lock().unwrap().duration = 12.0;
        log.lock().unwrap().frame_duration = 1.0 / 24.0;
        let mut tx = Tx::default();
        assert!(
            !session.poll(&mut tx, false),
            "the running export rebuilt for a clock refresh"
        );
        assert_eq!(session.timeline().duration(), 12.0);
        assert!((session.timeline().frame_duration() - 1.0 / 24.0).abs() < 1e-9);
        assert!(
            format!("{tx:?}").contains("100ms"),
            "the running export stopped waking for progress: {tx:?}"
        );
    }

    #[test]
    fn an_idle_export_does_not_request_a_wake_or_rebuild() {
        let (mut app, _) = app_with_fake();
        let session = session_mut(&mut app);
        session.set_export(Some(ExportJob::with_state(ExportState::Finished)));

        let mut tx = Tx::default();
        assert!(session.poll(&mut tx, false), "the finished export is shown");
        let mut tx = Tx::default();
        assert!(
            !session.poll(&mut tx, false),
            "an idle export rebuilt the view"
        );
        assert!(!format!("{tx:?}").contains("100ms"), "{tx:?}");
    }

    #[test]
    fn a_hidden_transport_with_an_idle_export_does_not_rebuild() {
        let (mut app, log) = app_with_fake();
        let session = session_mut(&mut app);
        session.set_export(Some(ExportJob::with_state(ExportState::Finished)));
        let mut tx = Tx::default();
        session.poll(&mut tx, false);

        log.lock().unwrap().duration = 12.0;
        log.lock().unwrap().frame_duration = 1.0 / 24.0;
        let mut tx = Tx::default();
        assert!(
            !session.poll(&mut tx, false),
            "the hidden transport rebuilt with an idle export"
        );
        assert_eq!(session.timeline().duration(), 12.0);
        assert!(!format!("{tx:?}").contains("100ms"), "{tx:?}");
    }

    #[test]
    fn a_running_export_keeps_the_progress_wake() {
        let (mut app, _) = app_with_fake();
        let session = session_mut(&mut app);
        session.set_export(Some(ExportJob::with_state(ExportState::Running {
            fraction: 0.25,
        })));

        let mut tx = Tx::default();
        session.poll(&mut tx, false);
        let mut tx = Tx::default();
        let changed = session.poll(&mut tx, false);
        assert!(!changed, "unchanged export progress rebuilt the view");
        assert!(format!("{tx:?}").contains("100ms"), "{tx:?}");
    }

    #[test]
    fn a_hidden_transport_with_no_export_does_not_rebuild_the_view() {
        let (app, log) = app_with_fake();
        log.lock().unwrap().state = PlaybackState::Playing;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        runtime.frame(3.1);
        assert!(
            runtime.find("transport").is_none(),
            "the transport stayed visible"
        );

        let reads = log.lock().unwrap().property_reads;
        let commits = runtime.stats.commits;
        for _ in 0..10 {
            runtime.frame(0.1);
        }
        assert_eq!(log.lock().unwrap().property_reads, reads);
        assert_eq!(
            runtime.stats.commits, commits,
            "the hidden transport rebuilt the view"
        );
    }

    #[test]
    fn a_track_list_change_from_the_snapshot_updates_the_view() {
        let (mut app, log) = app_with_fake();
        let next_track = Track {
            id: 20,
            kind: TrackKind::Subtitle,
            title: Some("Signs".into()),
            language: None,
            selected: true,
        };
        log.lock().unwrap().snapshot_tracks = Some(Ok(vec![next_track.clone()]));

        let session = session_mut(&mut app);
        let mut tx = Tx::default();
        assert!(
            session.poll(&mut tx, true),
            "the new track list is reported"
        );
        assert_eq!(session.tracks(), &TracksState::Loaded(vec![next_track]));

        let mut tx = Tx::default();
        assert!(
            !session.poll(&mut tx, true),
            "an unchanged track list rebuilt the view"
        );
    }

    #[test]
    fn the_view_reads_no_properties_when_the_snapshot_is_still() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        runtime.frame(0.0);
        let reads = log.lock().unwrap().property_reads;
        let commits = runtime.stats.commits;

        for _ in 0..10 {
            runtime.frame(1.0 / 60.0);
        }

        assert_eq!(
            log.lock().unwrap().property_reads,
            reads,
            "the view read an engine property with an unchanged snapshot"
        );
        assert_eq!(
            runtime.stats.commits, commits,
            "the view rebuilt with an unchanged snapshot"
        );
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
        session.poll(&mut tx, true);
        assert!(session.is_scrubbing());
        assert!(format!("{tx:?}").contains("16ms"), "{tx:?}");

        clock.advance(Duration::from_millis(4_999));
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert!(
            session.is_scrubbing(),
            "the seek cleared before the deadline"
        );
        assert!(format!("{tx:?}").contains("16ms"), "{tx:?}");

        clock.advance(Duration::from_millis(1));
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert!(
            !session.is_scrubbing(),
            "the stuck seek stayed pending past the deadline"
        );
        assert_eq!(session.timeline().position(), 2.0);
        assert!(!format!("{tx:?}").contains("16ms"), "{tx:?}");
        let notice = only_notice(session);
        assert!(notice.contains("Seek took too long"), "{notice}");

        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert!(notices(session).is_empty(), "the notice was raised twice");
    }

    #[test]
    fn a_stuck_preview_clears_at_five_seconds_and_restores_the_engine_position() {
        let (mut app, log, clock) = app_with_fake_and_clock();
        let session = session_mut(&mut app);
        session.timeline_mut().set_duration(10.0, 1.0 / 60.0);
        session.timeline_mut().set_position(5.0);

        session.seek_preview(5.0);
        assert_eq!(log.lock().unwrap().preview_seeks, 1);
        {
            let mut log = log.lock().unwrap();
            log.seeking = true;
            log.position = 2.0;
        }
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert!(session.is_scrubbing());
        assert!(format!("{tx:?}").contains("16ms"), "{tx:?}");

        clock.advance(Duration::from_millis(4_999));
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert!(
            session.is_scrubbing(),
            "the preview cleared before the deadline"
        );
        assert!(format!("{tx:?}").contains("16ms"), "{tx:?}");

        clock.advance(Duration::from_millis(1));
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert!(
            !session.is_scrubbing(),
            "the stuck preview stayed pending past the deadline"
        );
        assert_eq!(session.timeline().position(), 2.0);
        assert!(!format!("{tx:?}").contains("16ms"), "{tx:?}");
        let notice = only_notice(session);
        assert!(notice.contains("Seek took too long"), "{notice}");

        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert!(notices(session).is_empty(), "the notice was raised twice");
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
        session.poll(&mut tx, true);
        assert!(session.is_scrubbing());
        assert!(
            format!("{tx:?}").contains("5s"),
            "the deadline has no wake: {tx:?}"
        );

        clock.advance(Duration::from_secs(5));
        let mut tx = Tx::default();
        session.poll(&mut tx, true);

        assert!(
            !session.is_scrubbing(),
            "the failed engine kept the seek pending"
        );
        assert_eq!(session.timeline().position(), 2.0);
        let notices = notices(session);
        assert!(
            notices
                .iter()
                .any(|notice| notice.contains("Seek took too long")),
            "{notices:?}"
        );
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
        session.poll(&mut tx, true);
        assert!(
            session.is_scrubbing(),
            "the empty engine cleared the seek before the deadline"
        );

        clock.advance(Duration::from_millis(1));
        session.poll(&mut tx, true);
        assert!(!session.is_scrubbing());
        assert_eq!(session.timeline().position(), 0.0);
        let notice = only_notice(session);
        assert!(notice.contains("Seek took too long"), "{notice}");
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
        session.poll(&mut tx, true);
        let raised = notices(session);
        assert!(
            raised
                .iter()
                .any(|notice| notice.contains("Seek took too long")),
            "{raised:?}"
        );

        log.lock().unwrap().command_failure = Some("set speed 2: rejected".into());
        session.poll(&mut tx, true);
        let raised = notices(session);
        assert!(
            raised.iter().any(|notice| notice.contains("set speed 2")),
            "the deadline consumed the command notice: {raised:?}"
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
        session.poll(&mut tx, true);
        session.poll(&mut tx, true);

        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("notice: seek deadline").count(), 1, "{text}");
        assert_eq!(sink.lines(), text.lines().collect::<Vec<_>>());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_optimistic_selection_is_not_reported_again() {
        let (mut app, log) = app_with_fake();
        let initial = match session(&app).tracks() {
            TracksState::Loaded(tracks) => tracks.clone(),
            other => panic!("{other:?}"),
        };
        log.lock().unwrap().snapshot_tracks = Some(Ok(initial));

        let session = session_mut(&mut app);
        let mut tx = Tx::default();
        assert!(
            session.poll(&mut tx, true),
            "the first track list is reported"
        );
        session.select_audio(2).unwrap();
        let selected = match session.tracks() {
            TracksState::Loaded(tracks) => tracks.clone(),
            other => panic!("{other:?}"),
        };
        log.lock().unwrap().snapshot_tracks = Some(Ok(selected));

        let mut tx = Tx::default();
        assert!(
            !session.poll(&mut tx, true),
            "the optimistic selection was reported a second time"
        );
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
        assert!(app.toasts.is_empty());
        assert!(session(&app).is_awaiting_source());
        assert_eq!(
            log.lock().unwrap().opened.as_deref(),
            Some(source.as_path())
        );

        session_mut(&mut app).refresh_snapshot();
        let state = session(&app).state();
        assert!(session_mut(&mut app).confirm_open(&state));
        assert_eq!(session(&app).source(), Some(source.as_path()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_opened_before_the_first_frame_pauses_before_load() {
        let dir = temp_dir("startup-pause-order");
        let source = dir.join("start.mkv");
        std::fs::write(&source, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        session_mut(&mut app).open(source).unwrap();
        assert_eq!(
            log.lock().unwrap().calls,
            vec!["pause:true".to_string(), "open".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_startup_pause_clears_on_the_first_frame() {
        let dir = temp_dir("startup-pause-clear");
        let source = dir.join("start.mkv");
        std::fs::write(&source, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        let session = session_mut(&mut app);
        session.open(source).unwrap();
        log.lock().unwrap().frame_start = true;
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert_eq!(
            log.lock().unwrap().calls,
            vec![
                "pause:true".to_string(),
                "open".to_string(),
                "pause:false".to_string()
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_user_pause_is_not_cleared_by_the_first_frame() {
        let dir = temp_dir("startup-pause-user");
        let source = dir.join("start.mkv");
        std::fs::write(&source, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        let session = session_mut(&mut app);
        session.open(source).unwrap();
        log.lock().unwrap().state = PlaybackState::Playing;
        session.toggle();
        assert_eq!(log.lock().unwrap().state, PlaybackState::Paused);

        log.lock().unwrap().frame_start = true;
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert_eq!(
            log.lock().unwrap().state,
            PlaybackState::Paused,
            "the first frame resumed a user pause"
        );
        assert!(
            !log.lock()
                .unwrap()
                .calls
                .contains(&"pause:false".to_string()),
            "the automatic clear ran after a user took the pause"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_later_open_keeps_a_user_pause() {
        let dir = temp_dir("startup-pause-later");
        let first = dir.join("first.mkv");
        let second = dir.join("second.mkv");
        std::fs::write(&first, b"not a video").unwrap();
        std::fs::write(&second, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        let session = session_mut(&mut app);
        session.open(first).unwrap();
        log.lock().unwrap().frame_start = true;
        let mut tx = Tx::default();
        session.poll(&mut tx, true);

        log.lock().unwrap().state = PlaybackState::Playing;
        session.toggle();
        assert_eq!(log.lock().unwrap().state, PlaybackState::Paused);

        session.open(second).unwrap();
        log.lock().unwrap().frame_start = true;
        session.poll(&mut tx, true);
        assert_eq!(
            log.lock().unwrap().calls,
            vec![
                "pause:true".to_string(),
                "open".to_string(),
                "pause:false".to_string(),
                "open".to_string()
            ],
            "a later open paused the engine or resumed the user's pause"
        );
        assert_eq!(log.lock().unwrap().state, PlaybackState::Paused);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_user_pause_before_the_first_frame_survives_a_later_open() {
        let dir = temp_dir("startup-pause-before-frame");
        let first = dir.join("first.mkv");
        let second = dir.join("second.mkv");
        std::fs::write(&first, b"not a video").unwrap();
        std::fs::write(&second, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        let session = session_mut(&mut app);
        session.open(first).unwrap();

        log.lock().unwrap().state = PlaybackState::Playing;
        session.toggle();
        assert_eq!(log.lock().unwrap().state, PlaybackState::Paused);

        session.open(second).unwrap();
        log.lock().unwrap().frame_start = true;
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert_eq!(
            log.lock().unwrap().calls,
            vec![
                "pause:true".to_string(),
                "open".to_string(),
                "open".to_string()
            ],
            "a later open re-armed the startup pause or resumed the user's pause"
        );
        assert_eq!(log.lock().unwrap().state, PlaybackState::Paused);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_toggle_on_the_empty_screen_does_not_end_the_startup_phase() {
        let dir = temp_dir("startup-toggle-empty");
        let source = dir.join("start.mkv");
        std::fs::write(&source, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        let session = session_mut(&mut app);
        log.lock().unwrap().state = PlaybackState::Empty;

        session.toggle();
        session.open(source).unwrap();
        assert_eq!(
            log.lock().unwrap().calls,
            vec!["pause:true".to_string(), "open".to_string()],
            "a toggle on the empty screen ended the startup phase"
        );

        log.lock().unwrap().frame_start = true;
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert_eq!(
            log.lock().unwrap().calls,
            vec![
                "pause:true".to_string(),
                "open".to_string(),
                "pause:false".to_string()
            ],
            "the first frame did not clear the startup pause"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_toggle_while_loading_keeps_the_startup_pause() {
        let dir = temp_dir("startup-toggle-loading");
        let source = dir.join("start.mkv");
        std::fs::write(&source, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        let session = session_mut(&mut app);
        log.lock().unwrap().state = PlaybackState::Loading;

        session.open(source).unwrap();
        session.toggle();
        assert_eq!(
            log.lock().unwrap().calls,
            vec!["pause:true".to_string(), "open".to_string()],
            "a toggle while the file was loading ended the startup phase"
        );

        {
            let mut log = log.lock().unwrap();
            log.state = PlaybackState::Playing;
            log.frame_start = true;
        }
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        assert_eq!(
            log.lock().unwrap().calls,
            vec![
                "pause:true".to_string(),
                "open".to_string(),
                "pause:false".to_string()
            ],
            "the first frame did not resume the automatic pause"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_rejected_startup_pause_and_a_failed_load_are_logged() {
        let (log, sink, path) = crate::log::testing::log_to_file("session-startup");
        let fake = Arc::new(Mutex::new(FakeLog::default()));
        let engine = Fake {
            surface: rune::NativeSurface::new(),
            log: fake.clone(),
        };
        let mut session = PlaybackSession::new(Box::new(engine), log);
        let dir = temp_dir("startup-log");
        let source = dir.join("start.mkv");
        std::fs::write(&source, b"not a video").unwrap();

        fake.lock().unwrap().pause_error = Some("pause is unavailable".into());
        session.open(source.clone()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("warn: startup pause was not applied: pause is unavailable"),
            "{text}"
        );

        fake.lock().unwrap().pause_error = None;
        fake.lock().unwrap().open_error = Some("loadfile rejected".into());
        let error = session.open(source).unwrap_err();
        assert!(error.contains("loadfile rejected"), "{error}");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("failure: load failed: loadfile rejected"),
            "{text}"
        );
        assert_eq!(sink.lines(), text.lines().collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_failed_automatic_resume_keeps_the_pause_and_retries() {
        let (log, sink, path) = crate::log::testing::log_to_file("session-resume");
        let fake = Arc::new(Mutex::new(FakeLog::default()));
        let engine = Fake {
            surface: rune::NativeSurface::new(),
            log: fake.clone(),
        };
        let mut session = PlaybackSession::new(Box::new(engine), log);
        let dir = temp_dir("startup-resume");
        let source = dir.join("start.mkv");
        std::fs::write(&source, b"not a video").unwrap();

        session.open(source).unwrap();
        {
            let mut fake = fake.lock().unwrap();
            fake.frame_start = true;
            fake.pause_error = Some("pause is unavailable".into());
        }
        let mut tx = Tx::default();
        session.poll(&mut tx, true);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("warn: automatic pause was not cleared: pause is unavailable"),
            "{text}"
        );

        fake.lock().unwrap().pause_error = None;
        session.poll(&mut tx, true);
        fake.lock().unwrap().frame_start = true;
        session.poll(&mut tx, true);
        assert_eq!(
            fake.lock().unwrap().calls,
            vec![
                "pause:true".to_string(),
                "open".to_string(),
                "pause:false".to_string(),
                "pause:false".to_string(),
            ],
            "the failed resume was not retried, or the cleared pause was written again"
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(sink.lines(), text.lines().collect::<Vec<_>>());
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_rejected_command_raises_a_toast() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        assert!(!shows_text(&runtime, "The player could not apply"));

        log.lock().unwrap().command_failure = Some("set volume 40: property unavailable".into());
        runtime.frame(1.0 / 60.0);
        assert!(
            shows_text(&runtime, "set volume 40"),
            "the first rejection raises the toast"
        );

        log.lock().unwrap().command_failure =
            Some("seek 5.000 absolute+exact: invalid parameter".into());
        runtime.frame(1.0 / 60.0);
        assert!(shows_text(&runtime, "set volume 40"));
        assert!(
            shows_text(&runtime, "seek 5.000"),
            "the second rejection is its own toast"
        );
    }

    #[test]
    fn notices_from_different_sources_stack_and_the_cap_drops_the_oldest() {
        let (mut app, log) = app_with_fake();
        app.notify_error("Export failed", "The export failed.");
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        assert!(shows_text(&runtime, "Export failed"));
        assert!(shows_text(&runtime, "The export failed."));

        log.lock().unwrap().command_failure = Some("set volume 40: rejected".into());
        runtime.frame(1.0 / 60.0);
        assert!(shows_text(&runtime, "Command not applied"));
        assert!(shows_text(&runtime, "set volume 40"));
        assert!(
            shows_text(&runtime, "The export failed."),
            "a distinct notice stacks beside the older toast"
        );

        log.lock().unwrap().state = PlaybackState::Failed("the engine failed".into());
        runtime.frame(1.0 / 60.0);
        assert!(shows_text(&runtime, "Playback failed"));
        assert!(shows_text(&runtime, "The engine failed."));
        assert!(shows_text(&runtime, "The export failed."));
        assert!(shows_text(&runtime, "set volume 40"));

        log.lock().unwrap().state = PlaybackState::Paused;
        session_mut(&mut runtime.state).set_export_error("the newer export failed".into());
        runtime.frame(1.0 / 60.0);
        runtime.frame(0.3);
        assert!(shows_text(&runtime, "The newer export failed."));
        assert!(
            !shows_text(&runtime, "The export failed."),
            "the oldest toast left"
        );
        assert!(shows_text(&runtime, "The engine failed."));
        assert!(shows_text(&runtime, "set volume 40"));
    }

    #[test]
    fn a_failed_load_and_a_rejected_command_each_raise_a_toast() {
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
        assert!(shows_text(&runtime, "Loadfile rejected."));
        assert!(!shows_text(&runtime, "The player could not apply"));

        log.lock().unwrap().command_failure = Some("set volume 40: rejected".into());
        runtime.frame(1.0 / 60.0);
        assert!(
            shows_text(&runtime, "set volume 40"),
            "the later rejection gets its own toast"
        );
        assert!(shows_text(&runtime, "Loadfile rejected."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_successful_open_clears_the_toasts() {
        let dir = temp_dir("clear-command-notice");
        let next = dir.join("next.mkv");
        std::fs::write(&next, b"not a video").unwrap();
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        log.lock().unwrap().command_failure = Some("set pause yes: rejected".into());
        runtime.frame(1.0 / 60.0);
        assert!(runtime.find("toast-stack").is_some());

        runtime.state.open(next);
        runtime.frame(1.0 / 60.0);
        assert!(runtime.state.toasts.is_empty());
        assert!(
            runtime.find("toast-stack").is_none(),
            "the successful open clears the stack"
        );

        log.lock().unwrap().command_failure = Some("seek 1.000 absolute+exact: rejected".into());
        runtime.frame(1.0 / 60.0);
        assert!(
            runtime.find("toast-stack").is_some(),
            "the next file gets its own toast"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_seek_deadline_toast_dismisses_itself() {
        let (mut app, log, clock) = app_with_fake_and_clock();
        session_mut(&mut app)
            .timeline_mut()
            .set_duration(10.0, 1.0 / 60.0);
        session_mut(&mut app).timeline_mut().set_position(5.0);
        {
            let mut log = log.lock().unwrap();
            log.seeking = true;
            log.position = 2.0;
        }
        session_mut(&mut app).seek(5.0);

        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);

        clock.advance(Duration::from_secs(5));
        runtime.frame(0.1);
        assert!(shows_text(&runtime, "Seek took too long"));

        runtime.frame(crate::toast::TRANSIENT_SECONDS);
        runtime.frame(0.3);
        assert!(
            !shows_text(&runtime, "Seek took too long"),
            "the transient toast outlived its timer"
        );
    }

    #[test]
    fn a_persistent_toast_stays_past_the_timeout_until_closed() {
        let (mut app, _) = app_with_fake();
        app.notify_error("Playback failed", "The engine failed.");
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        runtime.frame(crate::toast::TRANSIENT_SECONDS + 1.0);
        assert!(shows_text(&runtime, "The engine failed."));
        let close = rect(&runtime, "toast-close-1");
        assert!(
            close[2] >= 40.0 && close[3] >= 40.0,
            "the close control is under the desktop hit floor: {close:?}"
        );

        click(&mut runtime, "toast-close-1");
        runtime.frame(0.3);
        assert!(!shows_text(&runtime, "The engine failed."));
    }

    #[test]
    fn a_toast_renders_without_a_session() {
        let mut app = App::with_session(None);
        app.notify_error(
            "Playback engine unavailable",
            "The playback engine is not available.",
        );
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.5);
        assert!(runtime.find("empty").is_some());
        assert!(shows_text(
            &runtime,
            "The playback engine is not available."
        ));
        assert!(runtime.find("toast-close-1").is_some());
    }

    #[test]
    fn an_identical_toast_refreshes_instead_of_stacking() {
        let (mut app, _) = app_with_fake();
        app.notify_error("Export failed", "The export failed.");
        app.notify_error("Export failed", "The export failed.");
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.5);
        assert_eq!(toast_cards(&runtime), vec!["toast-1".to_string()]);
    }

    #[test]
    fn the_toast_stack_sits_above_the_transport() {
        let (mut app, _) = app_with_fake();
        app.notify_error("Export failed", "The export failed.");
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.5);
        let toast = rect(&runtime, "toast-1");
        let transport = rect(&runtime, "transport");
        assert!(
            toast[1] + toast[3] <= transport[1] - 8.0,
            "the toast overlaps the transport: {toast:?} {transport:?}"
        );
    }

    #[test]
    fn the_toast_stack_sits_above_the_export_tray() {
        let (mut app, _) = app_with_fake();
        app.notify_error("Export failed", "The export failed.");
        app.export_open = true;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.5);
        let toast = rect(&runtime, "toast-1");
        let tray = rect(&runtime, "export-tray");
        assert!(
            toast[1] + toast[3] <= tray[1] - 8.0,
            "the toast overlaps the export tray: {toast:?} {tray:?}"
        );
    }

    #[test]
    fn a_failed_export_toasts_its_reason_and_leaves_the_tray_clean() {
        let (mut app, _) = app_with_fake();
        session_mut(&mut app).set_export(Some(ExportJob::with_state(ExportState::Failed(
            "the export process exited".into(),
        ))));
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        assert!(shows_text(&runtime, "The export process exited."));
        assert!(runtime.find("toast-close-1").is_some());
        assert_eq!(
            toast_fill(&runtime, "toast-1"),
            ui::palette().danger_surface,
            "an export failure is not the danger surface"
        );

        click(&mut runtime, "export-toggle");
        runtime.frame(0.2);
        assert!(runtime.find("export-tray").is_some());
        assert!(runtime.find("export-tray-status").is_none());
    }

    #[test]
    fn a_finished_export_toasts_and_carries_no_close() {
        let (mut app, _) = app_with_fake();
        session_mut(&mut app).set_export(Some(ExportJob::with_state(ExportState::Finished)));
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        assert!(shows_text(&runtime, "Clip exported"));
        assert!(runtime.find("toast-close-1").is_none());
        assert_eq!(
            toast_fill(&runtime, "toast-1"),
            ui::palette().surface_raised,
            "a finished export used the error surface"
        );
    }

    #[test]
    fn a_cancelled_export_toasts() {
        let (mut app, _) = app_with_fake();
        session_mut(&mut app).set_export(Some(ExportJob::with_state(ExportState::Cancelled)));
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        assert!(shows_text(&runtime, "Export cancelled"));
        assert_eq!(
            toast_fill(&runtime, "toast-1"),
            ui::palette().surface_raised,
            "a cancelled export used the error surface"
        );
    }

    #[test]
    fn a_track_list_failure_raises_a_toast_and_a_neutral_menu() {
        let (mut app, log) = app_with_fake();
        session_mut(&mut app).set_tracks(TracksState::Unloaded);
        log.lock().unwrap().tracks_result = Some(Err("track list unavailable".into()));
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        assert!(shows_text(&runtime, "Tracks unavailable"));
        assert!(shows_text(&runtime, "Track list unavailable."));
        click(&mut runtime, "audio-menu");
        runtime.frame(0.2);
        assert!(runtime.find("track-unavailable").is_some());
        assert!(runtime.find("track-error").is_none());
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
        fake.lock().unwrap().command_failure = Some("seek 2.000 absolute+exact: invalid".into());
        runtime.frame(1.0 / 60.0);

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("notice: rejected command: set speed 0: invalid parameter"),
            "{text}"
        );
        assert_eq!(
            text.matches("notice: rejected command").count(),
            1,
            "the notice line was logged more than once: {text}"
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
        session_mut(&mut runtime.state).refresh_snapshot();
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
        assert!(
            toast_texts(&app)
                .iter()
                .any(|text| text.contains("is not a supported video file")),
            "{:?}",
            toast_texts(&app)
        );
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
        assert_eq!(
            toast_texts(&app),
            vec!["Couldn't open the video: Remote URLs are not supported yet."]
        );
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
        assert!(
            toast_texts(&app)
                .iter()
                .any(|text| text.contains("is not a readable video file")),
            "{:?}",
            toast_texts(&app)
        );
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
        session_mut(&mut app).refresh_snapshot();
        let state = session(&app).state();
        assert!(session_mut(&mut app).confirm_open(&state));
        assert_eq!(session(&app).source(), old_path.as_deref());
        assert_eq!(session(&app).tracks(), &old_tracks);
        assert_eq!(session(&app).timeline(), &old_timeline);
        assert!(!session(&app).is_awaiting_source());
        assert!(only_notice(session_mut(&mut app)).contains("Corrupt"));
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

        session_mut(&mut app).refresh_snapshot();
        let state = session(&app).state();
        assert!(!session_mut(&mut app).confirm_open(&state));
        assert_eq!(session(&app).source(), old_path.as_deref());

        log.lock().unwrap().state = PlaybackState::Paused;
        session_mut(&mut app).refresh_snapshot();
        let state = session(&app).state();
        assert!(session_mut(&mut app).confirm_open(&state));
        assert_eq!(session(&app).source(), Some(next.as_path()));
        assert_eq!(session(&app).tracks(), &TracksState::Unloaded);
        assert_eq!(session(&app).timeline(), &Timeline::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_successful_source_swap_clears_the_old_export_and_toasts() {
        let dir = temp_dir("swap");
        let next = dir.join("next.mkv");
        std::fs::write(&next, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        session_mut(&mut app).set_export(Some(ExportJob::with_state(ExportState::Finished)));
        app.notify_error("Export failed", "The old export error.");
        log.lock().unwrap().state = PlaybackState::Loading;

        app.open(next);
        log.lock().unwrap().state = PlaybackState::Paused;
        session_mut(&mut app).refresh_snapshot();
        let state = session(&app).state();
        assert!(session_mut(&mut app).confirm_open(&state));
        assert_eq!(session(&app).export_state(), None);
        assert!(app.toasts.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_load_raises_its_own_notice_after_the_open_cleared_the_stack() {
        let dir = temp_dir("stale-export-error");
        let corrupt = dir.join("corrupt.mkv");
        std::fs::write(&corrupt, b"not a video").unwrap();
        let (mut app, log) = app_with_fake();
        app.notify_error("Export failed", "The old export error.");
        log.lock().unwrap().state = PlaybackState::Loading;

        app.open(corrupt);
        assert!(app.toasts.is_empty(), "the accepted open cleared the stack");
        log.lock().unwrap().state = PlaybackState::Failed("corrupt".into());
        session_mut(&mut app).refresh_snapshot();
        let state = session(&app).state();
        assert!(session_mut(&mut app).confirm_open(&state));
        assert!(only_notice(session_mut(&mut app)).contains("Corrupt"));
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
        assert!(shows_text(&runtime, "Corrupt."));
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
        session_mut(&mut app).refresh_snapshot();
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
        session_mut(&mut app).refresh_snapshot();
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
                "track-unavailable",
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
                "track-unavailable",
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
            let running = matches!(state, ExportState::Running { .. });
            assert!(
                runtime.find("export-toggle").is_some(),
                "the transport lost its Export control: {state:?}"
            );
            click(&mut runtime, "export-toggle");
            runtime.frame(0.2);
            assert!(runtime.find("export-tray").is_some(), "{state:?}");
            if running {
                assert!(runtime.find("export-progress").is_some(), "{state:?}");
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
            assert!(video[1] >= 0.0, "{size:?} {video:?}");
            assert!(video[0] + video[2] <= size.0 + 0.1, "{size:?} {video:?}");
            assert!(video[1] + video[3] <= size.1 + 0.1, "{size:?} {video:?}");
            assert!(
                (video[2] / video[3] - 16.0 / 9.0).abs() < 0.01,
                "{size:?} {video:?}"
            );
            let centre_x = video[0] + video[2] / 2.0;
            let centre_y = video[1] + video[3] / 2.0;
            assert!((centre_x - size.0 / 2.0).abs() < 0.5, "{size:?}");
            assert!((centre_y - size.1 / 2.0).abs() < 0.5, "{size:?} {centre_y}");
            assert!((transport[2] - 720.0).abs() < 0.5, "{size:?} {transport:?}");
            assert!(
                (transport[3] - ui::TRANSPORT_H).abs() < 0.5,
                "{size:?} {transport:?}"
            );
            assert!(
                (size.1 - (transport[1] + transport[3]) - ui::TRANSPORT_INSET).abs() < 0.5,
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
    fn the_float_capsules_keep_their_edges_and_speed_slot() {
        let (app, _) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);

        let title = rect(&runtime, "title-capsule");
        let open = rect(&runtime, "open");
        assert!((title[0] - 24.0).abs() < 0.5, "{title:?}");
        assert!((title[1] - 16.0).abs() < 0.5, "{title:?}");
        assert!(title[2] > 0.0 && title[2] < 360.0, "{title:?}");
        assert!((title[3] - ui::TOP_CAPSULE_H).abs() < 0.5, "{title:?}");
        assert!((open[1] - 16.0).abs() < 0.5, "{open:?}");
        assert!((open[3] - ui::TOP_CAPSULE_H).abs() < 0.5, "{open:?}");
        assert!(
            (open[0] + open[2] - (1280.0 - 24.0)).abs() < 0.5,
            "{open:?}"
        );

        let transport = rect(&runtime, "transport");
        for key in [
            "play",
            "back-10",
            "forward-10",
            "mute",
            "volume",
            "audio-menu",
            "subs-menu",
            "speed",
            "fullscreen",
            "export-toggle",
        ] {
            let control = rect(&runtime, key);
            assert!(
                control[0] >= transport[0]
                    && control[0] + control[2] <= transport[0] + transport[2],
                "{key} left the transport: {control:?} {transport:?}"
            );
        }
        let current = rect(&runtime, "time-current");
        let duration = rect(&runtime, "time-duration");
        let play = rect(&runtime, "play");
        let export = rect(&runtime, "export-toggle");
        assert!((current[0] - play[0]).abs() < 0.5, "{current:?} {play:?}");
        assert!((current[2] - 44.0).abs() < 0.5, "{current:?}");
        assert!((duration[2] - 44.0).abs() < 0.5, "{duration:?}");
        assert!(
            (duration[0] + duration[2] - (export[0] + export[2])).abs() < 0.5,
            "{duration:?} {export:?}"
        );
        assert!(
            (transport[0] + 14.0 - current[0]).abs() < 0.5,
            "{transport:?} {current:?}"
        );

        let speed = rect(&runtime, "speed");
        let fullscreen = rect(&runtime, "fullscreen");
        assert!((speed[2] - 44.0).abs() < 0.5, "{speed:?}");
        click(&mut runtime, "speed");
        click(&mut runtime, "speed-1.25");
        runtime.frame(0.1);
        let speed_after = rect(&runtime, "speed");
        assert!((speed_after[2] - 44.0).abs() < 0.5, "{speed_after:?}");
        assert!(
            (speed_after[0] - speed[0]).abs() < 0.5,
            "{speed:?} {speed_after:?}"
        );
        assert!(
            (rect(&runtime, "fullscreen")[0] - fullscreen[0]).abs() < 0.5,
            "fullscreen moved after the speed change"
        );
        assert!(
            (rect(&runtime, "export-toggle")[0] - export[0]).abs() < 0.5,
            "Export moved after the speed change"
        );
    }

    #[test]
    fn a_long_title_caps_the_capsule_at_360() {
        let dir = temp_dir("title");
        let source = dir.join("The Long Take (Director's Cut) [1080p] [HDR].mkv");
        std::fs::write(&source, b"not decoded by the fake").unwrap();

        let (mut app, _) = app_with_fake();
        app.open(source);
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        let title = rect(&runtime, "title-capsule");
        assert!((title[2] - 360.0).abs() < 0.5, "{title:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_speed_menu_changes_speed_and_fullscreen_is_direct() {
        let (app, log) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        click(&mut runtime, "speed");
        assert!(runtime.find("speed-menu-panel").is_some());
        click(&mut runtime, "speed-1.5");
        assert_eq!(log.lock().unwrap().speed, Some(1.5));
        assert!(runtime.find("speed-menu-panel").is_none());

        assert!(runtime.find("fullscreen").is_some());
        click(&mut runtime, "fullscreen");
        runtime.frame(0.1);
        assert!(runtime.find("speed-menu-panel").is_none());
    }

    #[test]
    fn the_chrome_follows_the_pointer_edges_while_playing() {
        let (app, log) = app_with_fake();
        log.lock().unwrap().state = PlaybackState::Playing;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.frame(0.0);
        assert!(runtime.find("play").is_some());
        assert!(runtime.find("open").is_some());

        runtime.frame(3.1);
        assert!(runtime.find("play").is_none());
        assert!(runtime.find("open").is_none());

        runtime.pointer_move(640.0, 760.0);
        runtime.frame(0.1);
        assert!(runtime.find("play").is_some());
        assert!(runtime.find("open").is_none());

        runtime.pointer_move(640.0, 20.0);
        runtime.frame(0.1);
        assert!(runtime.find("open").is_some());
        assert!(runtime.find("play").is_none());
    }

    #[test]
    fn keyboard_use_reveals_the_transport() {
        let (app, log) = app_with_fake();
        log.lock().unwrap().state = PlaybackState::Playing;
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.poll = Some(poll);
        runtime.on_key = Some(on_key);
        runtime.frame(0.0);
        runtime.frame(3.1);
        assert!(runtime.find("play").is_none());

        runtime.key(KeyEvent {
            key: Key::Text("m".into()),
            mods: Default::default(),
        });
        runtime.frame(0.1);
        assert!(runtime.find("play").is_some());
        runtime.frame(0.1);
        runtime.frame(3.1);
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
        assert_eq!(fill(&runtime), palette.text.alpha(0.55));

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
        session.poll(&mut tx, true);
        assert_eq!(session.timeline().position(), 5.0);

        session.seek(5.0);
        session.poll(&mut tx, true);
        assert_eq!(
            session.timeline().position(),
            5.0,
            "the preview position must survive the exact seek"
        );

        log.lock().unwrap().position = 5.0;
        session.poll(&mut tx, true);
        assert_eq!(session.timeline().position(), 5.0);

        log.lock().unwrap().position = 0.0;
        session.poll(&mut tx, true);
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
        session_mut(&mut app).poll(&mut tx, true);
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
        session_mut(&mut app).poll(&mut tx, true);
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
        session.poll(&mut tx, true);
        assert!(
            session.is_scrubbing(),
            "the busy engine cleared the release seek"
        );

        log.lock().unwrap().seeking = false;
        session.poll(&mut tx, true);
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
    fn opening_a_file_reveals_both_capsules() {
        let dir = temp_dir("open-reveal");
        let source = dir.join("reveal.mkv");
        std::fs::write(&source, b"not decoded by the fake").unwrap();

        let (mut app, _) = app_with_fake();
        app.top_visible = false;
        app.transport_visible = false;
        app.open(source);
        assert!(app.top_visible, "the title capsule stayed hidden");
        assert!(app.transport_visible, "the transport stayed hidden");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_empty_screen_has_one_open_action_and_no_player_chrome() {
        let app = App::with_session(None);
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        assert!(runtime.find("open-empty").is_some());
        for key in [
            "open",
            "audio-menu",
            "subs-menu",
            "export-toggle",
            "transport",
        ] {
            assert!(runtime.find(key).is_none(), "{key}");
        }
    }

    #[test]
    fn a_long_toast_detail_clamps_to_three_lines() {
        let (mut app, _) = app_with_fake();
        app.notify_error(
            "Couldn't open the video",
            "The playback engine could not open this file because the container is damaged.              Choose a different file or re-download the clip."
                .repeat(4),
        );
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        let card = rect(&runtime, "toast-1");
        assert!(card[3] > 38.0, "the detail stayed on one line: {card:?}");
        assert!(card[3] < 120.0, "the detail was not clamped: {card:?}");
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
    fn the_fullscreen_control_requests_fullscreen() {
        let (mut app, _) = app_with_fake();
        app.transport_visible = false;
        let mut tx = Tx::default();
        request_fullscreen(&mut app, &mut tx);
        assert!(
            format!("{tx:?}").contains("toggle_full_screen: true"),
            "{tx:?}"
        );
        assert!(app.transport_visible);
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
    fn the_export_tray_has_no_export_mode_choice() {
        let (app, _) = app_with_fake();
        let mut runtime = Runtime::new(app, view, window(), Env::new(true, 1.0));
        runtime.frame(0.0);
        click(&mut runtime, "export-toggle");
        runtime.frame(0.2);
        assert!(runtime.find("mode-precise").is_none());
        assert!(runtime.find("mode-fast").is_none());
        assert!(runtime.find("export").is_some());
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
