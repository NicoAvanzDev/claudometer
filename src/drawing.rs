use std::collections::HashMap;
use std::ptr::null_mut;
use std::sync::Mutex;

use once_cell::sync::Lazy;
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{D2DERR_RECREATE_TARGET, ERROR_SUCCESS, HINSTANCE, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D1_ALPHA_MODE_UNKNOWN, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Factory, ID2D1HwndRenderTarget, ID2D1RenderTarget,
    ID2D1SolidColorBrush, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_FEATURE_LEVEL_DEFAULT, D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE,
    D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
    D2D1_ROUNDED_RECT,
};
use windows::Win32::Graphics::DirectWrite::{
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_UNKNOWN;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::Registry::{
    RegGetValueW, HKEY_CURRENT_USER, REG_VALUE_TYPE, RRF_RT_REG_DWORD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyIcon, DrawIconEx, GetClientRect, LoadImageW, DI_NORMAL, HICON, IMAGE_ICON,
    LR_DEFAULTCOLOR,
};

use crate::usage;
use crate::widget::{SESSION_ROW_TOP, WEEKLY_ROW_TOP};
use crate::winstr;

const IDI_WIDGET: usize = 112;
static GRAPHICS: Lazy<Mutex<Option<GraphicsContext>>> = Lazy::new(|| Mutex::new(None));

struct GraphicsContext {
    d2d_factory: ID2D1Factory,
    text_format: IDWriteTextFormat,
    small_text_format: IDWriteTextFormat,
    percent_text_format: IDWriteTextFormat,
    icon: Option<usize>,
    light_taskbar: bool,
    windows: HashMap<isize, WindowResources>,
}

struct WindowResources {
    target: ID2D1HwndRenderTarget,
    bg_color: D2D1_COLOR_F,
    text_brush: ID2D1SolidColorBrush,
    muted_text_brush: ID2D1SolidColorBrush,
    track_brush: ID2D1SolidColorBrush,
    session_brush: ID2D1SolidColorBrush,
    weekly_brush: ID2D1SolidColorBrush,
}

pub fn init(instance: HINSTANCE) -> windows::core::Result<()> {
    let mut guard = GRAPHICS.lock().expect("graphics mutex poisoned");
    if guard.is_some() {
        return Ok(());
    }

    let d2d_factory = unsafe { D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)? };
    let dwrite_factory: IDWriteFactory =
        unsafe { DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)? };

    let text_format = create_text_format(&dwrite_factory, 10.5, DWRITE_TEXT_ALIGNMENT_LEADING)?;
    let small_text_format =
        create_text_format(&dwrite_factory, 11.0, DWRITE_TEXT_ALIGNMENT_LEADING)?;
    let percent_text_format =
        create_text_format(&dwrite_factory, 10.5, DWRITE_TEXT_ALIGNMENT_TRAILING)?;

    let icon = unsafe {
        LoadImageW(
            instance,
            PCWSTR(IDI_WIDGET as *const u16),
            IMAGE_ICON,
            32,
            32,
            LR_DEFAULTCOLOR,
        )
    }
    .ok()
    .map(|handle| handle.0 as usize);

    *guard = Some(GraphicsContext {
        d2d_factory,
        text_format,
        small_text_format,
        percent_text_format,
        icon,
        light_taskbar: system_uses_light_theme(),
        windows: HashMap::new(),
    });

    Ok(())
}

pub fn shutdown() {
    if let Some(mut context) = GRAPHICS.lock().expect("graphics mutex poisoned").take() {
        context.windows.clear();
        if let Some(icon) = context.icon.take() {
            unsafe {
                let _ = DestroyIcon(HICON(icon as *mut _));
            }
        }
    }
}

pub fn discard_window_resources(hwnd: HWND) {
    if let Some(context) = GRAPHICS.lock().expect("graphics mutex poisoned").as_mut() {
        context.windows.remove(&(hwnd.0 as isize));
    }
}

pub fn paint(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let hdc = unsafe { BeginPaint(hwnd, &mut ps) };
    if hdc.0 == null_mut() {
        return;
    }

    let mut recreate_target = false;
    {
        let mut guard = GRAPHICS.lock().expect("graphics mutex poisoned");
        if let Some(context) = guard.as_mut() {
            match context.paint_d2d(hwnd) {
                Ok(()) => {}
                Err(error) if error.code() == D2DERR_RECREATE_TARGET => {
                    recreate_target = true;
                }
                Err(_) => {}
            }

            if !recreate_target {
                if let Some(icon) = context.icon {
                    let _ = unsafe {
                        DrawIconEx(hdc, 4, 2, HICON(icon as *mut _), 32, 32, 0, None, DI_NORMAL)
                    };
                }
            }
        }
    }

    if recreate_target {
        discard_window_resources(hwnd);
        let _ = unsafe { InvalidateRect(hwnd, None, false) };
    }

    let _ = unsafe { EndPaint(hwnd, &ps) };
}

impl GraphicsContext {
    fn paint_d2d(&mut self, hwnd: HWND) -> windows::core::Result<()> {
        let text_format = self.text_format.clone();
        let small_text_format = self.small_text_format.clone();
        let percent_text_format = self.percent_text_format.clone();
        let resources = self.resources_for(hwnd)?;
        let snapshot = usage::snapshot();

        unsafe {
            resources.target.BeginDraw();
        }

        let size = unsafe { resources.target.GetSize() };
        unsafe {
            resources.target.Clear(Some(&resources.bg_color));
        }

        if snapshot.ok {
            draw_usage_row(
                resources,
                &text_format,
                &percent_text_format,
                "5h",
                snapshot.session_percent,
                SESSION_ROW_TOP,
                &resources.session_brush,
                size.width,
            );
            draw_usage_row(
                resources,
                &text_format,
                &percent_text_format,
                "7d",
                snapshot.weekly_percent,
                WEEKLY_ROW_TOP,
                &resources.weekly_brush,
                size.width,
            );
        } else {
            let text = format!("Claude {}", snapshot.status);
            draw_text(
                &resources.target,
                &text,
                &small_text_format,
                rect(45.0, 0.0, size.width - 6.0, size.height),
                &resources.muted_text_brush,
            );
        }

        unsafe { resources.target.EndDraw(None, None) }
    }

    fn resources_for(&mut self, hwnd: HWND) -> windows::core::Result<&mut WindowResources> {
        let key = hwnd.0 as isize;
        if !self.windows.contains_key(&key) {
            let resources = create_window_resources(&self.d2d_factory, hwnd, self.light_taskbar)?;
            self.windows.insert(key, resources);
        }

        Ok(self
            .windows
            .get_mut(&key)
            .expect("window resources inserted"))
    }
}

fn create_text_format(
    factory: &IDWriteFactory,
    size: f32,
    alignment: windows::Win32::Graphics::DirectWrite::DWRITE_TEXT_ALIGNMENT,
) -> windows::core::Result<IDWriteTextFormat> {
    let face = winstr::wide("Segoe UI Variable");
    let locale = winstr::wide("en-us");

    let format = unsafe {
        factory.CreateTextFormat(
            PCWSTR(face.as_ptr()),
            None,
            DWRITE_FONT_WEIGHT_SEMI_BOLD,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            PCWSTR(locale.as_ptr()),
        )?
    };

    unsafe {
        format.SetTextAlignment(alignment)?;
        format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
    }

    Ok(format)
}

fn create_window_resources(
    factory: &ID2D1Factory,
    hwnd: HWND,
    light_taskbar: bool,
) -> windows::core::Result<WindowResources> {
    let mut rc = RECT::default();
    let _ = unsafe { GetClientRect(hwnd, &mut rc) };

    let render_target_properties = D2D1_RENDER_TARGET_PROPERTIES {
        r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_UNKNOWN,
            alphaMode: D2D1_ALPHA_MODE_UNKNOWN,
        },
        dpiX: 0.0,
        dpiY: 0.0,
        usage: D2D1_RENDER_TARGET_USAGE_NONE,
        minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
    };
    let hwnd_properties = D2D1_HWND_RENDER_TARGET_PROPERTIES {
        hwnd,
        pixelSize: D2D_SIZE_U {
            width: (rc.right - rc.left) as u32,
            height: (rc.bottom - rc.top) as u32,
        },
        presentOptions: D2D1_PRESENT_OPTIONS_NONE,
    };

    let target =
        unsafe { factory.CreateHwndRenderTarget(&render_target_properties, &hwnd_properties)? };
    let render_target: ID2D1RenderTarget = target.cast()?;
    let palette = Palette::for_taskbar(light_taskbar);

    Ok(WindowResources {
        bg_color: colorref(crate::widget::transparent_color()),
        text_brush: create_brush(
            &render_target,
            palette.text.0,
            palette.text.1,
            palette.text.2,
            1.0,
        )?,
        muted_text_brush: create_brush(
            &render_target,
            palette.muted_text.0,
            palette.muted_text.1,
            palette.muted_text.2,
            1.0,
        )?,
        track_brush: create_brush(
            &render_target,
            palette.track.0,
            palette.track.1,
            palette.track.2,
            1.0,
        )?,
        session_brush: create_brush(&render_target, 0.93, 0.53, 0.27, 1.0)?,
        weekly_brush: create_brush(&render_target, 0.36, 0.66, 0.95, 1.0)?,
        target,
    })
}

fn create_brush(
    target: &ID2D1RenderTarget,
    r: f32,
    g: f32,
    b: f32,
    a: f32,
) -> windows::core::Result<ID2D1SolidColorBrush> {
    unsafe { target.CreateSolidColorBrush(&D2D1_COLOR_F { r, g, b, a }, None) }
}

fn draw_usage_row(
    resources: &WindowResources,
    text_format: &IDWriteTextFormat,
    percent_format: &IDWriteTextFormat,
    label: &str,
    percent: i32,
    top: f32,
    fill_brush: &ID2D1SolidColorBrush,
    width: f32,
) {
    draw_text(
        &resources.target,
        label,
        text_format,
        rect(45.0, top, 74.0, top + 13.0),
        &resources.text_brush,
    );

    draw_text(
        &resources.target,
        &format!("{percent}%"),
        percent_format,
        rect(76.0, top, width - 8.0, top + 13.0),
        &resources.text_brush,
    );

    let track = rect(45.0, top + 14.5, width - 8.0, top + 17.5);
    fill_rounded(&resources.target, track, 1.5, &resources.track_brush);

    let clamped = percent.clamp(0, 100) as f32;
    let bar_width = (track.right - track.left) * (clamped / 100.0);
    if bar_width > 0.0 {
        fill_rounded(
            &resources.target,
            rect(track.left, track.top, track.left + bar_width, track.bottom),
            1.5,
            fill_brush,
        );
    }
}

fn draw_text(
    target: &ID2D1HwndRenderTarget,
    text: &str,
    format: &IDWriteTextFormat,
    layout: D2D_RECT_F,
    brush: &ID2D1SolidColorBrush,
) {
    let wide = winstr::wide(text);
    unsafe {
        target.DrawText(
            &wide[..wide.len().saturating_sub(1)],
            format,
            &layout,
            brush,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
            DWRITE_MEASURING_MODE_NATURAL,
        );
    }
}

fn fill_rounded(
    target: &ID2D1HwndRenderTarget,
    area: D2D_RECT_F,
    radius: f32,
    brush: &ID2D1SolidColorBrush,
) {
    unsafe {
        target.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: area,
                radiusX: radius,
                radiusY: radius,
            },
            brush,
        );
    }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

struct Palette {
    text: (f32, f32, f32),
    muted_text: (f32, f32, f32),
    track: (f32, f32, f32),
}

impl Palette {
    fn for_taskbar(light_taskbar: bool) -> Self {
        if light_taskbar {
            return Self {
                text: (0.08, 0.08, 0.08),
                muted_text: (0.32, 0.32, 0.32),
                track: (0.68, 0.68, 0.68),
            };
        }

        Self {
            text: (1.0, 1.0, 1.0),
            muted_text: (0.70, 0.70, 0.70),
            track: (0.24, 0.24, 0.24),
        }
    }
}

fn colorref(value: windows::Win32::Foundation::COLORREF) -> D2D1_COLOR_F {
    let raw = value.0;
    D2D1_COLOR_F {
        r: (raw & 0xff) as f32 / 255.0,
        g: ((raw >> 8) & 0xff) as f32 / 255.0,
        b: ((raw >> 16) & 0xff) as f32 / 255.0,
        a: 1.0,
    }
}

fn system_uses_light_theme() -> bool {
    let subkey = winstr::wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
    let value_name = winstr::wide("SystemUsesLightTheme");
    let mut value = 0u32;
    let mut size = std::mem::size_of_val(&value) as u32;

    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(value_name.as_ptr()),
            RRF_RT_REG_DWORD,
            None::<*mut REG_VALUE_TYPE>,
            Some((&mut value as *mut u32).cast()),
            Some(&mut size),
        )
    };

    status == ERROR_SUCCESS && value != 0
}
