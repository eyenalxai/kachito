//! The Kachito screen: one file, one video surface, one transport bar.

use crate::engine::{MpvEngine, PlayerEngine, VIDEO_HEIGHT, VIDEO_WIDTH};
use rune::tok::{self, type_};
use rune::{AppSpec, Cx, Drag, El, FileDialogOptions, SurfaceFit, Tx, Waker, col, el, row, text};
use std::path::PathBuf;
use std::time::Duration;

const SUPPORTED: &[&str] = &["mkv", "mp4", "m4v", "mov", "webm"];
const SPEEDS: &[f32] = &[0.5, 1.0, 1.25, 1.5, 2.0];

struct App {
    engine: Option<Box<dyn PlayerEngine>>,
    path: Option<PathBuf>,
    error: Option<String>,
    volume: f32,
    speed: f32,
    last_dir: Option<PathBuf>,
}

impl App {
    fn new() -> Self {
        let mut app = Self {
            engine: None,
            path: None,
            error: None,
            volume: 1.0,
            speed: 1.0,
            last_dir: None,
        };
        match MpvEngine::new(rune::is_headless()) {
            Ok(engine) => app.engine = Some(Box::new(engine)),
            Err(error) => app.error = Some(error),
        }
        if let Some(path) = rune::extra_args().first() {
            app.open(PathBuf::from(path));
        }
        if rune::is_headless() && app.path.is_some() {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while app
                .engine
                .as_ref()
                .is_some_and(|engine| engine.frames() == 0)
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        app
    }

    fn open(&mut self, path: PathBuf) {
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !SUPPORTED.contains(&extension.as_str()) {
            self.error = Some(format!("{} is not an MKV or MP4 file.", path.display()));
            return;
        }
        let Some(engine) = self.engine.as_mut() else {
            self.error = Some("The playback engine is not available.".into());
            return;
        };
        match engine.open(&path) {
            Ok(()) => {
                self.last_dir = path.parent().map(|parent| parent.to_path_buf());
                self.path = Some(path);
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
        if let Some(engine) = self.engine.as_mut() {
            engine.toggle();
        }
    }

    fn seek_fraction(&mut self, fraction: f32) {
        if let Some(engine) = self.engine.as_mut()
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

    fn error_text(&self) -> Option<String> {
        self.error
            .clone()
            .or_else(|| self.engine.as_ref().and_then(|engine| engine.error()))
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
    if app
        .engine
        .as_ref()
        .is_some_and(|engine| engine.is_playing())
    {
        tx.wake_after(Duration::from_millis(100));
        true
    } else {
        false
    }
}

fn open_dialog(app: &mut App, tx: &mut Tx) {
    let options = FileDialogOptions {
        title: "Open video".into(),
        directory: app.last_dir.clone(),
        ..Default::default()
    }
    .filter("Video", &["mkv", "mp4", "m4v", "mov", "webm"]);
    tx.open_file(options, |app: &mut App, result, _| match result {
        Ok(Some(path)) => app.open(path),
        Ok(None) => {}
        Err(error) => app.error = Some(error.to_string()),
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

fn view(app: &App, cx: &mut Cx<App>) -> El {
    let theme = tok::theme(true);
    let accent = cx.accent();
    let error = app.error_text();
    let root = col().w_full().h_full().fill(theme.bg);

    let screen = if let Some(path) = &app.path {
        let engine = app.engine.as_ref().unwrap();
        let duration = engine.duration().unwrap_or(0.0);
        let position = engine.position();
        let playing = engine.is_playing();
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

        let top = row()
            .w_full()
            .padding(10.0)
            .gap(10.0)
            .center_y()
            .fill(theme.card)
            .child(text(title).font(type_::BODY).color(theme.text).grow(1.0))
            .child(open_button(&theme));

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
                button(if playing { "Pause" } else { "Play" }, true, &theme)
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
            .child(
                el().key("video")
                    .w(VIDEO_WIDTH as f32)
                    .h(VIDEO_HEIGHT as f32)
                    .native_surface(&surface, SurfaceFit::Fit),
            )
            .child(controls)
    } else {
        col()
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
            )
    };

    let screen = if let Some(error) = error {
        screen.child(text(error).font(type_::LABEL).color(tok::accent(2, true)))
    } else {
        screen
    };

    root.child(screen)
        .on_drop_files(|app: &mut App, files, _: &mut Tx| app.open_first(files))
}

fn format_time(position: f64, duration: f64) -> String {
    let position = position.max(0.0).round() as u64;
    let duration = duration.max(0.0).round() as u64;
    format!(
        "{}:{:02} / {}:{:02}",
        position / 60,
        position % 60,
        duration / 60,
        duration % 60
    )
}

pub fn run() {
    rune::run(AppSpec {
        setup: Some(setup),
        poll: Some(poll),
        ..AppSpec::new(
            "Kachito",
            (VIDEO_WIDTH as f32, VIDEO_HEIGHT as f32 + 104.0),
            state,
            view,
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::{Env, Runtime};

    struct Fake {
        surface: rune::NativeSurface,
        playing: bool,
    }

    impl Default for Fake {
        fn default() -> Self {
            Self {
                surface: rune::NativeSurface::new(),
                playing: false,
            }
        }
    }

    impl PlayerEngine for Fake {
        fn open(&mut self, _: &std::path::Path) -> Result<(), String> {
            Ok(())
        }
        fn play(&mut self) {
            self.playing = true;
        }
        fn pause(&mut self) {
            self.playing = false;
        }
        fn toggle(&mut self) {
            self.playing = !self.playing;
        }
        fn seek(&mut self, _: f64) {}
        fn set_volume(&mut self, _: f32) {}
        fn set_speed(&mut self, _: f32) {}
        fn position(&self) -> f64 {
            0.0
        }
        fn duration(&self) -> Option<f64> {
            Some(10.0)
        }
        fn is_playing(&self) -> bool {
            self.playing
        }
        fn error(&self) -> Option<String> {
            None
        }
        fn surface(&self) -> rune::NativeSurface {
            self.surface.clone()
        }
        fn frames(&self) -> u64 {
            0
        }
        fn set_waker(&self, _: Waker) {}
    }

    fn app_with_fake() -> App {
        App {
            engine: Some(Box::new(Fake::default())),
            path: Some(PathBuf::from("test.mkv")),
            error: None,
            volume: 1.0,
            speed: 1.0,
            last_dir: None,
        }
    }

    #[test]
    fn the_play_button_toggles_playback() {
        let mut runtime = Runtime::new(
            app_with_fake(),
            view,
            (VIDEO_WIDTH as f32, VIDEO_HEIGHT as f32 + 104.0),
            Env::new(true, 1.0),
        );
        runtime.frame(0.0);
        let (x, y) = runtime.find("play").expect("the play button");
        runtime.pointer_down(x, y);
        runtime.pointer_up(x, y);
        runtime.frame(1.0 / 60.0);
        assert!(runtime.state.engine.as_ref().unwrap().is_playing());
    }

    #[test]
    fn dropping_a_file_opens_it() {
        let mut app = app_with_fake();
        app.path = None;
        let mut runtime = Runtime::new(
            app,
            view,
            (VIDEO_WIDTH as f32, VIDEO_HEIGHT as f32 + 104.0),
            Env::new(true, 1.0),
        );
        runtime.frame(0.0);
        let at = runtime.find("video").unwrap_or((10.0, 10.0));
        runtime.drop_files(vec![PathBuf::from("dropped.mkv")], Some(at));
        runtime.frame(1.0 / 60.0);
        assert_eq!(runtime.state.path, Some(PathBuf::from("dropped.mkv")));
    }

    #[test]
    fn volume_and_speed_controls_clamp_and_cycle() {
        let mut app = app_with_fake();
        app.volume_delta(-0.5);
        assert_eq!(app.volume, 0.5);
        app.volume_delta(-5.0);
        assert_eq!(app.volume, 0.0);
        app.cycle_speed();
        assert_eq!(app.speed, 1.25);
    }

    #[test]
    fn a_non_video_file_is_rejected() {
        let mut app = app_with_fake();
        app.open(PathBuf::from("notes.txt"));
        assert!(app.error.is_some());
        assert!(app.path.is_some(), "the previous file stays open");
    }
}
