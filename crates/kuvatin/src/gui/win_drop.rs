//! Native Windows Explorer drag-and-drop support via `WM_DROPFILES`.
//!
//! Slint 1.16 does not expose OS file-drop events, so we obtain the window's
//! `HWND` (through the `raw-window-handle-06` slint feature), call
//! `DragAcceptFiles`, and subclass the window proc to intercept `WM_DROPFILES`.
//! Dropped paths are pushed into a process-global inbox that the UI thread
//! drains on a repeating timer — this avoids passing Rust closures through the
//! C callback boundary.

use super::AppWindow;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::ComponentHandle;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
};
use windows::Win32::System::Ole::RevokeDragDrop;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Shell::{
    DefSubclassProc, DragAcceptFiles, DragFinish, DragQueryFileW, SetWindowSubclass, HDROP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindowRect, IsZoomed, PostMessageW, ShowWindow, HTBOTTOM, HTBOTTOMLEFT,
    HTBOTTOMRIGHT, HTCAPTION, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT, SW_MAXIMIZE,
    SW_MINIMIZE, SW_RESTORE, WM_CLOSE, WM_DROPFILES,
};

/// Width of the invisible edge zone (in physical px) used for resize hit-testing.
const RESIZE_BORDER: i32 = 6;

/// Inbox of paths dropped onto the window, awaiting drain by the UI thread.
static INBOX: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();
/// Guards against subclassing the window more than once.
static INSTALLED: OnceLock<()> = OnceLock::new();
/// The native window handle, captured in `enable()` so the win-* callbacks
/// can reach it without re-deriving it from the Slint window each time.
static HWND_RAW: OnceLock<isize> = OnceLock::new();

fn inbox() -> &'static Mutex<Vec<PathBuf>> {
    INBOX.get_or_init(|| Mutex::new(Vec::new()))
}

/// The captured HWND, if `enable()` has run.
fn hwnd() -> Option<HWND> {
    HWND_RAW
        .get()
        .map(|raw| HWND(*raw as *mut std::ffi::c_void))
}

/// Minimize the window.
pub fn minimize() {
    if let Some(hwnd) = hwnd() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
        }
    }
}

/// Toggle maximize/restore.
pub fn maximize() {
    if let Some(hwnd) = hwnd() {
        unsafe {
            if IsZoomed(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            } else {
                let _ = ShowWindow(hwnd, SW_MAXIMIZE);
            }
        }
    }
}

/// Request a clean close (lets Slint tear down via the normal WM_CLOSE path).
pub fn close() {
    if let Some(hwnd) = hwnd() {
        unsafe {
            let _ = PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

/// Drain all queued dropped paths. Called by the UI-thread timer.
pub fn take_dropped() -> Vec<PathBuf> {
    let mut guard = inbox().lock().unwrap();
    std::mem::take(&mut *guard)
}

/// Enable Explorer drag-and-drop on the given window. Idempotent: only the
/// first call installs the subclass. Must run after the window is shown so
/// the native HWND exists.
pub fn enable(ui: &AppWindow) {
    if INSTALLED.get().is_some() {
        return;
    }
    let Some(hwnd) = hwnd_of(ui) else {
        return;
    };
    // Stash the raw handle so the win-* callbacks can use it later.
    let _ = HWND_RAW.set(hwnd.0 as isize);
    // Windows 11: round the frameless window's outer corners via DWM.
    unsafe {
        let pref = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &pref as *const _ as *const core::ffi::c_void,
            std::mem::size_of_val(&pref) as u32,
        );
    }
    // SAFETY: hwnd is a valid window handle obtained from the shown window,
    // and we run on the UI/event-loop thread that owns it.
    unsafe {
        // Slint's winit backend registers its own OLE drop target on the
        // window (RegisterDragDrop). While that's in place our DragAcceptFiles
        // call silently fails (RegisterDragDrop returns ALREADYREGISTERED), so
        // WM_DROPFILES never arrives. Revoke winit's target first, then claim
        // the window for the classic shell drag-drop that posts WM_DROPFILES.
        let _ = RevokeDragDrop(hwnd);
        DragAcceptFiles(hwnd, true);
        // Subclass id 1, no per-instance refdata (we use a global inbox).
        if SetWindowSubclass(hwnd, Some(subclass_proc), 1, 0).as_bool() {
            let _ = INSTALLED.set(());
        }
    }
}

/// Extract the Win32 HWND from a shown Slint window.
fn hwnd_of(ui: &AppWindow) -> Option<HWND> {
    let handle = ui.window().window_handle();
    match handle.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(isize::from(h.hwnd) as *mut std::ffi::c_void)),
        _ => None,
    }
}

/// Window subclass proc. Runs on the UI thread (same thread as the Slint
/// event loop). On `WM_DROPFILES` it reads the dropped paths and queues them
/// in the inbox; everything else is forwarded to the default chain.
unsafe extern "system" fn subclass_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _uid: usize,
    _refdata: usize,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::WM_NCHITTEST;
    if msg == WM_NCHITTEST {
        // The frameless window has no native border, so we synthesize resize
        // grips: if the cursor is within RESIZE_BORDER px of an edge, return
        // the matching hit code so Windows runs its native resize loop.
        // The lparam packs screen coords as signed 16-bit lo/hi words;
        // GetCursorPos avoids sign/monitor pitfalls and gives the same point.
        let mut pt = POINT { x: 0, y: 0 };
        if GetCursorPos(&mut pt).is_err() {
            // Fall back to the lparam-packed coords.
            pt.x = (lparam.0 & 0xFFFF) as i16 as i32;
            pt.y = ((lparam.0 >> 16) & 0xFFFF) as i16 as i32;
        }
        let mut rc = RECT::default();
        if GetWindowRect(hwnd, &mut rc).is_ok() {
            let b = RESIZE_BORDER;
            let left = pt.x < rc.left + b;
            let right = pt.x >= rc.right - b;
            let top = pt.y < rc.top + b;
            let bottom = pt.y >= rc.bottom - b;

            let hit = if top && left {
                Some(HTTOPLEFT)
            } else if top && right {
                Some(HTTOPRIGHT)
            } else if bottom && left {
                Some(HTBOTTOMLEFT)
            } else if bottom && right {
                Some(HTBOTTOMRIGHT)
            } else if left {
                Some(HTLEFT)
            } else if right {
                Some(HTRIGHT)
            } else if top {
                Some(HTTOP)
            } else if bottom {
                Some(HTBOTTOM)
            } else {
                None
            };

            if let Some(code) = hit {
                return LRESULT(code as isize);
            }

            // Title-bar band (excluding the right-side window buttons) acts
            // as the caption, so Windows drags the window natively. This
            // replaces firing WM_NCLBUTTONDOWN from inside Slint's pointer
            // handler, which nested a modal move loop and broke client input.
            let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
            let titlebar_h = (36.0 * scale) as i32;
            let buttons_w = (3.0 * 46.0 * scale) as i32;
            if pt.y < rc.top + titlebar_h && pt.x < rc.right - buttons_w {
                return LRESULT(HTCAPTION as isize);
            }
        }
        // Everything else: let the default proc classify it (HTCLIENT, etc.)
        // so winit/Slint receive normal mouse input.
        return DefSubclassProc(hwnd, msg, wparam, lparam);
    }
    if msg == WM_DROPFILES {
        let hdrop = HDROP(wparam.0 as *mut std::ffi::c_void);
        let mut dropped = Vec::new();
        // Passing 0xFFFFFFFF as the index returns the file count.
        let count = DragQueryFileW(hdrop, 0xFFFF_FFFF, None);
        for i in 0..count {
            // First query the required length (excluding NUL).
            let len = DragQueryFileW(hdrop, i, None);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let written = DragQueryFileW(hdrop, i, Some(&mut buf));
            if written > 0 {
                let s = String::from_utf16_lossy(&buf[..written as usize]);
                dropped.push(PathBuf::from(s));
            }
        }
        DragFinish(hdrop);
        if !dropped.is_empty() {
            inbox().lock().unwrap().extend(dropped);
        }
        return LRESULT(0);
    }
    DefSubclassProc(hwnd, msg, wparam, lparam)
}
