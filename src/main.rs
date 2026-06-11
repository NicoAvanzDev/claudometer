#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(target_os = "windows")]
mod app;
#[cfg(target_os = "windows")]
mod credentials;
#[cfg(target_os = "windows")]
mod diagnostics;
#[cfg(target_os = "windows")]
mod drawing;
#[cfg(target_os = "windows")]
mod taskbar;
#[cfg(target_os = "windows")]
mod tray;
#[cfg(target_os = "windows")]
mod usage;
#[cfg(target_os = "windows")]
mod widget;
#[cfg(target_os = "windows")]
mod winstr;

#[cfg(target_os = "windows")]
fn main() {
    diagnostics::init();
    diagnostics::install_exception_filter();
    diagnostics::install_panic_hook();

    let result = std::panic::catch_unwind(app::run);
    match result {
        Ok(Ok(())) => diagnostics::log("app", "process exited normally"),
        Ok(Err(error)) => {
            diagnostics::log("app", format!("process exited with error error={error:?}"));
            shutdown_after_failure();
            std::process::exit(1);
        }
        Err(_) => {
            diagnostics::log("panic", "process terminated after panic reached main");
            shutdown_after_failure();
            std::process::exit(101);
        }
    }
}

#[cfg(target_os = "windows")]
fn shutdown_after_failure() {
    if std::panic::catch_unwind(app::shutdown).is_err() {
        diagnostics::log("panic", "shutdown panicked after process failure");
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("Claudometer is a native Windows taskbar overlay.");
}
