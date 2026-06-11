use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;

const LOG_FILE_NAME: &str = "claudometer.log";
const OLD_LOG_FILE_NAME: &str = "claudometer.log.old";
const MAX_LOG_BYTES: u64 = 1024 * 1024;

static LOGGER: Lazy<Mutex<Logger>> = Lazy::new(|| Mutex::new(Logger::new()));

pub fn init() {
    let path = LOGGER.lock().expect("logger mutex poisoned").path.clone();

    log(
        "app",
        format!(
            "logger initialized version={} pid={} path={}",
            env!("CARGO_PKG_VERSION"),
            std::process::id(),
            display_path(path.as_ref())
        ),
    );
}

pub fn log(source: &str, message: impl AsRef<str>) {
    let Ok(mut logger) = LOGGER.lock() else {
        return;
    };
    logger.write(source, message.as_ref());
}

fn display_path(path: Option<&PathBuf>) -> String {
    path.map(|path| path.display().to_string())
        .unwrap_or_else(|| "<unavailable>".to_owned())
}

struct Logger {
    file: Option<File>,
    path: Option<PathBuf>,
}

impl Logger {
    fn new() -> Self {
        let Some(path) = log_path() else {
            return Self {
                file: None,
                path: None,
            };
        };

        rotate_log(&path);

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();

        Self {
            file,
            path: Some(path),
        }
    }

    fn write(&mut self, source: &str, message: &str) {
        let Some(file) = self.file.as_mut() else {
            return;
        };

        let _ = writeln!(
            file,
            "{} [{}] {}",
            timestamp(),
            source,
            message.replace(['\r', '\n'], " ")
        );
        let _ = file.flush();
    }
}

fn log_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    Some(dir.join(LOG_FILE_NAME))
}

fn rotate_log(path: &PathBuf) {
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.len() < MAX_LOG_BYTES {
        return;
    }

    let old_path = path.with_file_name(OLD_LOG_FILE_NAME);
    let _ = fs::remove_file(&old_path);
    let _ = fs::rename(path, old_path);
}

fn timestamp() -> String {
    let Ok(elapsed) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return "time=unavailable".to_owned();
    };

    format!("epoch_ms={}", elapsed.as_millis())
}
