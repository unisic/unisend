//! The count of waiting chats over the app's taskbar button: `ITaskbarList3::SetOverlayIcon`, as
//! Signal Desktop and Telegram show theirs, because an app without a package has no badge of its
//! own. The icon, a red circle with the number, is drawn here: GDI renders the digits as gray
//! coverage, and the circle and the alpha GDI cannot write are composed by hand. Windows' "Show
//! badges on taskbar buttons" setting hides it.
//!
//! Cost: one small-icon-sized bitmap (16 x 16 at 100 %) drawn and freed when the count changes.

use std::cell::Cell;
use std::ffi::c_void;
use std::time::Duration;

use gpui_kit::App;
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    ANTIALIASED_QUALITY, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CLIP_DEFAULT_PRECIS, CreateBitmap, CreateCompatibleDC, CreateDIBSection, CreateFontW, DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CENTER,
    DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW, FF_SWISS, FW_BOLD, GdiFlush, OUT_DEFAULT_PRECIS, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance};
use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, DestroyIcon, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON};
use windows::core::{HSTRING, PCWSTR, w};

use crate::i18n::tn;

/// The circle, as Windows' own red for errors and alerts.
const RED: [u8; 3] = [0xc4, 0x2b, 0x1c];

thread_local! {
    /// The count on the button now; `None` before the first.
    static SHOWN: Cell<Option<usize>> = const { Cell::new(None) };
}

/// Shows `count` over the taskbar button of every window, or nothing for 0.
pub(super) fn count(count: usize, cx: &mut App) {
    let first = SHOWN.get().is_none();
    if SHOWN.replace(Some(count)) == Some(count) {
        return;
    }
    apply(count, cx);
    // shortcut: the button may not exist yet when the first count comes (Windows posts
    // TaskbarButtonCreated to a window procedure GPUI keeps), so the first count is set once more a
    // moment later. Hook that message if a count is ever seen missing at start.
    if first {
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(2)).await;
            cx.update(|cx| apply(SHOWN.get().unwrap_or(0), cx));
        })
        .detach();
    }
}

fn apply(count: usize, cx: &mut App) {
    for window in cx.windows() {
        let _ = window.update(cx, |_, window, _| {
            let Ok(RawWindowHandle::Win32(handle)) = window.window_handle().map(|h| h.as_raw()) else { return };
            if let Err(e) = overlay(HWND(handle.hwnd.get() as *mut c_void), count) {
                log::warn!("taskbar count: {e}");
            }
        });
    }
}

fn overlay(hwnd: HWND, count: usize) -> windows::core::Result<()> {
    // SAFETY: COM is initialized on GPUI's thread (OleInitialize); the taskbar copies the icon, so
    // it is freed right after.
    unsafe {
        let taskbar: ITaskbarList3 = CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER)?;
        taskbar.HrInit()?;
        if count == 0 {
            return taskbar.SetOverlayIcon(hwnd, HICON::default(), PCWSTR::null());
        }
        let icon = icon(count)?;
        let result = taskbar.SetOverlayIcon(hwnd, icon, &HSTRING::from(tn!(count, "{n} unread chat", "{n} unread chats")));
        let _ = DestroyIcon(icon);
        result
    }
}

/// The circle with `count` in it ("99+" above 99), the size of a small icon.
fn icon(count: usize) -> windows::core::Result<HICON> {
    let text = if count > 99 { "99+".to_owned() } else { count.to_string() };
    // SAFETY: every GDI object made here is selected out and deleted before returning; `bits` is
    // the section's `size` x `size` x 4 bytes, read only after GdiFlush.
    unsafe {
        let size = GetSystemMetrics(SM_CXSMICON).max(16);
        let header = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        };
        let info = BITMAPINFO { bmiHeader: header, ..Default::default() };
        let dc = CreateCompatibleDC(None);
        let mut bits: *mut c_void = std::ptr::null_mut();
        let color = CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let before = SelectObject(dc, color.into());
        let height = size * if text.len() > 2 { 50 } else { 72 } / 100;
        let font = CreateFontW(-height, 0, 0, 0, FW_BOLD.0 as i32, 0, 0, 0, DEFAULT_CHARSET, OUT_DEFAULT_PRECIS, CLIP_DEFAULT_PRECIS, ANTIALIASED_QUALITY, FF_SWISS.0 as u32, w!("Segoe UI"));
        let before_font = SelectObject(dc, font.into());
        SetBkMode(dc, TRANSPARENT);
        SetTextColor(dc, COLORREF(0x00ff_ffff));
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let mut rect = RECT { left: 0, top: 0, right: size, bottom: size };
        DrawTextW(dc, &mut wide, &mut rect, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);
        let _ = GdiFlush();
        let pixels = std::slice::from_raw_parts_mut(bits.cast::<u8>(), (size * size * 4) as usize);
        compose(pixels, size as usize);
        // A 32-bit color bitmap carries its own alpha; the mask is only there because the call wants one.
        let mask = CreateBitmap(size, size, 1, 1, None);
        let made = CreateIconIndirect(&ICONINFO { fIcon: true.into(), xHotspot: 0, yHotspot: 0, hbmMask: mask, hbmColor: color });
        SelectObject(dc, before_font);
        SelectObject(dc, before);
        let _ = DeleteObject(font.into());
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        let _ = DeleteDC(dc);
        made
    }
}

/// Turns GDI's white-on-black text into the badge: a red circle filling the square, the text white
/// on it, straight BGRA alpha outside the circle's edge.
fn compose(pixels: &mut [u8], size: usize) {
    let r = size as f32 / 2.;
    for (i, px) in pixels.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (x, y) = ((i % size) as f32 + 0.5, (i / size) as f32 + 0.5);
        let fill = (r + 0.5 - ((x - r).powi(2) + (y - r).powi(2)).sqrt()).clamp(0., 1.);
        let ink = f32::from(px[1]) / 255.;
        let [red, green, blue] = RED.map(|c| (f32::from(c) * (1. - ink) + 255. * ink).round() as u8);
        *px = [blue, green, red, (fill * 255.).round() as u8];
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_badge_is_a_round_red_disc_with_white_ink() {
        let size = 16;
        let mut pixels = vec![0u8; size * size * 4];
        // One fully inked pixel in the middle.
        let mid = (8 * size + 8) * 4;
        pixels[mid..mid + 4].copy_from_slice(&[255, 255, 255, 0]);
        super::compose(&mut pixels, size);
        assert_eq!(pixels[3], 0, "a corner is outside the circle");
        assert_eq!(&pixels[mid..mid + 4], &[255, 255, 255, 255]);
        let plain = (8 * size + 3) * 4;
        assert_eq!(&pixels[plain..plain + 4], &[0x1c, 0x2b, 0xc4, 255]);
    }
}
