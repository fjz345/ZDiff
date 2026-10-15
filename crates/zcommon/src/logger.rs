use log::{Metadata, Record, SetLoggerError};
use std::collections::VecDeque;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub const DEFAULT_LOG_FILTER: &str = "info,wgpu=warn,naga=warn";
pub const LOG_BUFFER_CAP: usize = 10_000;

pub type LogBuffer = Arc<Mutex<VecDeque<String>>>;

static GLOBAL_LOG_BUFFER: OnceLock<LogBuffer> = OnceLock::new();

pub fn log_filter(rust_log: Option<&str>) -> &str {
    rust_log.unwrap_or(DEFAULT_LOG_FILTER)
}

fn push_capped(buf: &mut VecDeque<String>, entry: String, cap: usize) {
    while buf.len() >= cap {
        buf.pop_front();
    }
    buf.push_back(entry);
}

fn build_env_logger() -> env_logger::Logger {
    let mut builder = env_logger::Builder::new();
    builder.parse_filters(log_filter(env::var("RUST_LOG").ok().as_deref()));
    if let Ok(style) = env::var("RUST_LOG_STYLE") {
        builder.parse_write_style(&style);
    }
    builder.build()
}

pub fn env_file_path(exe: &Path) -> Option<PathBuf> {
    exe.parent().map(|dir| dir.join(".env"))
}

enum EnvLoad {
    Loaded(PathBuf),
    NoneFound,
    Unreadable(PathBuf, String),
    NoExe(String),
}

fn load_env_file() -> EnvLoad {
    let exe = match env::current_exe() {
        Ok(exe) => exe,
        Err(e) => return EnvLoad::NoExe(e.to_string()),
    };
    let Some(path) = env_file_path(&exe) else {
        return EnvLoad::NoExe("executable path has no parent".to_string());
    };
    if !path.is_file() {
        return EnvLoad::NoneFound;
    }
    match dotenvy::from_path(&path) {
        Ok(()) => EnvLoad::Loaded(path),
        Err(dotenvy::Error::Io(e)) => EnvLoad::Unreadable(path, e.to_string()),
        Err(_) => EnvLoad::Unreadable(path, "malformed content".to_string()),
    }
}

/// Also loads `.env` from the executable's folder; variables already in the real environment win.
pub fn init_logging() -> Result<LogBuffer, SetLoggerError> {
    let loaded = load_env_file();
    let buffer = LogCollector::init()?;
    match loaded {
        EnvLoad::Loaded(path) => log::info!("Loaded env file {}", path.display()),
        EnvLoad::NoneFound => log::info!("No .env file found next to the executable"),
        EnvLoad::Unreadable(path, why) => {
            log::error!("Skipped unreadable env file {}: {}", path.display(), why)
        }
        EnvLoad::NoExe(why) => {
            log::error!("Cannot resolve executable path, no .env loaded: {}", why)
        }
    }
    Ok(buffer)
}

pub struct LogCollector {
    pub buffer: LogBuffer,
    delegate: Box<dyn log::Log>,
}

impl Default for LogCollector {
    fn default() -> Self {
        Self {
            buffer: Default::default(),
            delegate: Box::new(build_env_logger()),
        }
    }
}

impl LogCollector {
    pub fn init() -> Result<LogBuffer, SetLoggerError> {
        if let Some(buffer) = GLOBAL_LOG_BUFFER.get() {
            return Ok(buffer.clone());
        }

        let env_logger = build_env_logger();
        let max_level = env_logger.filter();

        let buffer = LogBuffer::default();

        let collector = LogCollector {
            buffer: buffer.clone(),
            delegate: Box::new(env_logger),
        };

        // Set our collector as the logger
        log::set_boxed_logger(Box::new(collector))?;
        log::set_max_level(max_level);

        GLOBAL_LOG_BUFFER.set(buffer.clone()).unwrap();

        Ok(buffer)
    }
}

impl log::Log for LogCollector {
    fn enabled(&self, metadata: &Metadata) -> bool {
        self.delegate.enabled(metadata)
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            // Forward to env_logger
            self.delegate.log(record);

            // Capture in our buffer
            let mut buf = self.buffer.lock().unwrap();
            push_capped(
                &mut buf,
                format!("[{}] {}", record.level(), record.args()),
                LOG_BUFFER_CAP,
            );
        }
    }

    fn flush(&self) {
        self.delegate.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_filter_unset_gives_default() {
        assert_eq!(log_filter(None), DEFAULT_LOG_FILTER);
        assert_eq!(DEFAULT_LOG_FILTER, "info,wgpu=warn,naga=warn");
    }

    #[test]
    fn log_filter_set_wins() {
        assert_eq!(log_filter(Some("debug")), "debug");
    }

    #[test]
    fn push_capped_drops_oldest_and_keeps_order() {
        let mut buf = VecDeque::new();
        for i in 0..5 {
            push_capped(&mut buf, i.to_string(), 3);
        }
        assert_eq!(buf, ["2", "3", "4"]);
    }

    #[test]
    fn env_file_is_in_exe_dir() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        assert_eq!(env_file_path(&exe), Some(dir.path().join(".env")));
    }

    #[test]
    fn env_file_ignores_cwd_and_its_parent() {
        let cwd = tempfile::tempdir().unwrap();
        let child = cwd.path().join("child");
        let exe_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(&child).unwrap();
        std::fs::write(cwd.path().join(".env"), "A=1").unwrap();
        std::fs::write(child.join(".env"), "A=1").unwrap();
        let chosen = env_file_path(&exe_dir.path().join("app.exe")).unwrap();
        assert_eq!(chosen, exe_dir.path().join(".env"));
    }

    #[test]
    fn env_file_none_without_parent() {
        assert_eq!(env_file_path(Path::new("")), None);
    }

    #[test]
    fn from_path_does_not_override_existing_vars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        std::fs::write(
            &path,
            "ZCOMMON_TEST_ENV_KEEP=file\nZCOMMON_TEST_ENV_NEW=file\n",
        )
        .unwrap();
        unsafe { env::set_var("ZCOMMON_TEST_ENV_KEEP", "real") };
        dotenvy::from_path(&path).unwrap();
        assert_eq!(env::var("ZCOMMON_TEST_ENV_KEEP").unwrap(), "real");
        assert_eq!(env::var("ZCOMMON_TEST_ENV_NEW").unwrap(), "file");
    }

    #[test]
    fn log_buffer_cap_is_ten_thousand() {
        assert_eq!(LOG_BUFFER_CAP, 10_000);
    }
}
