//! Clip export through the system ffmpeg.

use std::io::{BufRead, BufReader, Read};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportMode {
    Fast,
    Precise,
}

impl ExportMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Fast => "Fast",
            Self::Precise => "Precise",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Fast => Self::Precise,
            Self::Precise => Self::Fast,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ExportRequest {
    pub source: PathBuf,
    pub start: f64,
    pub end: f64,
    pub audio_index: Option<usize>,
    pub mode: ExportMode,
    pub output: PathBuf,
}

impl ExportRequest {
    pub fn duration(&self) -> f64 {
        (self.end - self.start).max(0.0)
    }

    pub fn command(&self) -> Vec<String> {
        let mut args = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-nostats".into(),
            "-progress".into(),
            "pipe:1".into(),
            "-ss".into(),
            format!("{:.3}", self.start.max(0.0)),
            "-t".into(),
            format!("{:.3}", self.duration()),
            "-i".into(),
            self.source.display().to_string(),
            "-map".into(),
            "0:v:0".into(),
        ];
        if let Some(index) = self.audio_index {
            args.push("-map".into());
            args.push(format!("0:a:{index}"));
        }
        match self.mode {
            ExportMode::Fast => {
                args.extend(["-c".into(), "copy".into()]);
                args.extend(["-avoid_negative_ts".into(), "make_zero".into()]);
            }
            ExportMode::Precise => {
                args.extend(["-c:v".into(), "libx264".into()]);
                args.extend(["-preset".into(), "veryfast".into()]);
                args.extend(["-crf".into(), "20".into()]);
                args.extend(["-c:a".into(), "aac".into()]);
                args.extend(["-b:a".into(), "160k".into()]);
            }
        }
        args.push("-y".into());
        args.push(self.output.display().to_string());
        args
    }
}

#[derive(Clone, Debug, Default)]
pub struct ExportProgress {
    pub running: bool,
    pub fraction: f32,
    pub error: Option<String>,
    pub finished: bool,
}

pub struct ExportJob {
    progress: Arc<Mutex<ExportProgress>>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    thread: Option<JoinHandle<()>>,
}

impl ExportJob {
    pub fn start(request: ExportRequest) -> Self {
        let progress = Arc::new(Mutex::new(ExportProgress {
            running: true,
            ..Default::default()
        }));
        let cancel = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let thread = {
            let progress = progress.clone();
            let cancel = cancel.clone();
            let child = child.clone();
            thread::Builder::new()
                .name("kachito-export".into())
                .spawn(move || run(request, progress, cancel, child))
                .ok()
        };
        Self {
            progress,
            cancel,
            child,
            thread,
        }
    }

    pub fn progress(&self) -> ExportProgress {
        self.progress.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
    }
}

impl Drop for ExportJob {
    fn drop(&mut self) {
        self.cancel();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn run(
    request: ExportRequest,
    progress: Arc<Mutex<ExportProgress>>,
    cancel: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
) {
    let duration = request.duration();
    let mut command = Command::new("ffmpeg");
    command
        .args(request.command())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            finish(
                &progress,
                Some(format!("could not start ffmpeg: {error}")),
                false,
            );
            return;
        }
    };
    let stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    *child_slot.lock().unwrap() = Some(child);

    let errors = Arc::new(Mutex::new(String::new()));
    let stderr_thread = stderr.take().map(|mut stderr| {
        let errors = errors.clone();
        thread::spawn(move || {
            let mut text = String::new();
            let _ = stderr.read_to_string(&mut text);
            *errors.lock().unwrap() = text;
        })
    });

    if let Some(stdout) = stdout {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            if let Some(value) = line.strip_prefix("out_time_us=")
                && let Ok(micros) = value.parse::<f64>()
                && duration > 0.0
            {
                let fraction = (micros / 1_000_000.0 / duration).clamp(0.0, 1.0) as f32;
                progress.lock().unwrap().fraction = fraction;
            }
        }
    }

    let status = child_slot
        .lock()
        .unwrap()
        .as_mut()
        .and_then(|child| child.wait().ok());
    if let Some(thread) = stderr_thread {
        let _ = thread.join();
    }
    let cancelled = cancel.load(Ordering::Relaxed);
    if cancelled {
        let _ = std::fs::remove_file(&request.output);
        finish(&progress, None, false);
        return;
    }
    let errors = errors.lock().unwrap().trim().to_string();
    match status {
        Some(status) if status.success() => finish(&progress, None, true),
        Some(status) => finish(
            &progress,
            Some(if errors.is_empty() {
                format!("ffmpeg exited with {status}")
            } else {
                errors
            }),
            false,
        ),
        None => finish(&progress, Some("ffmpeg did not finish".into()), false),
    }
}

fn finish(progress: &Arc<Mutex<ExportProgress>>, error: Option<String>, finished: bool) {
    let mut progress = progress.lock().unwrap();
    progress.running = false;
    progress.finished = finished;
    progress.error = error;
    if finished {
        progress.fraction = 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::{Duration, Instant};

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/test")
            .join(name)
    }

    #[test]
    fn precise_command_keeps_video_and_audio_and_drops_subtitles() {
        let request = ExportRequest {
            source: PathBuf::from("in.mkv"),
            start: 2.0,
            end: 5.0,
            audio_index: Some(1),
            mode: ExportMode::Precise,
            output: PathBuf::from("out.mkv"),
        };
        let command = request.command();
        assert!(command.contains(&"-ss".into()));
        assert!(command.contains(&"2.000".into()));
        assert!(command.contains(&"3.000".into()));
        assert!(command.contains(&"0:v:0".into()));
        assert!(command.contains(&"0:a:1".into()));
        assert!(!command.iter().any(|arg| arg == "0:s:0"));
        assert!(command.contains(&"libx264".into()));
        assert!(command.contains(&"out.mkv".into()));
    }

    #[test]
    fn fast_command_copies_packets() {
        let request = ExportRequest {
            source: PathBuf::from("in.mkv"),
            start: 0.0,
            end: 2.0,
            audio_index: None,
            mode: ExportMode::Fast,
            output: PathBuf::from("out.mkv"),
        };
        let command = request.command();
        assert!(command.contains(&"copy".into()));
        assert!(!command.contains(&"libx264".into()));
        assert!(!command.contains(&"0:a:0".into()));
    }

    #[test]
    fn a_precise_export_writes_video_and_audio_without_subtitles() {
        let output =
            std::env::temp_dir().join(format!("kachito-export-test-{}.mkv", std::process::id()));
        let _ = std::fs::remove_file(&output);
        let job = ExportJob::start(ExportRequest {
            source: fixture("tracks.mkv"),
            start: 0.0,
            end: 0.8,
            audio_index: Some(0),
            mode: ExportMode::Precise,
            output: output.clone(),
        });
        let deadline = Instant::now() + Duration::from_secs(30);
        while job.progress().running && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        let progress = job.progress();
        assert!(!progress.running, "the export did not finish");
        assert_eq!(progress.error, None);
        assert!(Path::new(&output).exists());

        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-show_entries",
                "stream=codec_type",
                "-of",
                "csv=p=0",
            ])
            .arg(&output)
            .output()
            .expect("run ffprobe");
        let streams = String::from_utf8_lossy(&probe.stdout);
        assert!(streams.contains("video"), "{streams}");
        assert!(streams.contains("audio"), "{streams}");
        assert!(!streams.contains("subtitle"), "{streams}");
        let _ = std::fs::remove_file(&output);
    }
}
