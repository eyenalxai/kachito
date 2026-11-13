//! The run log: every line goes to stderr and to a file at the same time.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::path::PathBuf;

/// Where the run log writes. `path` is the file sink, `stderr` the terminal sink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogConfig {
    pub path: Option<PathBuf>,
    pub stderr: bool,
}

impl LogConfig {
    pub fn from_env() -> Self {
        Self::resolve(
            std::env::var_os("KACHITO_LOG"),
            std::env::var_os("XDG_STATE_HOME"),
            std::env::var_os("HOME"),
        )
    }

    pub fn resolve(
        value: Option<OsString>,
        state_home: Option<OsString>,
        home: Option<OsString>,
    ) -> Self {
        if value.as_deref() == Some(OsStr::new("off")) {
            return Self {
                path: None,
                stderr: false,
            };
        }
        if let Some(value) = value.filter(|value| !value.is_empty()) {
            return Self {
                path: Some(PathBuf::from(value)),
                stderr: true,
            };
        }
        let base = state_home
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .map(|state| state.join("kachito"))
            .or_else(|| {
                home.filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .map(|home| home.join(".local/state/kachito"))
            });
        Self {
            path: base.map(|base| base.join("kachito.log")),
            stderr: true,
        }
    }
}

/// A second destination for run-log lines. Production uses stderr.
pub trait LogSink: Send + Sync {
    fn write_line(&self, line: &str);
}

pub struct StderrSink;

impl LogSink for StderrSink {
    fn write_line(&self, line: &str) {
        let _ = writeln!(std::io::stderr().lock(), "{line}");
    }
}

/// The cap for the state-directory file. One line records when it is reached.
pub const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineKind {
    Info,
    Warn,
    Command,
    CommandFailed,
    PropertyWrite,
    PropertyFailed,
    Mpv,
}

impl LineKind {
    fn label(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Command => "command",
            Self::CommandFailed => "command failed",
            Self::PropertyWrite => "property write",
            Self::PropertyFailed => "property write failed",
            Self::Mpv => "mpv",
        }
    }
}

struct FileSink {
    file: Option<std::fs::File>,
    written: u64,
    capped: bool,
}

/// The run log. Lines go to the stderr sink and the file sink together.
pub struct RunLog {
    file: std::sync::Mutex<FileSink>,
    sink: Option<std::sync::Arc<dyn LogSink>>,
    limit: u64,
}

impl RunLog {
    pub fn from_env(sink: std::sync::Arc<dyn LogSink>) -> Self {
        Self::from_config(LogConfig::from_env(), sink)
    }

    pub fn from_config(config: LogConfig, sink: std::sync::Arc<dyn LogSink>) -> Self {
        Self::with_limit(config, sink, MAX_FILE_BYTES)
    }

    pub fn disabled() -> Self {
        Self {
            file: std::sync::Mutex::new(FileSink {
                file: None,
                written: 0,
                capped: false,
            }),
            sink: None,
            limit: MAX_FILE_BYTES,
        }
    }

    fn with_limit(config: LogConfig, sink: std::sync::Arc<dyn LogSink>, limit: u64) -> Self {
        let mut open_error = None;
        let mut file = None;
        if let Some(candidate) = config.path {
            if let Some(parent) = candidate
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                open_error = Some(format!("cannot create {}: {error}", parent.display()));
            }
            if open_error.is_none() {
                match std::fs::OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(&candidate)
                {
                    Ok(opened) => file = Some(opened),
                    Err(error) => {
                        open_error = Some(format!("cannot open {}: {error}", candidate.display()));
                    }
                }
            }
        }
        let log = Self {
            file: std::sync::Mutex::new(FileSink {
                file,
                written: 0,
                capped: false,
            }),
            sink: config.stderr.then_some(sink),
            limit,
        };
        if let Some(error) = open_error {
            log.warn(format!("run log file unavailable: {error}"));
        }
        log
    }

    pub fn info(&self, message: impl AsRef<str>) {
        self.write(LineKind::Info, message.as_ref());
    }

    pub fn warn(&self, message: impl AsRef<str>) {
        self.write(LineKind::Warn, message.as_ref());
    }

    pub fn command(&self, args: &[&str]) {
        self.write(LineKind::Command, &args.join(" "));
    }

    pub fn command_failed(&self, failure: &str) {
        self.write(LineKind::CommandFailed, failure);
    }

    pub fn property_write(&self, name: &str, value: &str) {
        self.write(LineKind::PropertyWrite, &format!("{name} = {value}"));
    }

    pub fn property_failed(&self, failure: &str) {
        self.write(LineKind::PropertyFailed, failure);
    }

    pub fn record_mpv_message(&self, level: &str, prefix: &str, text: &str) {
        let text = text.trim_end();
        self.write(LineKind::Mpv, &format!("{level} {prefix}: {text}"));
    }

    fn write(&self, kind: LineKind, message: &str) {
        let line = format!("{}: {message}", kind.label());
        let cap = self.write_file(&line);
        if let Some(sink) = &self.sink {
            sink.write_line(&line);
            if let Some(cap) = cap {
                sink.write_line(&cap);
            }
        }
    }

    fn write_file(&self, line: &str) -> Option<String> {
        let mut state = self.file.lock().unwrap();
        if state.file.is_none() || state.capped {
            return None;
        }
        let bytes = line.len() as u64 + 1;
        let cap = cap_line(self.limit);
        let reserve = cap.len() as u64 + 1;
        let effective = self.limit.saturating_sub(reserve);
        if state.written + bytes > effective {
            if let Some(file) = state.file.as_mut() {
                let _ = file
                    .write_all(cap.as_bytes())
                    .and_then(|_| file.write_all(b"\n"))
                    .and_then(|_| file.flush());
            }
            state.capped = true;
            return Some(cap);
        }
        let wrote = state.file.as_mut().is_some_and(|file| {
            file.write_all(line.as_bytes())
                .and_then(|_| file.write_all(b"\n"))
                .and_then(|_| file.flush())
                .is_ok()
        });
        if wrote {
            state.written += bytes;
        }
        None
    }
}

fn cap_line(limit: u64) -> String {
    if limit >= 1024 * 1024 && limit.is_multiple_of(1024 * 1024) {
        format!(
            "run log capped at {} MB; further lines are not written to the file",
            limit / (1024 * 1024)
        )
    } else {
        format!("run log capped at {limit} bytes; further lines are not written to the file")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::PathBuf;

    #[test]
    fn a_path_value_is_the_file_path_and_keeps_stderr_on() {
        let config = LogConfig::resolve(
            Some(OsString::from("/tmp/kachito.log")),
            Some(OsString::from("/state")),
            None,
        );
        assert_eq!(config.path, Some(PathBuf::from("/tmp/kachito.log")));
        assert!(config.stderr);
    }

    #[test]
    fn off_disables_both_sinks() {
        let config = LogConfig::resolve(Some(OsString::from("off")), None, None);
        assert_eq!(config.path, None);
        assert!(!config.stderr);
    }

    #[test]
    fn the_default_log_path_falls_back_to_the_home_directory() {
        let config = LogConfig::resolve(None, None, Some(OsString::from("/home/me")));
        assert_eq!(
            config.path,
            Some(PathBuf::from("/home/me/.local/state/kachito/kachito.log"))
        );
        assert!(config.stderr);
    }

    #[test]
    fn the_default_log_path_is_under_the_state_directory() {
        let config = LogConfig::resolve(
            None,
            Some(OsString::from("/state")),
            Some(OsString::from("/home/me")),
        );
        assert_eq!(
            config.path,
            Some(PathBuf::from("/state/kachito/kachito.log"))
        );
        assert!(config.stderr);
    }

    #[test]
    fn every_line_reaches_both_sinks() {
        let (log, sink, path) = testing::log_to_file("both");
        log.info("one");
        log.warn("two");
        log.command(&["seek", "1.000", "absolute"]);
        log.command_failed("seek 1.000 absolute: invalid parameter");
        log.property_write("volume", "NaN");
        log.property_failed("volume = NaN: invalid parameter");
        log.record_mpv_message("warn", "cplayer", "a warning");

        let file = std::fs::read_to_string(&path).unwrap();
        let captured = sink.lines();
        assert_eq!(captured, file.lines().collect::<Vec<_>>());
        assert!(file.contains("info: one"));
        assert!(file.contains("warn: two"));
        assert!(file.contains("command: seek 1.000 absolute"));
        assert!(file.contains("command failed: seek 1.000 absolute: invalid parameter"));
        assert!(file.contains("property write: volume = NaN"));
        assert!(file.contains("property write failed: volume = NaN: invalid parameter"));
        assert!(file.contains("mpv: warn cplayer: a warning"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_disabled_config_writes_neither_sink() {
        let (_log, sink, path) = testing::log_to_file("off");
        std::fs::remove_file(&path).unwrap();
        let config = LogConfig::resolve(Some(OsString::from("off")), None, None);
        let log = RunLog::from_config(config, sink.clone());
        log.info("gone");
        assert!(sink.lines().is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn the_file_is_truncated_at_start() {
        let path = testing::temp_path("truncate");
        std::fs::write(&path, "stale line\n").unwrap();
        let sink = testing::CaptureSink::new();
        let log = RunLog::from_config(
            LogConfig {
                path: Some(path.clone()),
                stderr: true,
            },
            sink.clone(),
        );
        log.info("fresh");
        let file = std::fs::read_to_string(&path).unwrap();
        assert!(!file.contains("stale line"), "{file}");
        assert!(file.contains("info: fresh"), "{file}");
        assert_eq!(sink.lines(), vec!["info: fresh"]);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_file_stops_at_the_cap_with_one_cap_line() {
        let (log, sink, path) = testing::log_to_file_with_limit("cap", 120);
        for index in 0..20 {
            log.info(format!("line {index:02}"));
        }
        let file = std::fs::read_to_string(&path).unwrap();
        let cap_lines = file.lines().filter(|line| line.contains("capped")).count();
        assert_eq!(cap_lines, 1, "{file}");
        assert!(file.contains("capped at 120 bytes"), "{file}");
        assert!(!file.contains("info: line 19"), "{file}");
        assert!(
            std::fs::metadata(&path).unwrap().len() <= 120,
            "the file must not grow past the cap"
        );
        assert!(
            sink.lines().iter().any(|line| line == "info: line 19"),
            "stderr keeps receiving lines after the file cap"
        );
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    pub struct CaptureSink {
        lines: Mutex<Vec<String>>,
    }

    impl CaptureSink {
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                lines: Mutex::new(Vec::new()),
            })
        }

        pub fn lines(&self) -> Vec<String> {
            self.lines.lock().unwrap().clone()
        }
    }

    impl LogSink for CaptureSink {
        fn write_line(&self, line: &str) {
            self.lines.lock().unwrap().push(line.to_string());
        }
    }

    pub fn temp_path(name: &str) -> PathBuf {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "kachito-log-{name}-{}-{id}.log",
            std::process::id()
        ))
    }

    pub fn log_to_file(name: &str) -> (Arc<RunLog>, Arc<CaptureSink>, PathBuf) {
        log_to_file_with_limit(name, MAX_FILE_BYTES)
    }

    pub fn log_to_file_with_limit(
        name: &str,
        limit: u64,
    ) -> (Arc<RunLog>, Arc<CaptureSink>, PathBuf) {
        let path = temp_path(name);
        let sink = CaptureSink::new();
        let log = RunLog::with_limit(
            LogConfig {
                path: Some(path.clone()),
                stderr: true,
            },
            sink.clone(),
            limit,
        );
        (Arc::new(log), sink, path)
    }
}
