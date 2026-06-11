use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetClientRect, GetParent, GetWindowLongPtrW,
    KillTimer, PostQuitMessage, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW,
    SetWindowPos, GWL_EXSTYLE, HWND_TOP, LWA_ALPHA, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    WINDOW_EX_STYLE, WM_DESTROY, WM_ERASEBKGND, WM_NCCREATE, WM_PAINT, WM_TIMER,
    WM_WTSSESSION_CHANGE, WS_CHILD, WS_EX_LAYERED, WS_EX_NOACTIVATE, WTS_SESSION_UNLOCK,
};

use crate::{drawing, usage, winstr};

pub const TIMER_ID: usize = 1;
pub const WM_USAGE_UPDATED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
pub const WIDGET_WIDTH: i32 = 164;
pub const WIDGET_HEIGHT: i32 = 36;
pub const SESSION_ROW_TOP: f32 = -0.5;
pub const WEEKLY_ROW_TOP: f32 = 18.0;

static WIDGETS: Lazy<Mutex<Vec<usize>>> = Lazy::new(|| Mutex::new(Vec::new()));
static WIDGET_COUNT: AtomicI32 = AtomicI32::new(0);

pub fn create_for_taskbar(taskbar: HWND, class_name: PCWSTR, instance: HINSTANCE) {
    let title = winstr::wide("Claudometer");
    // Created as a child of the taskbar itself: the widget shares the
    // taskbar's z-order, so the shell raising the taskbar can never cover it.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_NOACTIVATE.0),
            class_name,
            winstr::pcwstr(&title),
            WS_CHILD,
            0,
            0,
            WIDGET_WIDTH,
            WIDGET_HEIGHT,
            taskbar,
            None,
            instance,
            None,
        )
    };

    let Ok(hwnd) = hwnd else {
        return;
    };

    WIDGETS
        .lock()
        .expect("widgets mutex poisoned")
        .push(hwnd.0 as usize);
    WIDGET_COUNT.fetch_add(1, Ordering::SeqCst);

    unsafe {
        // WS_EX_LAYERED, applied post-creation (CreateWindowExW rejects it
        // for cross-process children): a plain child renders into the
        // taskbar's legacy surface, which Windows 11 composites *below* the
        // XAML content covering the whole bar. A layered child gets its own
        // surface that DWM stacks above it.
        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex_style | WS_EX_LAYERED.0 as isize);
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA);
        let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        let _ = SetTimer(hwnd, TIMER_ID, usage::TIMER_INTERVAL_MS, None);
    }
    position_over_taskbar(hwnd, taskbar);
}

pub fn has_widgets() -> bool {
    WIDGET_COUNT.load(Ordering::SeqCst) > 0
}

pub fn widget_hwnds() -> Vec<HWND> {
    WIDGETS
        .lock()
        .expect("widgets mutex poisoned")
        .iter()
        .filter_map(|hwnd| {
            let hwnd = HWND(*hwnd as *mut _);
            (!hwnd.0.is_null()).then_some(hwnd)
        })
        .collect()
}

pub fn primary_widget_hwnd() -> Option<HWND> {
    widget_hwnds().into_iter().next()
}

pub fn reload_all() {
    crate::diagnostics::log("widget", "reload requested");

    for ptr in WIDGETS
        .lock()
        .expect("widgets mutex poisoned")
        .iter()
        .copied()
        .collect::<Vec<_>>()
    {
        let hwnd = HWND(ptr as *mut _);
        if hwnd.0.is_null() {
            continue;
        }

        position_over_parent_taskbar(hwnd);
        drawing::discard_window_resources(hwnd);
        unsafe {
            let _ = InvalidateRect(hwnd, None, true);
        }
    }

    usage::start_fetch_if_due(true);
}

pub fn destroy_all() {
    crate::diagnostics::log("widget", "destroy all requested");

    for hwnd in widget_hwnds() {
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

pub unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == crate::tray::WM_TRAYICON {
        crate::tray::handle_message(hwnd, lp);
        return LRESULT(0);
    }

    match msg {
        WM_NCCREATE => LRESULT(1),
        WM_TIMER => {
            if wp.0 == TIMER_ID {
                position_over_parent_taskbar(hwnd);
                usage::start_fetch_if_due(false);
            }
            LRESULT(0)
        }
        WM_WTSSESSION_CHANGE => {
            if wp.0 == WTS_SESSION_UNLOCK as usize {
                usage::start_fetch_if_due(true);
            }
            LRESULT(0)
        }
        WM_USAGE_UPDATED => {
            let _ = unsafe { InvalidateRect(hwnd, None, false) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(0),
        WM_PAINT => {
            drawing::paint(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = unsafe { KillTimer(hwnd, TIMER_ID) };
            let _ = unsafe { WTSUnRegisterSessionNotification(hwnd) };
            drawing::discard_window_resources(hwnd);
            WIDGETS
                .lock()
                .expect("widgets mutex poisoned")
                .retain(|item| *item != hwnd.0 as usize);

            if WIDGET_COUNT.fetch_sub(1, Ordering::SeqCst) == 1 {
                crate::app::shutdown();
                unsafe {
                    PostQuitMessage(0);
                }
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wp, lp) },
    }
}

fn position_over_parent_taskbar(hwnd: HWND) {
    let Ok(taskbar) = (unsafe { GetParent(hwnd) }) else {
        return;
    };
    position_over_taskbar(hwnd, taskbar);
}

fn position_over_taskbar(hwnd: HWND, taskbar: HWND) {
    // Child window: coordinates are relative to the taskbar's client area.
    let mut rc = RECT::default();
    let _ = unsafe { GetClientRect(taskbar, &mut rc) };

    let taskbar_width = rc.right;
    let taskbar_height = rc.bottom;
    let mut x = 8;
    let mut y = (taskbar_height - WIDGET_HEIGHT) / 2;

    if taskbar_width < taskbar_height {
        x = (taskbar_width - WIDGET_WIDTH) / 2;
        y = 8;
    }

    // HWND_TOP keeps the widget above the taskbar's own content (the XAML
    // island child covers the whole bar on Windows 11).
    let _ = unsafe {
        SetWindowPos(
            hwnd,
            HWND_TOP,
            x,
            y,
            WIDGET_WIDTH,
            WIDGET_HEIGHT,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    };
}
