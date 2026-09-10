//! Opt-in update check. One HTTPS `HEAD` to GitHub's "latest release" URL with
//! redirects disabled: the `Location` header names the tag
//! (`…/releases/tag/v2.9.0`). No JSON, no API quota, and nothing is sent
//! beyond the request itself (User-Agent `Kuvatin/<version>`).

use anyhow::{anyhow, bail, Result};

pub const RELEASES_URL: &str = "https://github.com/Ville-Mattila/Kuvatin/releases/latest";
const HOST: &str = "github.com";
const PATH: &str = "/Ville-Mattila/Kuvatin/releases/latest";
/// Once a day is plenty for a desktop tool.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The latest published version, e.g. `"2.9.0"`. Blocking (network); call
/// from a worker thread.
pub fn latest_version() -> Result<String> {
    let location = head_location(HOST, PATH)?;
    parse_tag(&location).ok_or_else(|| anyhow!("unexpected redirect target {location:?}"))
}

/// `…/releases/tag/v2.9.0` → `Some("2.9.0")`; anything else → `None`.
pub fn parse_tag(location: &str) -> Option<String> {
    let (_, tag) = location.rsplit_once("/tag/")?;
    let v = tag
        .trim()
        .split(['?', '#', '/'])
        .next()?
        .trim_start_matches('v');
    let numeric = !v.is_empty()
        && v.split('.')
            .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()));
    numeric.then(|| v.to_string())
}

fn triple(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v
        .trim_start_matches('v')
        .split('.')
        .map(|p| p.parse::<u64>().ok());
    let (a, b, c) = (it.next()??, it.next()??, it.next()??);
    it.next().is_none().then_some((a, b, c))
}

/// Strictly newer, comparing `major.minor.patch` numerically. Unparseable
/// input is never "newer" (no false alarms from an odd tag).
pub fn is_newer(latest: &str, current: &str) -> bool {
    matches!((triple(latest), triple(current)), (Some(l), Some(c)) if l > c)
}

/// Open the releases page in the default browser.
pub fn open_releases_page() {
    #[cfg(windows)]
    unsafe {
        use windows::core::{w, HSTRING, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
        let url = HSTRING::from(RELEASES_URL);
        ShellExecuteW(
            None,
            w!("open"),
            &url,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

/// `HEAD https://host/path` with redirects disabled; returns the `Location`
/// header. Uses WinHTTP (system TLS and proxy settings, no extra crates).
#[cfg(windows)]
fn head_location(host: &str, path: &str) -> Result<String> {
    use windows::core::{w, Error, HSTRING, PCWSTR};
    use windows::Win32::Networking::WinHttp::*;

    struct Handle(*mut core::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }

    let agent = HSTRING::from(format!("Kuvatin/{CURRENT}"));
    let host = HSTRING::from(host);
    let path = HSTRING::from(path);
    unsafe {
        let session = Handle(WinHttpOpen(
            &agent,
            WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        ));
        if session.0.is_null() {
            bail!("WinHttpOpen: {}", Error::from_win32());
        }
        WinHttpSetTimeouts(session.0, 5_000, 5_000, 5_000, 5_000)?;
        let conn = Handle(WinHttpConnect(
            session.0,
            &host,
            INTERNET_DEFAULT_HTTPS_PORT,
            0,
        ));
        if conn.0.is_null() {
            bail!("WinHttpConnect: {}", Error::from_win32());
        }
        let req = Handle(WinHttpOpenRequest(
            conn.0,
            w!("HEAD"),
            &path,
            PCWSTR::null(),
            PCWSTR::null(),
            std::ptr::null(),
            WINHTTP_FLAG_SECURE,
        ));
        if req.0.is_null() {
            bail!("WinHttpOpenRequest: {}", Error::from_win32());
        }
        // Stay on the redirect response: its target is the answer.
        let disable = WINHTTP_DISABLE_REDIRECTS.to_ne_bytes();
        WinHttpSetOption(Some(req.0), WINHTTP_OPTION_DISABLE_FEATURE, Some(&disable))?;
        WinHttpSendRequest(req.0, None, None, 0, 0, 0)?;
        WinHttpReceiveResponse(req.0, std::ptr::null_mut())?;

        // Size the buffer (this call fails with ERROR_INSUFFICIENT_BUFFER and
        // sets `len`), then read the header.
        let mut len: u32 = 0;
        let _ = WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            None,
            &mut len,
            std::ptr::null_mut(),
        );
        if len == 0 {
            let mut code: u32 = 0;
            let mut code_len = std::mem::size_of::<u32>() as u32;
            let _ = WinHttpQueryHeaders(
                req.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&mut code as *mut u32).cast()),
                &mut code_len,
                std::ptr::null_mut(),
            );
            bail!("no Location header (HTTP {code})");
        }
        let mut buf = vec![0u16; len as usize / 2 + 1];
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_LOCATION,
            PCWSTR::null(),
            Some(buf.as_mut_ptr().cast()),
            &mut len,
            std::ptr::null_mut(),
        )?;
        Ok(String::from_utf16_lossy(&buf[..len as usize / 2]))
    }
}

#[cfg(not(windows))]
fn head_location(_host: &str, _path: &str) -> Result<String> {
    bail!("the update check is Windows-only")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_tag_out_of_the_redirect_target() {
        assert_eq!(
            parse_tag("https://github.com/Ville-Mattila/Kuvatin/releases/tag/v2.9.0").as_deref(),
            Some("2.9.0")
        );
        assert_eq!(
            parse_tag("/releases/tag/2.10.3?x=1").as_deref(),
            Some("2.10.3")
        );
        assert_eq!(
            parse_tag("https://github.com/Ville-Mattila/Kuvatin/releases"),
            None
        );
        assert_eq!(parse_tag("/releases/tag/nightly"), None);
        assert_eq!(
            parse_tag("/releases/tag/v2.9").as_deref(),
            Some("2.9"),
            "shape only; is_newer decides"
        );
    }

    /// Talks to github.com: run explicitly with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn latest_version_reaches_github() {
        let v = latest_version().expect("HEAD github.com/…/releases/latest");
        assert!(triple(&v).is_some(), "got {v:?}");
    }

    #[test]
    fn newer_compares_numerically_and_never_lies_on_junk() {
        assert!(is_newer("2.9.0", "2.8.1"));
        assert!(is_newer("2.10.0", "2.9.9"), "not a string compare");
        assert!(is_newer("3.0.0", "2.99.99"));
        assert!(!is_newer("2.8.1", "2.8.1"));
        assert!(!is_newer("2.8.0", "2.8.1"));
        assert!(
            !is_newer("2.9", "2.8.1"),
            "two-part tag: unknown, not newer"
        );
        assert!(!is_newer("nightly", "2.8.1"));
        assert!(triple(CURRENT).is_some(), "CARGO_PKG_VERSION is x.y.z");
    }
}
