#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{ensure_registered, notify_error, register, unregister};

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
