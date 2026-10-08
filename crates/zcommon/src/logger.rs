#[cfg(feature = "gui")]
use eframe::egui::{self, ScrollArea};
use log::{Metadata, Record, SetLoggerError};
use std::collections::VecDeque;
use std::env;
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

/// Loads `.env` from the cwd (real environment variables win), then installs the [`LogCollector`].
pub fn init_logging() -> Result<LogBuffer, SetLoggerError> {
    dotenvy::dotenv().ok();
    LogCollector::init()
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

        let buffer = LogBuffer::default();

        let collector = LogCollector {
            buffer: buffer.clone(),
            delegate: Box::new(env_logger),
        };

        // Set our collector as the logger
        log::set_boxed_logger(Box::new(collector))?;
        log::set_max_level(log::LevelFilter::Trace);

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

#[cfg(feature = "gui")]
pub fn ui_log_window(ui: &mut egui::Ui, log_buffer: LogBuffer, scroll_to_bottom: &mut bool) {
    // Lock and clone logs for UI rendering
    let logs = {
        let buf = log_buffer.lock().unwrap();
        buf.clone()
    };

    // ScrollArea with vertical scrollbar and full size
    let scroll_response = ScrollArea::vertical()
        .auto_shrink([false; 2]) // Don't shrink smaller than contents
        .stick_to_bottom(*scroll_to_bottom)
        .show(ui, |ui| {
            // Fill available width & stretch height as needed
            ui.vertical(|ui| {
                for line in logs {
                    ui.label(line);
                }
            });
        });

    let has_scroll = scroll_response.content_size.y > scroll_response.inner_rect.size().y;

    if has_scroll {
        let scroll_pos = ui.ctx().input(|input| input.raw_scroll_delta.y);
        if scroll_pos > 0.0 {
            *scroll_to_bottom = false;
        } else if scroll_pos < 0.0 {
            *scroll_to_bottom = true;
        }
    } else {
        *scroll_to_bottom = true;
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
    fn log_buffer_cap_is_ten_thousand() {
        assert_eq!(LOG_BUFFER_CAP, 10_000);
    }
}
