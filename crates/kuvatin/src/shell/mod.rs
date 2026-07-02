#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{ensure_registered, register, unregister};

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
