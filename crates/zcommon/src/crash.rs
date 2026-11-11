use std::{
    backtrace::Backtrace,
    fs::{self, OpenOptions},
    io::{self, Write},
    panic::PanicHookInfo,
    path::{Path, PathBuf},
    time::SystemTime,
};

pub fn install_crash_log(app_data_dir: Option<PathBuf>) -> Option<PathBuf> {
    let exe_dir = || {
        std::env::current_exe()
            .ok()?
            .parent()
            .map(Path::to_path_buf)
    };
    let path = app_data_dir.or_else(exe_dir)?.join("crash.log");
    let hook_path = path.clone();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let report = crash_report(info);
        if let Err(e) = append(&hook_path, &report) {
            log::error!("Cannot write {}: {e}", hook_path.display());
        }
        log::error!("{report}");
        previous(info);
    }));
    Some(path)
}

fn crash_report(info: &PanicHookInfo) -> String {
    let thread = std::thread::current();
    let unix_time = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    format!(
        "== unix time {unix_time}: thread '{}' {info}\nbacktrace:\n{}\n",
        thread.name().unwrap_or("<unnamed>"),
        Backtrace::force_capture()
    )
}

fn append(path: &Path, report: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(report.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn a_panic_is_appended_to_the_crash_log_and_the_previous_hook_still_runs() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let previous_ran = Arc::new(AtomicBool::new(false));
        let flag = previous_ran.clone();
        std::panic::set_hook(Box::new(move |_| flag.store(true, Ordering::SeqCst)));

        let path = install_crash_log(Some(data_dir.clone())).unwrap();
        for message in ["first crash", "second crash"] {
            let panicked = std::thread::Builder::new()
                .name("crasher".into())
                .spawn(move || panic!("{message}"))
                .unwrap()
                .join();
            assert!(panicked.is_err());
        }
        drop(std::panic::take_hook());

        assert_eq!(path, data_dir.join("crash.log"));
        let log = std::fs::read_to_string(&path).unwrap();
        let first = log.find("first crash").unwrap();
        let second = log.find("second crash").unwrap();
        assert!(first < second, "{log}");
        assert!(log.contains("thread 'crasher' panicked at "), "{log}");
        assert!(log.contains(file!()), "{log}");
        assert!(log.contains("backtrace:"), "{log}");
        assert!(previous_ran.load(Ordering::SeqCst));
    }
}
