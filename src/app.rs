use windows::core::PCWSTR;
use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, OpenInputDesktop, SwitchDesktop, DESKTOP_CONTROL_FLAGS, DESKTOP_SWITCHDESKTOP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetMessageW, LoadCursorW, RegisterClassW, TranslateMessage, HCURSOR,
    IDC_ARROW, MSG, WNDCLASSW,
};

use crate::{drawing, taskbar, tray, usage, widget, winstr};

pub fn run() -> windows::core::Result<()> {
    crate::diagnostics::init();
    crate::diagnostics::log("app", "startup");

    unsafe {
        let module = GetModuleHandleW(None)?;
        let instance = HINSTANCE(module.0);
        let class_name = winstr::wide("ClaudometerOverlay");
        crate::diagnostics::log("app", "registering window class");

        let wc = WNDCLASSW {
            hInstance: instance,
            lpszClassName: PCWSTR(class_name.as_ptr()),
            lpfnWndProc: Some(widget::wnd_proc),
            hCursor: HCURSOR(LoadCursorW(None, IDC_ARROW)?.0),
            ..Default::default()
        };
        RegisterClassW(&wc);

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
        while GetMessageW(&mut msg, None, 0, 0).into() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    crate::diagnostics::log("app", "message loop exited");
    Ok(())
}

pub fn shutdown() {
    crate::diagnostics::log("app", "shutdown");
    tray::shutdown();
    usage::shutdown();
    drawing::shutdown();
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
