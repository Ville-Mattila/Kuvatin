#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{register, unregister};

#[cfg(not(windows))]
pub fn register() -> anyhow::Result<()> {
    anyhow::bail!("context-menu registration is only supported on Windows")
}

#[cfg(not(windows))]
pub fn unregister() -> anyhow::Result<()> {
    anyhow::bail!("context-menu registration is only supported on Windows")
}
