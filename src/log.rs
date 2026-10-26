//! Leveled logger (XIRO_LOG=debug for debug output). Writes to the console and xiro.log,
//! rotated at 1 MB even while running so a long session can't fill the disk.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use windows::Win32::System::SystemInformation::GetLocalTime;

const MAX_LOG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO ",
            Level::Warn => "WARN ",
            Level::Error => "ERROR",
        }
    }
}

struct FileSink {
    path: PathBuf,
    file: Option<File>,
    size: u64,
}

impl FileSink {
    fn open(path: PathBuf) -> Self {
        let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        let mut sink = FileSink { path, file: None, size };
        if sink.size > MAX_LOG_BYTES {
            sink.rotate();
        }
        sink.file = OpenOptions::new().create(true).append(true).open(&sink.path).ok();
        sink
    }

    fn rotate(&mut self) {
        self.file = None;
        let old = self.path.with_extension("log.old");
        let _ = fs::remove_file(&old);
        let _ = fs::rename(&self.path, &old);
        self.size = 0;
        self.file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
    }

    fn write(&mut self, line: &str) {
        if self.size > MAX_LOG_BYTES {
            self.rotate();
        }
        // Disk full or locked: logging must never take the host down.
        if let Some(file) = &mut self.file {
            if file.write_all(line.as_bytes()).is_ok() {
                self.size += line.len() as u64;
            }
        }
    }
}

struct Logger {
    threshold: Level,
    console: bool,
    file: Option<FileSink>,
}

static LOGGER: Mutex<Logger> = Mutex::new(Logger { threshold: Level::Info, console: true, file: None });

/// Configures the sinks; call once at startup.
pub fn init(console: bool, file: Option<PathBuf>) {
    let threshold = match std::env::var("XIRO_LOG").as_deref() {
        Ok("debug") => Level::Debug,
        Ok("warn") => Level::Warn,
        Ok("error") => Level::Error,
        _ => Level::Info,
    };
    if let Ok(mut logger) = LOGGER.lock() {
        logger.threshold = threshold;
        logger.console = console;
        logger.file = file.map(FileSink::open);
    }
}

fn timestamp() -> String {
    let t = unsafe { GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds)
}

pub fn write(level: Level, args: fmt::Arguments) {
    let Ok(mut logger) = LOGGER.lock() else { return };
    if level < logger.threshold {
        return;
    }
    let line = format!("{} {} {args}\n", timestamp(), level.label());
    if logger.console {
        // The console shows time of day only.
        let short = &line[11..];
        if level >= Level::Warn {
            let _ = std::io::stderr().write_all(short.as_bytes());
        } else {
            let _ = std::io::stdout().write_all(short.as_bytes());
        }
    }
    if let Some(file) = &mut logger.file {
        file.write(&line);
    }
}

/// Console-only line without prefix, never written to the file (the pairing link holds the token).
pub fn raw(text: &str) {
    if LOGGER.lock().is_ok_and(|l| l.console) {
        let _ = writeln!(std::io::stdout(), "{text}");
    }
}

macro_rules! debug {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Debug, format_args!($($arg)*)) };
}
macro_rules! info {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Info, format_args!($($arg)*)) };
}
macro_rules! warning {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Warn, format_args!($($arg)*)) };
}
macro_rules! error {
    ($($arg:tt)*) => { $crate::log::write($crate::log::Level::Error, format_args!($($arg)*)) };
}
pub(crate) use {debug, error, info, warning};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_the_file_while_running() {
        let dir = std::env::temp_dir().join(format!("xiro-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("xiro.log");
        let mut sink = FileSink::open(path.clone());
        let line = format!("{}\n", "x".repeat(999));
        for _ in 0..2500 {
            sink.write(&line);
        }
        assert!(dir.join("xiro.log.old").exists(), "rotated during the run");
        assert!(fs::metadata(&path).unwrap().len() <= MAX_LOG_BYTES + 2048);
        assert!(fs::metadata(dir.join("xiro.log.old")).unwrap().len() <= MAX_LOG_BYTES + 2048);
    }

    #[test]
    fn levels_are_ordered_and_labelled() {
        assert!(Level::Debug < Level::Info && Level::Warn < Level::Error);
        assert_eq!(Level::Warn.label().trim(), "WARN");
        assert_eq!(timestamp().len(), 23);
    }
}
