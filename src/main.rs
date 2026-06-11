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
fn main() -> windows::core::Result<()> {
    app::run()
}

#[cfg(not(target_os = "windows"))]
fn main() {
    eprintln!("Claudometer is a native Windows taskbar overlay.");
}
