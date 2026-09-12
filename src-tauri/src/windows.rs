use crate::logger;
use serde::Serialize;
use tauri::{
    AppHandle, Manager, PhysicalPosition, PhysicalSize, Position, Size, WebviewUrl,
    WebviewWindow, WebviewWindowBuilder,
};

pub const DISPLAY_LABEL: &str = "display";
pub const CONTROLLER_LABEL: &str = "controller";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorInfo {
    pub index: usize,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub is_primary: bool,
    pub scale_factor: f64,
    pub x: i32,
    pub y: i32,
}

pub fn list_monitors(app: &AppHandle) -> Result<Vec<MonitorInfo>, String> {
    let monitors = app.available_monitors().map_err(|e| e.to_string())?;
    let primary_pos = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| *m.position());

    Ok(monitors
        .into_iter()
        .enumerate()
        .map(|(index, monitor)| {
            let pos = *monitor.position();
            let size = *monitor.size();
            let is_primary = primary_pos.map(|p| p == pos).unwrap_or(false);
            MonitorInfo {
                index,
                name: monitor
                    .name()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("Monitor {}", index + 1)),
                width: size.width,
                height: size.height,
                is_primary,
                scale_factor: monitor.scale_factor(),
                x: pos.x,
                y: pos.y,
            }
        })
        .collect())
}

pub fn pick_output_monitor(
    app: &AppHandle,
    preferred_index: Option<usize>,
) -> Option<tauri::Monitor> {
    let monitors = app.available_monitors().ok()?;
    if monitors.is_empty() {
        return app.primary_monitor().ok().flatten();
    }

    if let Some(index) = preferred_index {
        if let Some(monitor) = monitors.get(index) {
            return Some(monitor.clone());
        }
    }

    let primary_pos = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| *m.position());

    for monitor in &monitors {
        if primary_pos.map(|p| *monitor.position() != p).unwrap_or(true) {
            return Some(monitor.clone());
        }
    }
    app.primary_monitor().ok().flatten()
}

/// Opens the projector window if missing, places it on the preferred monitor.
pub fn ensure_display(app: &AppHandle, preferred_index: Option<usize>) -> tauri::Result<()> {
    if app.get_webview_window(DISPLAY_LABEL).is_some() {
        logger::info("display", "ensure_display: окно уже есть, переставляем");
        return place_display(app, preferred_index);
    }
    logger::info("display", "ensure_display: создаём окно вывода");
    open_display(app, preferred_index)
}

/// Opens the projector window on the preferred / secondary monitor.
pub fn open_display(app: &AppHandle, preferred_index: Option<usize>) -> tauri::Result<()> {
    if app.get_webview_window(DISPLAY_LABEL).is_some() {
        return place_display(app, preferred_index);
    }

    let monitors = app.available_monitors().unwrap_or_default();
    let multi_monitor = monitors.len() >= 2;
    let output = pick_output_monitor(app, preferred_index);
    logger::info(
        "display",
        &format!(
            "open_display: мониторов {}, многомониторный {}, запрошенный {:?}",
            monitors.len(),
            multi_monitor,
            preferred_index
        ),
    );

    let window = WebviewWindowBuilder::new(
        app,
        DISPLAY_LABEL,
        WebviewUrl::App("display.html".into()),
    )
    .title("ChyguiSlide Display")
    .decorations(false)
    .resizable(false)
    .transparent(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .focused(false)
    .visible(false)
    .shadow(false)
    .build()?;

    if let Some(monitor) = output {
        let pos = monitor.position();
        let size = monitor.size();
        logger::info(
            "display",
            &format!(
                "размещение на мониторе {}x{} в {},{}",
                size.width, size.height, pos.x, pos.y
            ),
        );
        let _ = window.set_position(Position::Physical(PhysicalPosition {
            x: pos.x,
            y: pos.y,
        }));
        if multi_monitor {
            let _ = window.set_size(Size::Physical(PhysicalSize {
                width: size.width,
                height: size.height,
            }));
        } else {
            let _ = window.set_size(Size::Physical(PhysicalSize {
                width: 1280,
                height: 720,
            }));
        }
    }

    exclude_from_aero_peek(&window);
    let _ = window.set_cursor_visible(false);
    window.show()?;
    // Re-apply after show — DWM sometimes ignores attributes on hidden windows.
    exclude_from_aero_peek(&window);
    eprintln!("[display] window shown");
    logger::info("display", "окно вывода показано");
    Ok(())
}

pub fn close_display(app: &AppHandle) -> tauri::Result<()> {
    logger::info("display", "закрытие окна вывода");
    if let Some(window) = app.get_webview_window(DISPLAY_LABEL) {
        window.close()?;
    }
    Ok(())
}

pub fn place_display(app: &AppHandle, preferred_index: Option<usize>) -> tauri::Result<()> {
    let Some(window) = app.get_webview_window(DISPLAY_LABEL) else {
        return Ok(());
    };
    let monitors = app.available_monitors().unwrap_or_default();
    let multi_monitor = monitors.len() >= 2;
    if let Some(monitor) = pick_output_monitor(app, preferred_index) {
        let pos = monitor.position();
        let size = monitor.size();
        let _ = window.set_position(Position::Physical(PhysicalPosition {
            x: pos.x,
            y: pos.y,
        }));
        if multi_monitor {
            let _ = window.set_size(Size::Physical(PhysicalSize {
                width: size.width,
                height: size.height,
            }));
        }
    }
    exclude_from_aero_peek(&window);
    Ok(())
}

/// Prevent Windows Aero Peek from hiding/minimizing the Display window.
#[cfg(windows)]
fn exclude_from_aero_peek(window: &WebviewWindow) {
    // DWMWA_EXCLUDED_FROM_PEEK = 12
    const DWMWA_EXCLUDED_FROM_PEEK: u32 = 12;
    let Ok(hwnd) = window.hwnd() else {
        return;
    };
    let value: i32 = 1; // TRUE
    unsafe {
        windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute(
            hwnd.0 as _,
            DWMWA_EXCLUDED_FROM_PEEK,
            &value as *const i32 as *const _,
            std::mem::size_of::<i32>() as u32,
        );
    }
}

#[cfg(not(windows))]
fn exclude_from_aero_peek(_window: &WebviewWindow) {}
