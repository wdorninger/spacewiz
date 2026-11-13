//! Log file in logs/spacewiz.log (plus the console when started with --console).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;

use log::{Level, LevelFilter, Log, Metadata, Record};

struct Logger {
    file: Mutex<Option<File>>,
    console: bool,
    own_level: LevelFilter,
}

impl Log for Logger {
    fn enabled(&self, m: &Metadata) -> bool {
        // Our own messages at INFO, third-party crates only when something goes wrong.
        if m.target().starts_with(env!("CARGO_CRATE_NAME")) {
            m.level() <= self.own_level
        } else {
            m.level() <= Level::Warn
        }
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {} {}\n",
            chrono::Local::now().format("%Y-%m-%d %H:%M:%S,%3f"),
            record.level(),
            record.args()
        );
        if let Ok(mut f) = self.file.lock() {
            if let Some(f) = f.as_mut() {
                let _ = f.write_all(line.as_bytes());
            }
        }
        if self.console {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            if let Some(f) = f.as_mut() {
                let _ = f.flush();
            }
        }
    }
}

pub fn init(console: bool) {
    let dir = crate::config::root().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let file = OpenOptions::new().create(true).append(true).open(dir.join("spacewiz.log")).ok();
    let own_level = if std::env::var_os("SPACEWIZ_DEBUG").is_some() { LevelFilter::Debug } else { LevelFilter::Info };
    let logger = Logger { file: Mutex::new(file), console, own_level };
    if log::set_boxed_logger(Box::new(logger)).is_ok() {
        log::set_max_level(LevelFilter::Debug);
    }
}
