use std::mem::size_of;
use std::process::Command;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT};
use windows::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_SETVERSION,
    NIN_SELECT, NOTIFYICONDATAW, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos, LoadImageW,
    SetForegroundWindow, TrackPopupMenu, HICON, IMAGE_ICON, LR_DEFAULTCOLOR, MF_SEPARATOR,
    MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_CONTEXTMENU, WM_LBUTTONUP, WM_RBUTTONUP,
};

use crate::{diagnostics, widget, winstr};

pub const WM_TRAYICON: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;

const TRAY_ID: u32 = 1;
const IDI_SMALL: usize = 108;
const CMD_RELOAD: u32 = 1001;
const CMD_OPEN_LOGS: u32 = 1002;
const CMD_QUIT: u32 = 1003;

static TRAY: Lazy<Mutex<Option<TrayIcon>>> = Lazy::new(|| Mutex::new(None));

struct TrayIcon {
    hwnd: usize,
    icon: Option<usize>,
}

pub fn init(hwnd: HWND, instance: HINSTANCE) {
    let mut guard = TRAY.lock().expect("tray mutex poisoned");
    if guard.is_some() {
        return;
    }

    let icon = unsafe {
        LoadImageW(
            instance,
            PCWSTR(IDI_SMALL as *const u16),
            IMAGE_ICON,
            16,
            16,
            LR_DEFAULTCOLOR,
        )
    }
    .ok()
    .map(|handle| handle.0 as usize);

    let mut data = notify_data(hwnd, icon);
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data).as_bool() };
    if !added {
        diagnostics::log("tray", "failed to add tray icon");
        if let Some(icon) = icon {
            unsafe {
                let _ = DestroyIcon(HICON(icon as *mut _));
            }
        }
        return;
    }

    unsafe {
        data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
    }

    *guard = Some(TrayIcon {
        hwnd: hwnd.0 as usize,
        icon,
    });
    diagnostics::log("tray", "tray icon added");
}

pub fn shutdown() {
    let Some(tray) = TRAY.lock().expect("tray mutex poisoned").take() else {
        return;
    };

    let data = notify_data(HWND(tray.hwnd as *mut _), tray.icon);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        if let Some(icon) = tray.icon {
            let _ = DestroyIcon(HICON(icon as *mut _));
        }
    }

    diagnostics::log("tray", "tray icon removed");
}

pub unsafe fn handle_message(hwnd: HWND, lp: LPARAM) {
    let event = (lp.0 as u32) & 0xffff;
    if event == NIN_SELECT
        || event == WM_LBUTTONUP
        || event == WM_RBUTTONUP
        || event == WM_CONTEXTMENU
    {
        show_menu(hwnd);
    }
}

fn notify_data(hwnd: HWND, icon: Option<usize>) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ID,
        uFlags: NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAYICON,
        ..Default::default()
    };

    if let Some(icon) = icon {
        data.uFlags |= NIF_ICON;
        data.hIcon = HICON(icon as *mut _);
    }

    let tip = winstr::wide("Claudometer");
    let len = tip.len().min(data.szTip.len());
    data.szTip[..len].copy_from_slice(&tip[..len]);

    data
}

unsafe fn show_menu(hwnd: HWND) {
    let Ok(menu) = CreatePopupMenu() else {
        diagnostics::log("tray", "failed to create popup menu");
        return;
    };

    let reload = winstr::wide("Reload widget");
    let logs = winstr::wide("Open logs folder");
    let quit = winstr::wide("Quit");

    let _ = AppendMenuW(
        menu,
        MF_STRING,
        CMD_RELOAD as usize,
        PCWSTR(reload.as_ptr()),
    );
    let _ = AppendMenuW(
        menu,
        MF_STRING,
        CMD_OPEN_LOGS as usize,
        PCWSTR(logs.as_ptr()),
    );
    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
    let _ = AppendMenuW(menu, MF_STRING, CMD_QUIT as usize, PCWSTR(quit.as_ptr()));

    let mut point = POINT::default();
    if GetCursorPos(&mut point).is_err() {
        let _ = DestroyMenu(menu);
        diagnostics::log("tray", "failed to read cursor position");
        return;
    }

    let _ = SetForegroundWindow(hwnd);
    let command = TrackPopupMenu(
        menu,
        TPM_RETURNCMD | TPM_RIGHTBUTTON,
        point.x,
        point.y,
        0,
        hwnd,
        None,
    )
    .0 as u32;
    let _ = DestroyMenu(menu);

    match command {
        CMD_RELOAD => widget::reload_all(),
        CMD_OPEN_LOGS => open_logs_folder(),
        CMD_QUIT => {
            shutdown();
            widget::destroy_all();
        }
        _ => {}
    }
}

fn open_logs_folder() {
    let Some(folder) = diagnostics::log_folder() else {
        diagnostics::log("tray", "open logs failed no folder");
        return;
    };

    diagnostics::log(
        "tray",
        format!("open logs folder path={}", folder.display()),
    );
    if let Err(error) = Command::new("explorer").arg(&folder).spawn() {
        diagnostics::log("tray", format!("open logs failed error={error}"));
    }
}
