#[cfg(windows)]
mod package;
#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{
    attach_parent_console, ensure_registered, menu_extensions, notify_error, register, set_quiet,
    sync_menu, unregister,
};

/// Surface an error to the user when there's no console to print to. No-op off
/// Windows (the headless quick-run path is Windows-only).
#[cfg(not(windows))]
pub fn notify_error(_title: &str, _text: &str) {}

#[cfg(not(windows))]
pub fn register() -> anyhow::Result<()> {
    anyhow::bail!("context-menu registration is only supported on Windows")
}

#[cfg(not(windows))]
pub fn unregister() -> anyhow::Result<()> {
    anyhow::bail!("context-menu registration is only supported on Windows")
}

/// Best-effort self-healing registration for GUI startup; no-op off Windows.
#[cfg(not(windows))]
pub fn ensure_registered() {}

/// Rewrite the submenu after presets changed; no-op off Windows.
#[cfg(not(windows))]
pub fn sync_menu() {}

/// Suppress dialogs (installer runs); no-op off Windows.
#[cfg(not(windows))]
pub fn set_quiet(_quiet: bool) {}

/// Attach to a parent terminal's console; no-op off Windows.
#[cfg(not(windows))]
pub fn attach_parent_console() {}

/// The Explorer menu's extensions; empty off Windows (no menu there).
#[cfg(not(windows))]
pub fn menu_extensions() -> Vec<&'static str> {
    Vec::new()
}
