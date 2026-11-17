//! Windows-11-Fensterdetails über DWM: dunkler Rahmen, runde Ecken, Mica-Hintergrund.

use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::Graphics::Dwm::{
    DWMSBT_MAINWINDOW, DWMWA_SYSTEMBACKDROP_TYPE, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

fn hwnd(window: &Window) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get() as HWND),
        _ => None,
    }
}

fn set(hwnd: HWND, attribute: i32, value: i32) {
    // SAFETY: gültiges Fenster-Handle, Wert ist ein 4-Byte-Integer wie von DWM erwartet.
    let hr = unsafe {
        DwmSetWindowAttribute(hwnd, attribute as _, &value as *const i32 as *const _, 4)
    };
    if hr != 0 {
        log::debug!("DwmSetWindowAttribute({attribute}) = {hr:#x}");
    }
}

/// Dunkler Modus, runde Ecken und Mica für das rahmenlose Hauptfenster.
pub fn style_main_window(window: &Window) {
    let Some(h) = hwnd(window) else { return };
    set(h, DWMWA_USE_IMMERSIVE_DARK_MODE as i32, 1);
    set(h, DWMWA_WINDOW_CORNER_PREFERENCE as i32, DWMWCP_ROUND as i32);
    set(h, DWMWA_SYSTEMBACKDROP_TYPE as i32, DWMSBT_MAINWINDOW as i32);
}
