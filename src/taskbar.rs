use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, FindWindowW, GetClassNameW};

use crate::winstr;

pub fn find_taskbars() -> Vec<HWND> {
    let mut taskbars = Vec::<HWND>::new();

    unsafe {
        let _ = EnumWindows(
            Some(enum_taskbar_windows),
            LPARAM((&mut taskbars as *mut Vec<HWND>) as isize),
        );
    }

    if taskbars.is_empty() {
        let class = winstr::wide("Shell_TrayWnd");
        let hwnd = unsafe { FindWindowW(winstr::pcwstr(&class), None) };
        if let Ok(hwnd) = hwnd {
            taskbars.push(hwnd);
        }
    }

    taskbars
}

pub fn is_taskbar_window(hwnd: HWND) -> bool {
    class_name(hwnd)
        .map(|name| name == "Shell_TrayWnd" || name == "Shell_SecondaryTrayWnd")
        .unwrap_or(false)
}

unsafe extern "system" fn enum_taskbar_windows(hwnd: HWND, lp: LPARAM) -> BOOL {
    if is_taskbar_window(hwnd) {
        let taskbars = &mut *(lp.0 as *mut Vec<HWND>);
        taskbars.push(hwnd);
    }

    true.into()
}

fn class_name(hwnd: HWND) -> Option<String> {
    let mut buffer = [0u16; 64];
    let len = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if len == 0 {
        None
    } else {
        Some(String::from_utf16_lossy(&buffer[..len as usize]))
    }
}
