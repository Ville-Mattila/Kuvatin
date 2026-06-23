//! OS shell integration: the right-click menu that runs Kuvatin presets.
//!
//! Windows registers classic Explorer context-menu verbs in the registry;
//! macOS writes Automator Quick Actions ("Services") into `~/Library/Services`.
//! Both expose the same set of [`ITEMS`].

/// The right-click menu entries, shared across platforms.
///
/// `(stable id, menu label, preset name)` — an empty preset name means the item
/// opens the GUI with the selected files instead of running a preset headlessly.
/// The preset names must match built-in presets in `kuvatin-core`.
pub(crate) const ITEMS: &[(&str, &str, &str)] = &[
    ("Kuvatin.Webp", "Convert to WebP", "Convert to WebP"),
    ("Kuvatin.1080p", "Resize to 1080p", "Resize to 1080p"),
    ("Kuvatin.Half", "Resize to 50%", "Resize to 50%"),
    ("Kuvatin.Open", "Open in Kuvatin…", ""),
];

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{register, unregister};

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
pub use macos::{register, unregister};

#[cfg(not(any(windows, target_os = "macos")))]
pub fn register() -> anyhow::Result<()> {
    anyhow::bail!("right-click menu integration is only supported on Windows and macOS")
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn unregister() -> anyhow::Result<()> {
    anyhow::bail!("right-click menu integration is only supported on Windows and macOS")
}
