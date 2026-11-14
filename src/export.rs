//! Clip export through the system ffmpeg.

use std::ffi::OsString;
use std::fmt;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportError {
    InvalidRange,
    RangeShorterThanFrame,
    InvalidFrameDuration,
    InvalidPath,
    SameFile,
    AudioTrackOutOfRange,
    Spawn(String),
}

impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRange => {
                f.write_str("Choose a start and end time with the end after the start.")
            }
            Self::RangeShorterThanFrame => {
                f.write_str("Choose a range at least one source frame long.")
            }
            Self::InvalidFrameDuration => {
                f.write_str("The source frame duration is not available.")
            }
            Self::InvalidPath => {
                f.write_str("Choose a source video and an output file with valid paths.")
            }
            Self::SameFile => f.write_str("Choose an output file other than the source video."),
            Self::AudioTrackOutOfRange => {
                f.write_str("The selected audio track is no longer available.")
            }
            Self::Spawn(error) => write!(f, "Could not start the export worker: {error}"),
        }
    }
}

impl std::error::Error for ExportError {}

#[derive(Clone, Copy, Debug, Default)]
pub struct AudioSelection {
    pub index: Option<usize>,
    pub count: usize,
}

#[derive(Clone, Debug)]
pub struct ExportRequest {
    source: PathBuf,
    start: f64,
    end: f64,
    audio_index: Option<usize>,
    mode: ExportMode,
    output: PathBuf,
}

impl ExportRequest {
    pub fn new(
        source: PathBuf,
        start: f64,
        end: f64,
        audio: AudioSelection,
        frame_duration: f64,
        mode: ExportMode,
        output: PathBuf,
    ) -> Result<Self, ExportError> {
        if source.as_os_str().is_empty() || output.as_os_str().is_empty() {
            return Err(ExportError::InvalidPath);
        }
        if source.file_name().is_none() || output.file_name().is_none() {
            return Err(ExportError::InvalidPath);
        }
        if has_nul(&source) || has_nul(&output) {
            return Err(ExportError::InvalidPath);
        }
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
            return Err(ExportError::InvalidRange);
        }
        if !frame_duration.is_finite() || frame_duration <= 0.0 {
            return Err(ExportError::InvalidFrameDuration);
        }
        if end - start + 1e-9 < frame_duration {
            return Err(ExportError::RangeShorterThanFrame);
        }
        if same_file(&source, &output)? {
            return Err(ExportError::SameFile);
        }
        if let Some(index) = audio.index
            && index >= audio.count
        {
            return Err(ExportError::AudioTrackOutOfRange);
        }
        Ok(Self {
            source,
            start,
            end,
            audio_index: audio.index,
            mode,
            output,
        })
    }

    pub fn duration(&self) -> f64 {
        self.end - self.start
    }

    fn command_to(&self, output: &Path) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "-hide_banner".into(),
            "-loglevel".into(),
            "error".into(),
            "-nostats".into(),
            "-progress".into(),
            "pipe:1".into(),
            "-ss".into(),
            format!("{:.3}", self.start).into(),
            "-t".into(),
            format!("{:.3}", self.duration()).into(),
            "-i".into(),
            self.source.clone().into_os_string(),
            "-map".into(),
            "0:v:0".into(),
        ];
        if let Some(index) = self.audio_index {
            args.push("-map".into());
            args.push(format!("0:a:{index}").into());
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
        args.push(output.as_os_str().to_os_string());
        args
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ExportState {
    Running { fraction: f32 },
    Finished,
    Cancelled,
    Failed(String),
}

pub struct ExportJob {
    state: Arc<Mutex<ExportState>>,
    cancel: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    thread: Option<JoinHandle<()>>,
}

impl ExportJob {
    pub fn start(request: ExportRequest) -> Result<Self, ExportError> {
        let state = Arc::new(Mutex::new(ExportState::Running { fraction: 0.0 }));
        let cancel = Arc::new(AtomicBool::new(false));
        let child = Arc::new(Mutex::new(None));
        let thread = {
            let state = state.clone();
            let cancel = cancel.clone();
            let child = child.clone();
            thread::Builder::new()
                .name("kachito-export".into())
                .spawn(move || run(request, state, cancel, child))
                .map_err(|error| ExportError::Spawn(error.to_string()))?
        };
        Ok(Self {
            state,
            cancel,
            child,
            thread: Some(thread),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_state(state: ExportState) -> Self {
        Self {
            state: Arc::new(Mutex::new(state)),
            cancel: Arc::new(AtomicBool::new(false)),
            child: Arc::new(Mutex::new(None)),
            thread: None,
        }
    }

    pub fn state(&self) -> ExportState {
        self.state.lock().unwrap().clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        kill_child(&self.child);
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
    state: Arc<Mutex<ExportState>>,
    cancel: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
) {
    let duration = request.duration();
    let temp = temp_output(&request.output);
    let mut command = Command::new("ffmpeg");
    command
        .args(request.command_to(&temp))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            remove_file(&temp);
            set_state(
                &state,
                ExportState::Failed(format!("Could not start ffmpeg: {error}")),
            );
            return;
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    *child_slot.lock().unwrap() = Some(child);
    if cancel.load(Ordering::Relaxed) {
        kill_child(&child_slot);
    }

    let errors = Arc::new(Mutex::new(String::new()));
    let stderr_thread = stderr.map(|mut stderr| {
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
                let next = (micros / 1_000_000.0 / duration).clamp(0.0, 1.0) as f32;
                if let ExportState::Running { fraction } = &mut *state.lock().unwrap() {
                    *fraction = next;
                }
            }
        }
    }

    let status = loop {
        let mut slot = child_slot.lock().unwrap();
        let Some(child) = slot.as_mut() else {
            break Err("The export process was not available.".to_string());
        };
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Ok(None) => {}
            Err(error) => break Err(format!("Could not wait for ffmpeg: {error}")),
        }
        drop(slot);
        if cancel.load(Ordering::Relaxed) {
            kill_child(&child_slot);
        }
        thread::sleep(Duration::from_millis(10));
    };
    if let Some(thread) = stderr_thread {
        let _ = thread.join();
    }
    if cancel.load(Ordering::Relaxed) {
        remove_file(&temp);
        set_state(&state, ExportState::Cancelled);
        return;
    }
    let errors = errors.lock().unwrap().trim().to_string();
    match status {
        Ok(status) if status.success() => match std::fs::rename(&temp, &request.output) {
            Ok(()) => set_state(&state, ExportState::Finished),
            Err(error) => {
                remove_file(&temp);
                set_state(
                    &state,
                    ExportState::Failed(format!("Could not move the export into place: {error}")),
                );
            }
        },
        Ok(status) => {
            remove_file(&temp);
            set_state(
                &state,
                ExportState::Failed(if errors.is_empty() {
                    format!("ffmpeg exited with {status}")
                } else {
                    errors
                }),
            );
        }
        Err(error) => {
            remove_file(&temp);
            set_state(&state, ExportState::Failed(error));
        }
    }
}

fn set_state(state: &Arc<Mutex<ExportState>>, next: ExportState) {
    *state.lock().unwrap() = next;
}

fn kill_child(slot: &Arc<Mutex<Option<Child>>>) {
    if let Some(child) = slot.lock().unwrap().as_mut() {
        let _ = child.kill();
    }
}

fn temp_output(output: &Path) -> PathBuf {
    static TEMP_ID: AtomicU64 = AtomicU64::new(0);
    let mut name = std::ffi::OsString::from(".");
    if let Some(file_name) = output.file_name() {
        name.push(file_name);
    }
    let id = TEMP_ID.fetch_add(1, Ordering::Relaxed);
    name.push(format!(".{}.{id}.part", std::process::id()));
    if let Some(extension) = output.extension() {
        name.push(".");
        name.push(extension);
    }
    output.with_file_name(name)
}

fn remove_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn has_nul(path: &Path) -> bool {
    path.as_os_str().as_encoded_bytes().contains(&0)
}

fn same_file(a: &Path, b: &Path) -> Result<bool, ExportError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(a), Ok(b)) = (std::fs::metadata(a), std::fs::metadata(b)) {
            return Ok(a.dev() == b.dev() && a.ino() == b.ino());
        }
    }
    if let (Ok(a), Ok(b)) = (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        return Ok(a == b);
    }
    match (std::path::absolute(a), std::path::absolute(b)) {
        (Ok(a), Ok(b)) => Ok(a == b),
        _ => Err(ExportError::InvalidPath),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("assets/test")
            .join(name)
    }

    fn wait_for_terminal(job: &ExportJob, timeout: Duration) -> ExportState {
        let deadline = Instant::now() + timeout;
        loop {
            let state = job.state();
            if !matches!(&state, ExportState::Running { .. }) || Instant::now() >= deadline {
                return state;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn precise_command_keeps_video_and_audio_and_drops_subtitles() {
        let request = ExportRequest::new(
            PathBuf::from("in.mkv"),
            2.0,
            5.0,
            AudioSelection {
                index: Some(1),
                count: 2,
            },
            1.0 / 60.0,
            ExportMode::Precise,
            PathBuf::from("out.mkv"),
        )
        .unwrap();
        let command = request.command_to(Path::new("out.mkv"));
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
        let request = ExportRequest::new(
            PathBuf::from("in.mkv"),
            0.0,
            2.0,
            AudioSelection::default(),
            1.0 / 60.0,
            ExportMode::Fast,
            PathBuf::from("out.mkv"),
        )
        .unwrap();
        let command = request.command_to(Path::new("out.mkv"));
        assert!(command.contains(&"copy".into()));
        assert!(!command.contains(&"libx264".into()));
        assert!(!command.contains(&"0:a:0".into()));
    }

    #[test]
    fn rejects_inverted_range() {
        let result = ExportRequest::new(
            PathBuf::from("in.mkv"),
            2.0,
            2.0,
            AudioSelection::default(),
            1.0 / 60.0,
            ExportMode::Fast,
            PathBuf::from("out.mkv"),
        );
        assert_eq!(result.unwrap_err(), ExportError::InvalidRange);
    }

    #[test]
    fn rejects_non_finite_range() {
        for (start, end) in [(f64::NAN, 2.0), (0.0, f64::INFINITY)] {
            let result = ExportRequest::new(
                PathBuf::from("in.mkv"),
                start,
                end,
                AudioSelection::default(),
                1.0 / 60.0,
                ExportMode::Fast,
                PathBuf::from("out.mkv"),
            );
            assert_eq!(result.unwrap_err(), ExportError::InvalidRange);
        }
    }

    #[test]
    fn rejects_a_range_shorter_than_one_source_frame() {
        let result = ExportRequest::new(
            PathBuf::from("in.mkv"),
            0.0,
            0.01,
            AudioSelection::default(),
            1.0 / 24.0,
            ExportMode::Fast,
            PathBuf::from("out.mkv"),
        );
        assert_eq!(result.unwrap_err(), ExportError::RangeShorterThanFrame);
    }

    #[test]
    fn accepts_an_exact_one_frame_range() {
        let result = ExportRequest::new(
            PathBuf::from("in.mkv"),
            3.0,
            3.0 + 1.0 / 24.0,
            AudioSelection::default(),
            1.0 / 24.0,
            ExportMode::Fast,
            PathBuf::from("out.mkv"),
        );
        assert!(result.is_ok());
    }

    #[test]
    fn rejects_a_missing_frame_duration() {
        let result = ExportRequest::new(
            PathBuf::from("in.mkv"),
            0.0,
            1.0,
            AudioSelection::default(),
            0.0,
            ExportMode::Fast,
            PathBuf::from("out.mkv"),
        );
        assert_eq!(result.unwrap_err(), ExportError::InvalidFrameDuration);
    }

    #[test]
    fn rejects_the_source_as_output() {
        let result = ExportRequest::new(
            PathBuf::from("clip.mkv"),
            0.0,
            2.0,
            AudioSelection::default(),
            1.0 / 60.0,
            ExportMode::Fast,
            PathBuf::from("clip.mkv"),
        );
        assert_eq!(result.unwrap_err(), ExportError::SameFile);
    }

    #[test]
    fn rejects_an_audio_index_outside_the_track_list() {
        let result = ExportRequest::new(
            PathBuf::from("in.mkv"),
            0.0,
            2.0,
            AudioSelection {
                index: Some(1),
                count: 1,
            },
            1.0 / 60.0,
            ExportMode::Fast,
            PathBuf::from("out.mkv"),
        );
        assert_eq!(result.unwrap_err(), ExportError::AudioTrackOutOfRange);
    }

    #[test]
    fn rejects_empty_paths() {
        let result = ExportRequest::new(
            PathBuf::new(),
            0.0,
            2.0,
            AudioSelection::default(),
            1.0 / 60.0,
            ExportMode::Fast,
            PathBuf::from("out.mkv"),
        );
        assert_eq!(result.unwrap_err(), ExportError::InvalidPath);
    }

    #[test]
    fn rejects_paths_with_a_nul_byte() {
        let result = ExportRequest::new(
            PathBuf::from("in\0.mkv"),
            0.0,
            2.0,
            AudioSelection::default(),
            1.0 / 60.0,
            ExportMode::Fast,
            PathBuf::from("out.mkv"),
        );
        assert_eq!(result.unwrap_err(), ExportError::InvalidPath);
    }

    #[cfg(unix)]
    #[test]
    fn accepts_non_utf8_paths_and_passes_them_as_path_arguments() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let source = PathBuf::from(OsString::from_vec(b"in-\xff.mkv".to_vec()));
        let output = PathBuf::from(OsString::from_vec(b"out-\xfe.mkv".to_vec()));
        let request = ExportRequest::new(
            source.clone(),
            0.0,
            2.0,
            AudioSelection::default(),
            1.0 / 60.0,
            ExportMode::Fast,
            output.clone(),
        )
        .expect("a non-UTF-8 path is a valid path");
        let command = request.command_to(&output);
        assert!(
            command
                .windows(2)
                .any(|pair| pair[0] == "-i" && pair[1] == source.as_os_str()),
            "{command:?}"
        );
        assert_eq!(command.last(), Some(&output.into_os_string()));
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_source_and_output_export() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir =
            std::env::temp_dir().join(format!("kachito-export-non-utf8-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join(OsString::from_vec(b"source-\xff.mkv".to_vec()));
        std::fs::copy(fixture("tracks.mkv"), &source).unwrap();
        let output = dir.join(OsString::from_vec(b"out-\xfe.mkv".to_vec()));
        let request = ExportRequest::new(
            source,
            0.0,
            0.8,
            AudioSelection {
                index: Some(0),
                count: 1,
            },
            1.0 / 60.0,
            ExportMode::Precise,
            output.clone(),
        )
        .expect("a non-UTF-8 source and output are valid paths");
        let job = ExportJob::start(request).expect("start the export");
        let state = wait_for_terminal(&job, Duration::from_secs(30));
        assert_eq!(state, ExportState::Finished);
        assert!(output.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_precise_export_writes_video_and_audio_without_subtitles() {
        let dir =
            std::env::temp_dir().join(format!("kachito-export-success-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.mkv");
        let request = ExportRequest::new(
            fixture("tracks.mkv"),
            0.0,
            0.8,
            AudioSelection {
                index: Some(0),
                count: 1,
            },
            1.0 / 60.0,
            ExportMode::Precise,
            output.clone(),
        )
        .expect("a valid request");
        let job = ExportJob::start(request).expect("start the export");
        let state = wait_for_terminal(&job, Duration::from_secs(30));
        assert_eq!(state, ExportState::Finished);
        assert!(output.exists());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "a successful export leaves only the final file"
        );

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
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_one_frame_export_at_24fps_contains_a_frame() {
        let dir = std::env::temp_dir().join(format!("kachito-export-24fps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("source.mkv");
        let generated = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc=size=64x64:rate=24:duration=1",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
                "-y",
            ])
            .arg(&source)
            .status()
            .expect("generate the 24 fps source");
        assert!(generated.success(), "ffmpeg could not generate the source");

        let output = dir.join("out.mkv");
        let request = ExportRequest::new(
            source,
            0.0,
            1.0 / 24.0,
            AudioSelection::default(),
            1.0 / 24.0,
            ExportMode::Precise,
            output.clone(),
        )
        .expect("a one-frame request");
        let job = ExportJob::start(request).expect("start the export");
        let state = wait_for_terminal(&job, Duration::from_secs(30));
        assert_eq!(state, ExportState::Finished);

        let probe = Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-count_frames",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=nb_read_frames",
                "-of",
                "csv=p=0",
            ])
            .arg(&output)
            .output()
            .expect("run ffprobe");
        let frames: u32 = String::from_utf8_lossy(&probe.stdout)
            .trim()
            .parse()
            .expect("a frame count");
        assert!(frames >= 1, "{frames}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_export_leaves_no_output() {
        let dir =
            std::env::temp_dir().join(format!("kachito-export-failure-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("not-video.mkv");
        std::fs::write(&source, b"not a video").unwrap();
        let output = dir.join("out.mkv");
        let request = ExportRequest::new(
            source,
            0.0,
            1.0,
            AudioSelection::default(),
            1.0 / 60.0,
            ExportMode::Fast,
            output.clone(),
        )
        .expect("a valid request");
        let job = ExportJob::start(request).expect("start the export");
        let state = wait_for_terminal(&job, Duration::from_secs(10));
        assert!(matches!(state, ExportState::Failed(_)), "{state:?}");
        assert!(!output.exists());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            1,
            "a failed export leaves only the source file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_cancelled_export_leaves_no_output() {
        let dir =
            std::env::temp_dir().join(format!("kachito-export-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.mkv");
        let request = ExportRequest::new(
            fixture("tracks.mkv"),
            0.0,
            2.0,
            AudioSelection {
                index: Some(0),
                count: 1,
            },
            1.0 / 60.0,
            ExportMode::Precise,
            output.clone(),
        )
        .expect("a valid request");
        let job = ExportJob::start(request).expect("start the export");
        job.cancel();
        let state = wait_for_terminal(&job, Duration::from_secs(10));
        assert_eq!(state, ExportState::Cancelled);
        assert!(!output.exists());
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "a cancelled export leaves no file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
