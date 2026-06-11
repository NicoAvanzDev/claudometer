use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HINSTANCE};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, SwitchDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_SWITCHDESKTOP,
};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, LoadCursorW, LoadImageW, RegisterClassW, TranslateMessage,
    HCURSOR, HICON, IDC_ARROW, IMAGE_ICON, LR_DEFAULTCOLOR, MSG, WNDCLASSW,
};

use crate::{drawing, taskbar, tray, usage, widget, winstr};

const IDI_APP_32: usize = 110;

pub fn run() -> windows::core::Result<()> {
    crate::diagnostics::init();
    crate::diagnostics::log("app", "startup");
    let Some(single_instance_lock) = SingleInstanceLock::acquire()? else {
        return Ok(());
    };

    let module = unsafe { GetModuleHandleW(None)? };
    let instance = HINSTANCE(module.0);
    let class_name = winstr::wide("ClaudometerOverlay");
    crate::diagnostics::log("app", "registering window class");

    let wc = WNDCLASSW {
        hInstance: instance,
        hIcon: HICON(
            unsafe {
                LoadImageW(
                    instance,
                    PCWSTR(IDI_APP_32 as *const u16),
                    IMAGE_ICON,
                    32,
                    32,
                    LR_DEFAULTCOLOR,
                )?
            }
            .0,
        ),
        lpszClassName: PCWSTR(class_name.as_ptr()),
        lpfnWndProc: Some(widget::wnd_proc),
        hCursor: HCURSOR(unsafe { LoadCursorW(None, IDC_ARROW)? }.0),
        ..Default::default()
    };
    unsafe {
        RegisterClassW(&wc);
    }

    drawing::init(instance)?;
    crate::diagnostics::log("app", "drawing initialized");

    let taskbars = taskbar::find_taskbars();
    crate::diagnostics::log("app", format!("taskbars found count={}", taskbars.len()));
    for taskbar in taskbars {
        widget::create_for_taskbar(taskbar, PCWSTR(class_name.as_ptr()), instance);
    }

    if !widget::has_widgets() {
        crate::diagnostics::log("app", "startup failed no widgets created");
        return Err(windows::core::Error::from_win32());
    }

    if let Some(hwnd) = widget::primary_widget_hwnd() {
        tray::init(hwnd, instance);
    }

    crate::diagnostics::log("app", "starting initial usage fetch");
    usage::start_fetch_if_due(true);

    let mut msg = MSG::default();
    while unsafe { GetMessageW(&mut msg, None, 0, 0) }.into() {
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    crate::diagnostics::log("app", "message loop exited");
    drop(single_instance_lock);
    Ok(())
}

struct SingleInstanceLock(windows::Win32::Foundation::HANDLE);

impl SingleInstanceLock {
    fn acquire() -> windows::core::Result<Option<Self>> {
        let name = winstr::wide("Local\\ClaudometerSingleInstance");
        let handle = unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr()))? };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            crate::diagnostics::log("app", "startup skipped existing instance");
            unsafe {
                let _ = CloseHandle(handle);
            }
            return Ok(None);
        }

        Ok(Some(Self(handle)))
    }
}

impl Drop for SingleInstanceLock {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

pub fn shutdown() {
    crate::diagnostics::log("app", "shutdown");
    tray::shutdown();
    usage::shutdown();
    drawing::shutdown();
}

pub fn is_workstation_locked() -> bool {
    let Ok(desktop) =
        (unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_SWITCHDESKTOP) })
    else {
        return true;
    };

    let switchable = unsafe { SwitchDesktop(desktop) }.is_ok();
    let _ = unsafe { CloseDesktop(desktop) };
    !switchable
}
