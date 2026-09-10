//! The sparse package ("package with external location") that carries the
//! Windows 11 context-menu handler. Registering `Kuvatin.msix` with the install
//! directory as its external location gives that directory package identity,
//! which is the only way onto Windows 11's top-level context menu; the handler
//! itself is `kuvatin_shellext.dll` next to the exe (see `crates/kuvatin-shellext`).
//!
//! Registration is per user. The package must be signed with a certificate the
//! machine trusts: it declares a COM server ("executable activations"), which
//! Windows refuses in an unsigned package (0x80073D2B), and a signature whose
//! certificate is in neither the machine's Trusted People nor Trusted Root
//! store fails with 0x800B0109. Windows 10 cannot show the menu or register the
//! package, so everything here is skipped below build 22000, and every failure
//! is non-fatal: the classic registry menu keeps working regardless.

use anyhow::{bail, Context, Result};
use std::path::Path;
use windows::core::HSTRING;
use windows::Foundation::Uri;
use windows::Management::Deployment::{AddPackageOptions, PackageManager};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// `Identity Name` in `crates/kuvatin/msix/AppxManifest.xml`.
pub const PACKAGE_NAME: &str = "VilleMattila.Kuvatin";
/// The package file, installed next to the exe.
pub const MSIX_FILE: &str = "Kuvatin.msix";
/// Windows 11: the first build with the new context menu.
const MIN_BUILD: u32 = 22000;

/// What one registration attempt concluded, for the log and the console.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Registered (or re-registered) now.
    Registered,
    /// This version at this location was already registered.
    AlreadyRegistered,
    /// Windows 10 (or older): no top-level menu to register into.
    Unsupported,
}

/// Windows build number from the registry (`GetVersionEx` lies to unmanifested
/// callers); 0 when unreadable.
pub fn windows_build() -> u32 {
    super::windows::read_hklm_string(
        r"SOFTWARE\Microsoft\Windows NT\CurrentVersion",
        "CurrentBuildNumber",
    )
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0)
}

pub fn os_supports_package() -> bool {
    windows_build() >= MIN_BUILD
}

fn init_com() {
    // WinRT activation needs an apartment; MTA suits the blocking `.get()`
    // waits below. RPC_E_CHANGED_MODE (already STA) is fine too.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// `file:///C:/Program%20Files/...`: forward slashes, everything outside the
/// URI-unreserved set percent-encoded (UTF-8 bytes for non-ASCII).
fn file_uri_string(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut encoded = String::from("file:///");
    for b in text.trim_start_matches('/').bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(b as char)
            }
            _ => encoded.push_str(&format!("%{b:02X}")),
        }
    }
    encoded
}

fn file_uri(path: &Path) -> Result<Uri> {
    Uri::CreateUri(&HSTRING::from(file_uri_string(path))).context("file URI")
}

/// The registered package for the current user, as `(full name, version,
/// external location)`, if any.
pub fn registered() -> Result<Option<(String, String, String)>> {
    init_com();
    let pm = PackageManager::new()?;
    for p in pm.FindPackagesByUserSecurityId(&HSTRING::new())? {
        let id = p.Id()?;
        if id.Name()? != PACKAGE_NAME {
            continue;
        }
        let v = id.Version()?;
        let version = format!("{}.{}.{}", v.Major, v.Minor, v.Build);
        // The external location (the install dir), not the WindowsApps folder
        // that holds the package's own manifest.
        let location = p
            .EffectiveExternalPath()
            .map(|h| h.to_string())
            .unwrap_or_default();
        return Ok(Some((id.FullName()?.to_string(), version, location)));
    }
    Ok(None)
}

/// Register `install_dir\Kuvatin.msix` with `install_dir` as the external
/// location, replacing a registration of another version or location.
pub fn register(install_dir: &Path) -> Result<Outcome> {
    if !os_supports_package() {
        return Ok(Outcome::Unsupported);
    }
    let msix = install_dir.join(MSIX_FILE);
    if !msix.is_file() {
        bail!("{} is missing", msix.display());
    }
    let wanted = env!("CARGO_PKG_VERSION");
    if let Some((full, version, location)) = registered()? {
        if version == wanted && same_dir(&location, install_dir) {
            return Ok(Outcome::AlreadyRegistered);
        }
        // Same version elsewhere, or another version: Windows refuses to
        // re-register an identical version in place, so start clean.
        remove(&full)?;
    }
    let pm = PackageManager::new()?;
    let opts = AddPackageOptions::new()?;
    opts.SetExternalLocationUri(&file_uri(install_dir)?)?;
    let result = pm.AddPackageByUriAsync(&file_uri(&msix)?, &opts)?.get()?;
    let code = result.ExtendedErrorCode()?;
    if code.is_err() {
        bail!(
            "AddPackageByUriAsync: {} (0x{:08X})",
            result.ErrorText()?.to_string().trim(),
            code.0 as u32
        );
    }
    Ok(Outcome::Registered)
}

/// Remove the current user's registration, if any.
pub fn unregister() -> Result<bool> {
    if !os_supports_package() {
        return Ok(false);
    }
    match registered()? {
        Some((full, _, _)) => {
            remove(&full)?;
            Ok(true)
        }
        None => Ok(false),
    }
}

fn remove(full_name: &str) -> Result<()> {
    let pm = PackageManager::new()?;
    let result = pm.RemovePackageAsync(&HSTRING::from(full_name))?.get()?;
    let code = result.ExtendedErrorCode()?;
    if code.is_err() {
        bail!(
            "RemovePackageAsync({full_name}): {} (0x{:08X})",
            result.ErrorText()?.to_string().trim(),
            code.0 as u32
        );
    }
    Ok(())
}

fn same_dir(a: &str, b: &Path) -> bool {
    let norm = |s: &str| s.trim_end_matches(['\\', '/']).to_lowercase();
    norm(a) == norm(&b.to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_escape_spaces_and_use_forward_slashes() {
        assert_eq!(
            file_uri_string(Path::new(r"C:\Program Files\kuvatin\bin")),
            "file:///C:/Program%20Files/kuvatin/bin"
        );
        assert_eq!(
            file_uri_string(Path::new(r"D:\Työt\Kuvatin.msix")),
            "file:///D:/Ty%C3%B6t/Kuvatin.msix"
        );
        // And WinRT accepts the result (it may re-render it differently).
        assert!(file_uri(Path::new(r"C:\Program Files\kuvatin\bin")).is_ok());
    }

    #[test]
    fn same_dir_ignores_case_and_trailing_separators() {
        assert!(same_dir(
            r"C:\Program Files\Kuvatin\bin\",
            Path::new(r"c:\program files\kuvatin\bin")
        ));
        assert!(!same_dir(
            r"C:\Other",
            Path::new(r"C:\Program Files\kuvatin\bin")
        ));
    }

    #[test]
    fn windows_build_is_a_real_number_here() {
        assert!(windows_build() >= 10240, "got {}", windows_build());
    }
}
