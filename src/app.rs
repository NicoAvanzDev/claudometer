use std::ptr::null_mut;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, SwitchDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_SWITCHDESKTOP,
};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, LoadCursorW, RegisterClassW, TranslateMessage,
    EVENT_OBJECT_FOCUS, EVENT_SYSTEM_FOREGROUND, HCURSOR, IDC_ARROW, MSG, WINEVENT_OUTOFCONTEXT,
    WINEVENT_SKIPOWNPROCESS, WNDCLASSW,
};

use crate::{drawing, taskbar, usage, widget, winstr};

static WIN_EVENT_HOOKS: Lazy<Mutex<Vec<usize>>> = Lazy::new(|| Mutex::new(Vec::new()));

pub fn run() -> windows::core::Result<()> {
    unsafe {
        let module = GetModuleHandleW(None)?;
        let instance = HINSTANCE(module.0);
        let class_name = winstr::wide("MetricsTaskbarOverlay");

        let wc = WNDCLASSW {
            hInstance: instance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            lpfnWndProc: Some(widget::wnd_proc),
            hCursor: HCURSOR(LoadCursorW(None, IDC_ARROW)?.0),
            ..Default::default()
        };
        RegisterClassW(&wc);

        drawing::init(instance)?;

        for taskbar in taskbar::find_taskbars() {
            widget::create_for_taskbar(taskbar, PCWSTR(class_name.as_ptr()), instance);
        }

        if !widget::has_widgets() {
            return Err(windows::core::Error::from_win32());
        }

        install_win_event_hook(EVENT_SYSTEM_FOREGROUND, EVENT_SYSTEM_FOREGROUND);
        install_win_event_hook(EVENT_OBJECT_FOCUS, EVENT_OBJECT_FOCUS);

        usage::start_fetch_if_due(true);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

pub fn shutdown() {
    for hook in WIN_EVENT_HOOKS
        .lock()
        .expect("hook mutex poisoned")
        .drain(..)
    {
        unsafe {
            let _ = UnhookWinEvent(HWINEVENTHOOK(hook as *mut _));
        }
    }
    usage::shutdown();
    drawing::shutdown();
}

unsafe fn install_win_event_hook(event_min: u32, event_max: u32) {
    let hook = SetWinEventHook(
        event_min,
        event_max,
        None,
        Some(win_event_proc),
        0,
        0,
        WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
    );
    if !hook.0.is_null() {
        WIN_EVENT_HOOKS
            .lock()
            .expect("hook mutex poisoned")
            .push(hook.0 as usize);
    }
}

pub fn is_workstation_locked() -> bool {
    unsafe {
        let Ok(desktop) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP)
        else {
            return true;
        };

        let switchable = SwitchDesktop(desktop).is_ok();
        let _ = CloseDesktop(desktop);
        !switchable
    }
}

unsafe extern "system" fn win_event_proc(
    _: HWINEVENTHOOK,
    _: u32,
    hwnd: HWND,
    _: i32,
    _: i32,
    _: u32,
    _: u32,
) {
    if hwnd.0 != null_mut() && !widget::is_widget(hwnd) {
        widget::restore_above_taskbars();
        widget::schedule_deferred_restacks();
    }
}
