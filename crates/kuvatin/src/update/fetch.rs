//! Fetching over HTTPS with WinHTTP, the way the check already talks to
//! GitHub: system proxy, system trust store, no extra crates.

use anyhow::{bail, Result};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Nothing we publish comes near this. It stops a wrong address, or a server
/// that keeps talking, from filling the disk.
#[allow(dead_code)] // Only the tests call this so far; staging a download is next.
pub const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

/// How far a download has got.
#[allow(dead_code)] // Only the tests read this so far; the dialog is next.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub done: u64,
    /// `None` when the server sent no Content-Length.
    pub total: Option<u64>,
}

impl Progress {
    /// 0.0 to 1.0, or `None` when the size is unknown.
    #[allow(dead_code)] // Only the tests call this so far; the dialog is next.
    pub fn fraction(&self) -> Option<f32> {
        let total = self.total?;
        if total == 0 {
            return Some(1.0);
        }
        Some((self.done as f32 / total as f32).clamp(0.0, 1.0))
    }
}

fn within_ceiling(bytes: u64, ceiling: u64) -> Result<()> {
    if bytes > ceiling {
        bail!("the download is too large ({bytes} bytes, limit {ceiling})");
    }
    Ok(())
}

/// The WinHTTP calls, kept together so the rest of the file reads as plain
/// Rust. This follows `update::head_location`: the same handle wrapper, the
/// same five-second timeouts, the same agent string. The one difference is
/// that redirects are left enabled, because a release asset redirects to
/// `objects.githubusercontent.com`.
#[cfg(windows)]
mod win {
    use super::*;
    use windows::core::{Error, HSTRING, PCWSTR, PWSTR};
    use windows::Win32::Networking::WinHttp::*;

    pub(super) struct Handle(pub *mut core::ffi::c_void);
    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                unsafe {
                    let _ = WinHttpCloseHandle(self.0);
                }
            }
        }
    }

    /// host, path, and whether it is https.
    pub(super) fn crack(url: &str) -> Result<(String, String, bool)> {
        let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
        let mut host = vec![0u16; 256];
        let mut path = vec![0u16; 2048];
        let mut c = URL_COMPONENTS {
            dwStructSize: std::mem::size_of::<URL_COMPONENTS>() as u32,
            lpszHostName: PWSTR(host.as_mut_ptr()),
            dwHostNameLength: host.len() as u32,
            lpszUrlPath: PWSTR(path.as_mut_ptr()),
            dwUrlPathLength: path.len() as u32,
            ..Default::default()
        };
        unsafe { WinHttpCrackUrl(&wide[..wide.len() - 1], 0, &mut c)? };
        let host = String::from_utf16_lossy(&host[..c.dwHostNameLength as usize]);
        let path = String::from_utf16_lossy(&path[..c.dwUrlPathLength as usize]);
        Ok((host, path, c.nScheme == WINHTTP_INTERNET_SCHEME_HTTPS))
    }

    /// Send a GET and leave the response ready to read. The session and
    /// connection handles ride along so they outlive the request: bind the
    /// three in this order and they close in the reverse, children first.
    pub(super) fn send(url: &str) -> Result<(Handle, Handle, Handle, Option<u64>)> {
        let (host, path, https) = crack(url)?;
        if !https {
            bail!("refusing a plain HTTP address: {url}");
        }
        let agent = HSTRING::from(format!("Kuvatin/{}", crate::update::CURRENT));
        let host_w = HSTRING::from(host.as_str());
        let path_w = HSTRING::from(path.as_str());
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
                &host_w,
                INTERNET_DEFAULT_HTTPS_PORT,
                0,
            ));
            if conn.0.is_null() {
                bail!("WinHttpConnect: {}", Error::from_win32());
            }
            let req = Handle(WinHttpOpenRequest(
                conn.0,
                windows::core::w!("GET"),
                &path_w,
                PCWSTR::null(),
                PCWSTR::null(),
                std::ptr::null(),
                WINHTTP_FLAG_SECURE,
            ));
            if req.0.is_null() {
                bail!("WinHttpOpenRequest: {}", Error::from_win32());
            }
            WinHttpSendRequest(req.0, None, None, 0, 0, 0)?;
            WinHttpReceiveResponse(req.0, std::ptr::null_mut())?;

            let mut code: u32 = 0;
            let mut len = std::mem::size_of::<u32>() as u32;
            WinHttpQueryHeaders(
                req.0,
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                PCWSTR::null(),
                Some((&mut code as *mut u32).cast()),
                &mut len,
                std::ptr::null_mut(),
            )?;
            if code != 200 {
                bail!("the server answered HTTP {code}");
            }

            let mut total: u64 = 0;
            let mut len = std::mem::size_of::<u64>() as u32;
            let got = WinHttpQueryHeaders(
                req.0,
                WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER64,
                PCWSTR::null(),
                Some((&mut total as *mut u64).cast()),
                &mut len,
                std::ptr::null_mut(),
            )
            .is_ok();
            Ok((session, conn, req, got.then_some(total)))
        }
    }

    /// Read the whole body in 64 KiB chunks, handing each to `sink`. `sink`
    /// returning `false` stops the transfer early.
    pub(super) fn drain(
        req: &Handle,
        ceiling: u64,
        mut sink: impl FnMut(&[u8]) -> Result<bool>,
    ) -> Result<u64> {
        let mut buf = vec![0u8; 64 * 1024];
        let mut done: u64 = 0;
        loop {
            let mut read: u32 = 0;
            unsafe {
                WinHttpReadData(req.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut read)?
            };
            if read == 0 {
                return Ok(done);
            }
            done += u64::from(read);
            within_ceiling(done, ceiling)?;
            if !sink(&buf[..read as usize])? {
                return Ok(done);
            }
        }
    }
}

/// GET `url` into `dest`, following redirects. `on_progress` is called as
/// bytes land. A set `cancel` stops the transfer and removes the part file.
#[allow(dead_code)] // Only the tests call this so far; staging a download is next.
#[cfg(windows)]
pub fn get_to_file(
    url: &str,
    dest: &Path,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(Progress),
) -> Result<()> {
    use std::io::Write;

    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // A half-written file never carries the name the installer is looked up
    // by; it is renamed into place only once the body is complete.
    let part = dest.with_extension("part");
    let outcome = (|| -> Result<bool> {
        let (_session, _conn, req, total) = win::send(url)?;
        if let Some(t) = total {
            within_ceiling(t, MAX_DOWNLOAD)?;
        }
        let mut file = std::fs::File::create(&part)?;
        let mut done: u64 = 0;
        on_progress(Progress { done: 0, total });
        win::drain(&req, MAX_DOWNLOAD, |chunk| {
            if cancel.load(Ordering::Relaxed) {
                return Ok(false);
            }
            file.write_all(chunk)?;
            done += chunk.len() as u64;
            on_progress(Progress { done, total });
            Ok(true)
        })?;
        file.flush()?;
        Ok(!cancel.load(Ordering::Relaxed))
    })();

    match outcome {
        Ok(true) => {
            std::fs::rename(&part, dest)?;
            Ok(())
        }
        Ok(false) => {
            let _ = std::fs::remove_file(&part);
            bail!("cancelled")
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// GET `url` as text, refusing anything past `limit` bytes.
#[allow(dead_code)] // Only the tests call this so far; staging a download is next.
#[cfg(windows)]
pub fn get_to_string(url: &str, limit: usize) -> Result<String> {
    let (_session, _conn, req, _total) = win::send(url)?;
    let mut body: Vec<u8> = Vec::new();
    win::drain(&req, limit as u64, |chunk| {
        body.extend_from_slice(chunk);
        Ok(true)
    })?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[allow(dead_code)] // Only the tests call this so far; staging a download is next.
#[cfg(not(windows))]
pub fn get_to_file(
    _url: &str,
    _dest: &Path,
    _cancel: &AtomicBool,
    _on_progress: &mut dyn FnMut(Progress),
) -> Result<()> {
    bail!("downloading is Windows-only")
}

#[allow(dead_code)] // Only the tests call this so far; staging a download is next.
#[cfg(not(windows))]
pub fn get_to_string(_url: &str, _limit: usize) -> Result<String> {
    bail!("downloading is Windows-only")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_response_within_the_ceiling_is_fine_and_one_past_it_is_not() {
        assert!(within_ceiling(0, MAX_DOWNLOAD).is_ok());
        assert!(within_ceiling(MAX_DOWNLOAD, MAX_DOWNLOAD).is_ok());
        let past = within_ceiling(MAX_DOWNLOAD + 1, MAX_DOWNLOAD);
        let msg = format!("{:#}", past.expect_err("past the ceiling"));
        assert!(msg.contains("too large"), "{msg}");
    }

    #[test]
    fn progress_reports_a_total_only_when_the_server_gave_one() {
        let known = Progress {
            done: 10,
            total: Some(100),
        };
        assert_eq!(known.fraction(), Some(0.1));
        let unknown = Progress {
            done: 10,
            total: None,
        };
        assert_eq!(unknown.fraction(), None);
        // A server that lies about the length must not produce a fraction
        // above one; the bar would run off the end of the card.
        let over = Progress {
            done: 200,
            total: Some(100),
        };
        assert_eq!(over.fraction(), Some(1.0));
    }

    /// Talks to github.com: run explicitly with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn reads_the_checksum_file_of_the_current_release() {
        let latest = crate::update::latest_version().expect("latest version");
        let (_, sha_url) = crate::update::asset_urls(&latest);
        let body = get_to_string(&sha_url, 4096).expect("fetch the checksum file");
        let name = crate::update::asset_name(&latest);
        assert!(
            crate::update::verify::expected_hash(&body, &name).is_some(),
            "checksum file did not name {name}: {body:?}"
        );
    }
}
