use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::panic;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use once_cell::sync::Lazy;

const LOG_FILE_NAME: &str = "claudometer.log";
const OLD_LOG_FILE_NAME: &str = "claudometer.log.old";
const LOG_DIR_NAME: &str = "logs";
const MAX_LOG_BYTES: u64 = 1024 * 1024;

static LOGGER: Lazy<Mutex<Logger>> = Lazy::new(|| Mutex::new(Logger::new()));
static LOGGER_INITIALIZED: AtomicBool = AtomicBool::new(false);
static EXCEPTION_FILTER_INSTALLED: AtomicBool = AtomicBool::new(false);
static PANIC_HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

pub fn init() {
    if LOGGER_INITIALIZED.swap(true, Ordering::SeqCst) {
        return;
    }

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

pub fn install_panic_hook() {
    if PANIC_HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }

    let default_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let payload = info
            .payload()
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
            .unwrap_or("<non-string panic payload>");
        let location = info
            .location()
            .map(|location| {
                format!(
                    "{}:{}:{}",
                    location.file(),
                    location.line(),
                    location.column()
                )
            })
            .unwrap_or_else(|| "<unknown>".to_owned());

        log(
            "panic",
            format!(
                "process panic pid={} thread={:?} location={} message={}",
                std::process::id(),
                std::thread::current().id(),
                location,
                payload
            ),
        );

        default_hook(info);
    }));
}

#[cfg(target_os = "windows")]
pub fn install_exception_filter() {
    if EXCEPTION_FILTER_INSTALLED.swap(true, Ordering::SeqCst) {
        return;
    }

    unsafe {
        windows::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(
            unhandled_exception_filter,
        ));
    }
}

#[cfg(target_os = "windows")]
unsafe extern "system" fn unhandled_exception_filter(
    exception_info: *const windows::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    let Some(exception_info) = (unsafe { exception_info.as_ref() }) else {
        log("crash", "unhandled exception with no exception info");
        return windows::Win32::System::Diagnostics::Debug::EXCEPTION_CONTINUE_SEARCH;
    };
    let Some(record) = (unsafe { exception_info.ExceptionRecord.as_ref() }) else {
        log("crash", "unhandled exception with no exception record");
        return windows::Win32::System::Diagnostics::Debug::EXCEPTION_CONTINUE_SEARCH;
    };

    log(
        "crash",
        format!(
            "unhandled exception pid={} code=0x{:08x} flags=0x{:08x} address={:?}",
            std::process::id(),
            record.ExceptionCode.0 as u32,
            record.ExceptionFlags,
            record.ExceptionAddress
        ),
    );

    windows::Win32::System::Diagnostics::Debug::EXCEPTION_CONTINUE_SEARCH
}

pub fn log(source: &str, message: impl AsRef<str>) {
    let Ok(mut logger) = LOGGER.lock() else {
        return;
    };
    logger.write(source, message.as_ref());
}

pub fn log_folder() -> Option<PathBuf> {
    LOGGER
        .lock()
        .expect("logger mutex poisoned")
        .path
        .as_ref()
        .and_then(|path| path.parent().map(|parent| parent.to_path_buf()))
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|path| path.parent().map(|parent| parent.join(LOG_DIR_NAME)))
        })
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
    let log_dir = dir.join(LOG_DIR_NAME);
    let _ = fs::create_dir_all(&log_dir);
    Some(log_dir.join(LOG_FILE_NAME))
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
