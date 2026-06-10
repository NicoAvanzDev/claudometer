use std::ptr::null_mut;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

use once_cell::sync::Lazy;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::System::RemoteDesktop::{
    WTSRegisterSessionNotification, WTSUnRegisterSessionNotification, NOTIFY_FOR_THIS_SESSION,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, GetWindowLongPtrW, GetWindowRect, IsWindow, KillTimer,
    PostQuitMessage, SetTimer, SetWindowLongPtrW, SetWindowPos, CREATESTRUCTW, GWLP_USERDATA,
    HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOOWNERZORDER, SWP_NOZORDER, SWP_SHOWWINDOW, WINDOW_EX_STYLE,
    WM_DESTROY, WM_NCCREATE, WM_PAINT, WM_TIMER, WM_WTSSESSION_CHANGE, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WTS_SESSION_UNLOCK,
};

use crate::{drawing, usage, winstr};

pub const TIMER_ID: usize = 1;
const RESTACK_TIMER_FAST_ID: usize = 2;
const RESTACK_TIMER_SLOW_ID: usize = 3;
const RESTACK_FAST_INTERVAL_MS: u32 = 80;
const RESTACK_SLOW_INTERVAL_MS: u32 = 350;
pub const WM_USAGE_UPDATED: u32 = windows::Win32::UI::WindowsAndMessaging::WM_APP + 1;
pub const WIDGET_WIDTH: i32 = 164;
pub const WIDGET_HEIGHT: i32 = 36;
pub const SESSION_ROW_TOP: f32 = -0.5;
pub const WEEKLY_ROW_TOP: f32 = 18.0;

pub struct WidgetWindow {
    hwnd: HWND,
    taskbar: HWND,
}

static WIDGETS: Lazy<Mutex<Vec<usize>>> = Lazy::new(|| Mutex::new(Vec::new()));
static WIDGET_COUNT: AtomicI32 = AtomicI32::new(0);

pub fn create_for_taskbar(taskbar: HWND, class_name: PCWSTR, instance: HINSTANCE) {
    let widget = Box::new(WidgetWindow {
        hwnd: HWND(null_mut()),
        taskbar,
    });
    let widget_ptr = Box::into_raw(widget);

    let title = winstr::wide("Claude Code Usage");
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0),
            class_name,
            winstr::pcwstr(&title),
            WS_POPUP,
            0,
            0,
            WIDGET_WIDTH,
            WIDGET_HEIGHT,
            None,
            None,
            instance,
            Some(widget_ptr.cast()),
        )
    };

    let Ok(hwnd) = hwnd else {
        unsafe {
            drop(Box::from_raw(widget_ptr));
        }
        return;
    };

    unsafe {
        (*widget_ptr).hwnd = hwnd;
    }
    WIDGETS
        .lock()
        .expect("widgets mutex poisoned")
        .push(widget_ptr as usize);
    WIDGET_COUNT.fetch_add(1, Ordering::SeqCst);

    unsafe {
        position_over_taskbar(&*widget_ptr, true);
        let _ = WTSRegisterSessionNotification(hwnd, NOTIFY_FOR_THIS_SESSION);
        let _ = SetTimer(hwnd, TIMER_ID, usage::TIMER_INTERVAL_MS, None);
    }
}

pub fn has_widgets() -> bool {
    WIDGET_COUNT.load(Ordering::SeqCst) > 0
}

pub fn widget_hwnds() -> Vec<HWND> {
    WIDGETS
        .lock()
        .expect("widgets mutex poisoned")
        .iter()
        .filter_map(|ptr| {
            let widget = unsafe { &*(*ptr as *const WidgetWindow) };
            (!widget.hwnd.0.is_null()).then_some(widget.hwnd)
        })
        .collect()
}

pub fn restore_above_taskbars() {
    for ptr in WIDGETS
        .lock()
        .expect("widgets mutex poisoned")
        .iter()
        .copied()
    {
        let widget = unsafe { &*(ptr as *const WidgetWindow) };
        unsafe {
            if !widget.hwnd.0.is_null() && IsWindow(widget.hwnd).as_bool() {
                position_over_taskbar(widget, true);
            }
        }
    }
}

pub fn schedule_deferred_restacks() {
    for hwnd in widget_hwnds() {
        unsafe {
            let _ = SetTimer(hwnd, RESTACK_TIMER_FAST_ID, RESTACK_FAST_INTERVAL_MS, None);
            let _ = SetTimer(hwnd, RESTACK_TIMER_SLOW_ID, RESTACK_SLOW_INTERVAL_MS, None);
        }
    }
}

pub fn is_widget(hwnd: HWND) -> bool {
    WIDGETS
        .lock()
        .expect("widgets mutex poisoned")
        .iter()
        .any(|ptr| unsafe { (*(*ptr as *const WidgetWindow)).hwnd == hwnd })
}

pub unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_NCCREATE => {
            let create = &*(lp.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
            LRESULT(1)
        }
        WM_TIMER => {
            match wp.0 {
                TIMER_ID => usage::start_fetch_if_due(false),
                RESTACK_TIMER_FAST_ID | RESTACK_TIMER_SLOW_ID => {
                    let _ = KillTimer(hwnd, wp.0);
                    restore_above_taskbars();
                }
                _ => {}
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
            let _ = InvalidateRect(hwnd, None, true);
            LRESULT(0)
        }
        WM_PAINT => {
            drawing::paint(hwnd);
            LRESULT(0)
        }
        WM_DESTROY => {
            let _ = KillTimer(hwnd, TIMER_ID);
            let _ = WTSUnRegisterSessionNotification(hwnd);
            drawing::discard_window_resources(hwnd);

            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut WidgetWindow;
            if !ptr.is_null() {
                WIDGETS
                    .lock()
                    .expect("widgets mutex poisoned")
                    .retain(|item| *item != ptr as usize);
                drop(Box::from_raw(ptr));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }

            if WIDGET_COUNT.fetch_sub(1, Ordering::SeqCst) == 1 {
                crate::app::shutdown();
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe fn position_over_taskbar(widget: &WidgetWindow, restore_topmost: bool) {
    let mut rc = RECT::default();
    let _ = GetWindowRect(widget.taskbar, &mut rc);

    let taskbar_width = rc.right - rc.left;
    let taskbar_height = rc.bottom - rc.top;
    let mut x = rc.left + 8;
    let mut y = rc.top + ((taskbar_height - WIDGET_HEIGHT) / 2);

    if taskbar_width < taskbar_height {
        x = rc.left + ((taskbar_width - WIDGET_WIDTH) / 2);
        y = rc.top + 8;
    }

    let insert_after = if restore_topmost {
        HWND_TOPMOST
    } else {
        HWND(null_mut())
    };
    let flags = SWP_NOACTIVATE
        | SWP_NOOWNERZORDER
        | SWP_SHOWWINDOW
        | if restore_topmost {
            Default::default()
        } else {
            SWP_NOZORDER
        };

    let _ = SetWindowPos(
        widget.hwnd,
        insert_after,
        x,
        y,
        WIDGET_WIDTH,
        WIDGET_HEIGHT,
        flags,
    );
}
