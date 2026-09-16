# In-App Update Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Clicking the update badge installs the new version: confirm, download, check the published checksum, then close, install and reopen.

**Architecture:** `update.rs` becomes a four-file module (check, fetch, verify, apply). An installer cannot overwrite a running executable, so the app copies itself into a staging folder and starts that copy with `--apply-update`; the copy waits for the app to exit, runs `msiexec`, relaunches the installed app and stops. The interface is one modal driven by a phase property.

**Tech Stack:** Rust, Slint 1.8, the `windows` crate (WinHTTP for the download, CNG for SHA-256, `Win32_System_Threading` for the wait), WiX 3 MSI, GitHub Actions.

**Spec:** `docs/superpowers/specs/2026-09-16-in-app-update-design.md`. Read it before Task 1; it explains why the helper is a copy of the executable and what the checksum does and does not prove.

---

## Before you start

- **Work in a worktree on branch `in-app-update`.** Do not commit to `master`.
  Absolute paths and `git -C`; never touch the main checkout at
  `C:\Työt\Koodaus\Kuvatin`.
- **GStreamer must be on PATH for any cargo command** (Git Bash):
  `export PATH="/c/Program Files/gstreamer/1.0/msvc_x86_64/bin:$PATH"`.
- **Never run filesystem-wide searches** (`find /`). Crate sources live under
  `~/.cargo/registry/src/index.crates.io-*/`.
- **Never run the installer, `msiexec`, or `kuvatin.exe --apply-update` against
  a real install on this machine.** Tests use temporary folders and a process
  that has already exited. The only real install happens on the CI runner.
- **Every commit ends with** `Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>`.
  Commit titles are plain sentences, as `git log --oneline -15` shows.
- **Gates before every commit:** `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test -p kuvatin`.

## File structure

| File | Responsibility |
| --- | --- |
| `crates/kuvatin/src/update/mod.rs` | The existing check, unchanged, plus the release asset addresses. |
| `crates/kuvatin/src/update/fetch.rs` | WinHTTP GET: to a file with progress and cancellation, or to a short string. |
| `crates/kuvatin/src/update/verify.rs` | SHA-256 of a file (CNG) and reading a checksum file. |
| `crates/kuvatin/src/update/apply.rs` | Staging folder, download-and-verify, handing off to the helper, the helper itself, the startup sweep. |
| `crates/kuvatin/src/gui/updates.rs` | Dialog wiring on top of the existing check wiring. |
| `crates/kuvatin/src/cli.rs`, `main.rs` | The `--apply-update` mode. |
| `crates/kuvatin/ui/app.slint` | Badge action, the update dialog. |
| `crates/kuvatin-video/src/project.rs` | `is_dirty()` over the flag that already exists. |
| `.github/workflows/release.yml` | A step that drives the helper against the built installer. |

---

### Task 1: Make `update` a module folder and add the asset addresses

**Files:**
- Create: `crates/kuvatin/src/update/mod.rs` (moved from `crates/kuvatin/src/update.rs`)
- Delete: `crates/kuvatin/src/update.rs`

- [ ] **Step 1: Move the file, no edits**

```bash
git mv crates/kuvatin/src/update.rs crates/kuvatin/src/update/mod.rs
```

- [ ] **Step 2: Prove nothing moved but the file**

Run: `cargo test -p kuvatin -- update::`
Expected: the three existing tests pass (`parses_the_tag_out_of_the_redirect_target`, `newer_compares_numerically_and_never_lies_on_junk`, and the ignored network one is listed).

- [ ] **Step 3: Write the failing test for the asset addresses**

Add to the `tests` module at the bottom of `crates/kuvatin/src/update/mod.rs`:

```rust
    #[test]
    fn asset_addresses_name_the_version_not_the_fixed_name_copy() {
        let (msi, sha) = asset_urls("2.13.0");
        assert_eq!(
            msi,
            "https://github.com/Ville-Mattila/Kuvatin/releases/download/v2.13.0/kuvatin-2.13.0-x86_64.msi"
        );
        assert_eq!(sha, format!("{msi}.sha256"));
        assert_eq!(asset_name("2.13.0"), "kuvatin-2.13.0-x86_64.msi");
        // The release also publishes kuvatin-x86_64.msi. We never ask for it:
        // its checksum file would not say which build it describes.
        assert!(!msi.contains("/kuvatin-x86_64.msi"));
    }
```

- [ ] **Step 4: Run it and watch it fail**

Run: `cargo test -p kuvatin -- asset_addresses_name_the_version`
Expected: FAIL to compile, "cannot find function `asset_urls`".

- [ ] **Step 5: Implement**

Add near `RELEASES_URL` in `crates/kuvatin/src/update/mod.rs`:

```rust
const DOWNLOAD_BASE: &str = "https://github.com/Ville-Mattila/Kuvatin/releases/download";

/// The installer file a release publishes for `version`.
pub fn asset_name(version: &str) -> String {
    format!("kuvatin-{version}-x86_64.msi")
}

/// The installer and its checksum file, in that order.
pub fn asset_urls(version: &str) -> (String, String) {
    let msi = format!("{DOWNLOAD_BASE}/v{version}/{}", asset_name(version));
    let sha = format!("{msi}.sha256");
    (msi, sha)
}
```

- [ ] **Step 6: Run it and watch it pass**

Run: `cargo test -p kuvatin -- update::`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "The update module becomes a folder, and knows the asset addresses"
```

---

### Task 2: Read the published checksum file

**Files:**
- Create: `crates/kuvatin/src/update/verify.rs`
- Modify: `crates/kuvatin/src/update/mod.rs` (add `pub mod verify;`)

- [ ] **Step 1: Write the failing tests**

Create `crates/kuvatin/src/update/verify.rs` with only the test module:

```rust
//! What a download has to prove before it is allowed to run.

#[cfg(test)]
mod tests {
    use super::*;

    const NAME: &str = "kuvatin-2.13.0-x86_64.msi";
    const HEX: &str = "9f2c4a1b8e7d6c5b4a39281706f5e4d3c2b1a09887766554433221100ffeeddc";

    #[test]
    fn reads_the_hash_for_the_file_it_names() {
        let file = format!("{HEX}  {NAME}\n");
        assert_eq!(expected_hash(&file, NAME).as_deref(), Some(HEX));
    }

    #[test]
    fn ignores_surrounding_space_and_uppercase_hex() {
        let file = format!("  {}  {NAME}  \n", HEX.to_uppercase());
        assert_eq!(expected_hash(&file, NAME).as_deref(), Some(HEX));
    }

    #[test]
    fn picks_the_line_that_names_our_file() {
        let other = "1111111111111111111111111111111111111111111111111111111111111111";
        let file = format!("{other}  kuvatin-x86_64.msi\n{HEX}  {NAME}\n");
        assert_eq!(expected_hash(&file, NAME).as_deref(), Some(HEX));
    }

    #[test]
    fn refuses_a_file_that_names_something_else() {
        let file = format!("{HEX}  some-other-build.msi\n");
        assert_eq!(expected_hash(&file, NAME), None);
    }

    #[test]
    fn refuses_a_digest_that_is_not_sixty_four_hex_characters() {
        assert_eq!(expected_hash(&format!("abc  {NAME}\n"), NAME), None);
        let long = format!("{HEX}00  {NAME}\n");
        assert_eq!(expected_hash(&long, NAME), None);
        let not_hex = format!("{}  {NAME}\n", "z".repeat(64));
        assert_eq!(expected_hash(&not_hex, NAME), None);
    }

    #[test]
    fn refuses_an_empty_or_garbage_file() {
        assert_eq!(expected_hash("", NAME), None);
        assert_eq!(expected_hash("<!doctype html>", NAME), None);
    }
}
```

Add to `crates/kuvatin/src/update/mod.rs`, under the module docs:

```rust
pub mod verify;
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin -- update::verify`
Expected: FAIL to compile, "cannot find function `expected_hash`".

- [ ] **Step 3: Implement**

Put above the test module in `crates/kuvatin/src/update/verify.rs`:

```rust
/// The digest a `sha256sum`-style file gives for `asset_name`, if it names it
/// and the digest is 64 hex characters. Lower-cased, so callers can compare
/// with `==`.
pub fn expected_hash(checksum_file: &str, asset_name: &str) -> Option<String> {
    checksum_file.lines().find_map(|line| {
        let (hex, name) = line.trim().split_once(char::is_whitespace)?;
        (name.trim() == asset_name).then_some(hex)?;
        let hex = hex.trim();
        let ok = hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit());
        ok.then(|| hex.to_ascii_lowercase())
    })
}
```

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test -p kuvatin -- update::verify`
Expected: 6 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "A checksum file only counts when it names the file we fetched"
```

---

### Task 3: Hash a file with the Windows cryptography API

**Files:**
- Modify: `crates/kuvatin/src/update/verify.rs`
- Modify: `crates/kuvatin/Cargo.toml` (add the `Win32_Security_Cryptography` feature)

- [ ] **Step 1: Add the Windows feature**

In `crates/kuvatin/Cargo.toml`, in the `features` list of the `windows`
dependency under `[target.'cfg(windows)'.dependencies]`, add the line:

```toml
    "Win32_Security_Cryptography",
```

- [ ] **Step 2: Write the failing tests**

Add to the `tests` module in `crates/kuvatin/src/update/verify.rs`:

```rust
    /// Known answers, so a wrong chunk loop or a wrong digest length is caught
    /// here rather than by a download that will not install.
    #[test]
    fn hashes_a_file_the_way_sha256_is_defined_to() {
        let dir = std::env::temp_dir().join(format!("kuvatin-hash-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");

        let empty = dir.join("empty.bin");
        std::fs::write(&empty, b"").expect("write");
        assert_eq!(
            sha256_file(&empty).expect("hash the empty file"),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );

        let abc = dir.join("abc.bin");
        std::fs::write(&abc, b"abc").expect("write");
        assert_eq!(
            sha256_file(&abc).expect("hash abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );

        // Bigger than one read buffer, so the chunk loop is actually exercised.
        let big = dir.join("big.bin");
        std::fs::write(&big, vec![0u8; 200_000]).expect("write");
        let digest = sha256_file(&big).expect("hash the big file");
        assert_eq!(digest.len(), 64);
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn says_so_when_the_file_is_not_there() {
        let missing = std::env::temp_dir().join("kuvatin-no-such-file-9e1f.bin");
        assert!(sha256_file(&missing).is_err());
    }
```

- [ ] **Step 3: Run them and watch them fail**

Run: `cargo test -p kuvatin -- update::verify::tests::hashes_a_file`
Expected: FAIL to compile, "cannot find function `sha256_file`".

- [ ] **Step 4: Implement**

Add to `crates/kuvatin/src/update/verify.rs`, above the tests:

```rust
use anyhow::{bail, Result};
use std::io::Read;
use std::path::Path;

/// Lower-case hex SHA-256 of a file, through CNG so no hashing crate is
/// needed. Read in chunks: the installer is tens of megabytes.
#[cfg(windows)]
pub fn sha256_file(path: &Path) -> Result<String> {
    use windows::Win32::Security::Cryptography::*;

    /// Closes whichever CNG handle it holds, however the function leaves.
    struct Alg(BCRYPT_ALG_HANDLE);
    impl Drop for Alg {
        fn drop(&mut self) {
            unsafe {
                let _ = BCryptCloseAlgorithmProvider(self.0, 0);
            }
        }
    }
    struct Hash(BCRYPT_HASH_HANDLE);
    impl Drop for Hash {
        fn drop(&mut self) {
            unsafe {
                let _ = BCryptDestroyHash(self.0);
            }
        }
    }

    let mut file = std::fs::File::open(path)?;
    unsafe {
        let mut alg = BCRYPT_ALG_HANDLE::default();
        BCryptOpenAlgorithmProvider(&mut alg, BCRYPT_SHA256_ALGORITHM, None, Default::default())
            .ok()?;
        let alg = Alg(alg);

        let mut hash = BCRYPT_HASH_HANDLE::default();
        BCryptCreateHash(alg.0, &mut hash, None, None, 0).ok()?;
        let hash = Hash(hash);

        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            BCryptHashData(hash.0, &buf[..n], 0).ok()?;
        }

        let mut digest = [0u8; 32];
        BCryptFinishHash(hash.0, &mut digest, 0).ok()?;
        Ok(digest.iter().map(|b| format!("{b:02x}")).collect())
    }
}

#[cfg(not(windows))]
pub fn sha256_file(_path: &Path) -> Result<String> {
    bail!("hashing is Windows-only")
}
```

- [ ] **Step 5: Run them and watch them pass**

Run: `cargo test -p kuvatin -- update::verify`
Expected: 8 passed.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "A downloaded file can be hashed without a hashing crate"
```

---

### Task 4: Download with progress and cancellation

**Files:**
- Create: `crates/kuvatin/src/update/fetch.rs`
- Modify: `crates/kuvatin/src/update/mod.rs` (add `pub mod fetch;`)

- [ ] **Step 1: Write the failing tests**

Create `crates/kuvatin/src/update/fetch.rs` with the test module only:

```rust
//! Fetching over HTTPS with WinHTTP, the way the check already talks to
//! GitHub: system proxy, system trust store, no extra crates.

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
        let known = Progress { done: 10, total: Some(100) };
        assert_eq!(known.fraction(), Some(0.1));
        let unknown = Progress { done: 10, total: None };
        assert_eq!(unknown.fraction(), None);
        // A server that lies about the length must not produce a fraction
        // above one; the bar would run off the end of the card.
        let over = Progress { done: 200, total: Some(100) };
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
```

Add `pub mod fetch;` next to `pub mod verify;` in `crates/kuvatin/src/update/mod.rs`.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin -- update::fetch`
Expected: FAIL to compile, "cannot find function `within_ceiling`".

- [ ] **Step 3: Implement**

Put above the tests in `crates/kuvatin/src/update/fetch.rs`:

```rust
use anyhow::{bail, Result};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// Nothing we publish comes near this. It stops a wrong address, or a server
/// that keeps talking, from filling the disk.
pub const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

/// How far a download has got.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub done: u64,
    /// `None` when the server sent no Content-Length.
    pub total: Option<u64>,
}

impl Progress {
    /// 0.0 to 1.0, or `None` when the size is unknown.
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
```

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test -p kuvatin -- update::fetch`
Expected: 2 passed, 1 ignored.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "The downloader knows how far it has got and when to stop"
```

- [ ] **Step 6: Add the two transfer functions**

Append to `crates/kuvatin/src/update/fetch.rs`, above the tests. This follows
`update::mod`'s `head_location` exactly: same `Handle` wrapper, same timeouts,
same agent string. The one difference is that redirects are **not** disabled,
because a release asset redirects to `objects.githubusercontent.com`.

```rust
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

    /// host, path-with-query, and whether it is https.
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
    /// connection handles ride along so they outlive the request.
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

    /// Read the whole body in 64 KiB chunks, handing each to `sink`.
    pub(super) fn drain(
        req: &Handle,
        ceiling: u64,
        mut sink: impl FnMut(&[u8]) -> Result<bool>,
    ) -> Result<u64> {
        let mut buf = vec![0u8; 64 * 1024];
        let mut done: u64 = 0;
        loop {
            let mut read: u32 = 0;
            unsafe { WinHttpReadData(req.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut read)? };
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
    let part = dest.with_extension("part");
    let outcome = (|| -> Result<bool> {
        let (_s, _c, req, total) = win::send(url)?;
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
#[cfg(windows)]
pub fn get_to_string(url: &str, limit: usize) -> Result<String> {
    let (_s, _c, req, _total) = win::send(url)?;
    let mut body: Vec<u8> = Vec::new();
    win::drain(&req, limit as u64, |chunk| {
        body.extend_from_slice(chunk);
        Ok(true)
    })?;
    Ok(String::from_utf8_lossy(&body).into_owned())
}

#[cfg(not(windows))]
pub fn get_to_file(
    _url: &str,
    _dest: &Path,
    _cancel: &AtomicBool,
    _on_progress: &mut dyn FnMut(Progress),
) -> Result<()> {
    bail!("downloading is Windows-only")
}

#[cfg(not(windows))]
pub fn get_to_string(_url: &str, _limit: usize) -> Result<String> {
    bail!("downloading is Windows-only")
}
```

- [ ] **Step 7: Prove it against the real release**

Run: `cargo test -p kuvatin -- --ignored update::fetch`
Expected: `reads_the_checksum_file_of_the_current_release` passes. If it fails
with an HTTP status, stop and report: the asset naming has changed and Task 1
needs revisiting.

- [ ] **Step 8: Commit**

```bash
git add -A
git commit -m "The update can fetch an asset and its checksum file"
```

---

### Task 5: The staging folder, and sweeping it

**Files:**
- Create: `crates/kuvatin/src/update/apply.rs`
- Modify: `crates/kuvatin/src/update/mod.rs` (add `pub mod apply;`)

- [ ] **Step 1: Write the failing tests**

Create `crates/kuvatin/src/update/apply.rs` with the test module only:

```rust
//! Staging an installer, handing off to a copy of this executable, and that
//! copy's own run. See the design doc for why the copy exists.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn staging_sits_inside_the_tree_the_uninstall_sweeps() {
        let dir = stage_dir().expect("a staging path");
        let tail: Vec<String> = dir
            .components()
            .rev()
            .take(3)
            .map(|c| c.as_os_str().to_string_lossy().to_ascii_lowercase())
            .collect();
        // %LOCALAPPDATA%\Temp\kuvatin\update — the uninstaller deletes the
        // whole Temp\kuvatin tree, so anything left here goes with it.
        assert_eq!(tail, vec!["update", "kuvatin", "temp"], "{dir:?}");
    }

    #[test]
    fn sweeping_removes_what_was_staged_and_shrugs_at_nothing() {
        let root = std::env::temp_dir().join(format!("kuvatin-sweep-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("deep")).expect("make the folder");
        std::fs::write(root.join("deep").join("kuvatin.exe"), b"not really").expect("write");

        sweep(&root);
        assert!(!root.exists(), "the staging folder should be gone");

        // Called again on the next start, with nothing there: still quiet.
        sweep(&root);
    }
}
```

Add `pub mod apply;` next to the other two in `crates/kuvatin/src/update/mod.rs`.

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin -- update::apply`
Expected: FAIL to compile, "cannot find function `stage_dir`".

- [ ] **Step 3: Implement**

Put above the tests in `crates/kuvatin/src/update/apply.rs`:

```rust
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Where a download waits to be installed. Under `%TEMP%\kuvatin`, which the
/// every-account uninstall deletes, so a machine that never runs Kuvatin
/// again is still left clean.
pub fn stage_dir() -> Result<PathBuf> {
    let temp = std::env::temp_dir();
    if temp.as_os_str().is_empty() {
        bail!("there is no temporary folder to stage the download in");
    }
    Ok(temp.join("kuvatin").join("update"))
}

/// Delete a staging folder, saying nothing if it is not there. The helper
/// cannot delete the copy it is running from, so this runs at the next start.
pub fn sweep(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => crate::applog::log(&format!("update: cleared {}", dir.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => crate::applog::log(&format!("update: could not clear {}: {e}", dir.display())),
    }
}

/// Clear the staging folder left by a previous update. Called once at start.
pub fn sweep_stage() {
    if let Ok(dir) = stage_dir() {
        sweep(&dir);
    }
}
```

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test -p kuvatin -- update::apply`
Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "A download is staged where the uninstall would find it"
```

---

### Task 6: Decide what an installer exit code means

**Files:**
- Modify: `crates/kuvatin/src/update/apply.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/kuvatin/src/update/apply.rs`:

```rust
    #[test]
    fn reads_what_msiexec_said() {
        assert_eq!(install_outcome(0), Installed::Yes);
        // 3010: installed, wants a reboot at the user's convenience.
        assert_eq!(install_outcome(3010), Installed::Yes);
        // 1602 and 1223: the elevation prompt was declined.
        assert_eq!(install_outcome(1602), Installed::Declined);
        assert_eq!(install_outcome(1223), Installed::Declined);
        assert_eq!(install_outcome(1603), Installed::Failed(1603));
        assert_eq!(install_outcome(1), Installed::Failed(1));
    }

    #[test]
    fn every_word_the_helper_can_print_is_ascii() {
        // The helper's lines end up in kuvatin.log and in a message box, and
        // the uninstall report taught us what a non-ASCII character does on
        // the way through Windows tooling.
        for code in [0u32, 3010, 1602, 1223, 1603, 7] {
            let said = describe(install_outcome(code));
            assert!(said.is_ascii(), "{said:?}");
        }
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin -- update::apply::tests::reads_what_msiexec_said`
Expected: FAIL to compile, "cannot find function `install_outcome`".

- [ ] **Step 3: Implement**

Add to `crates/kuvatin/src/update/apply.rs`:

```rust
/// What `msiexec` exiting with a given code means for us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Installed {
    Yes,
    /// The elevation prompt was declined. Nothing was changed.
    Declined,
    Failed(u32),
}

/// 3010 is "installed, reboot when you like", which is still installed. 1602
/// is "user cancelled" and 1223 is "the elevation prompt was refused".
pub fn install_outcome(code: u32) -> Installed {
    match code {
        0 | 3010 => Installed::Yes,
        1602 | 1223 => Installed::Declined,
        other => Installed::Failed(other),
    }
}

/// One ASCII line about an outcome, for the log and the message box.
pub fn describe(outcome: Installed) -> String {
    match outcome {
        Installed::Yes => "the update installed".to_string(),
        Installed::Declined => {
            "the update needs administrator rights, and the prompt was declined. \
             Kuvatin is unchanged."
                .to_string()
        }
        Installed::Failed(code) => format!(
            "the installer stopped with code {code}. Kuvatin is unchanged. \
             You can install by hand from {}",
            crate::update::RELEASES_URL
        ),
    }
}
```

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test -p kuvatin -- update::apply`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "The helper knows which installer exit codes mean it worked"
```

---

### Task 7: Stage a download: fetch, check, and put the helper beside it

**Files:**
- Modify: `crates/kuvatin/src/update/apply.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/kuvatin/src/update/apply.rs`:

```rust
    /// The rule that decides whether a downloaded file may be run, separated
    /// from the download so it can be tested without a network.
    #[test]
    fn a_file_is_only_accepted_when_it_matches_the_published_digest() {
        let dir = std::env::temp_dir().join(format!("kuvatin-accept-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let msi = dir.join("kuvatin-2.13.0-x86_64.msi");
        std::fs::write(&msi, b"abc").expect("write");
        let real = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

        let good = format!("{real}  kuvatin-2.13.0-x86_64.msi\n");
        accept(&msi, &good, "kuvatin-2.13.0-x86_64.msi").expect("the digest matches");

        let wrong = format!("{}  kuvatin-2.13.0-x86_64.msi\n", "0".repeat(64));
        let err = accept(&msi, &wrong, "kuvatin-2.13.0-x86_64.msi")
            .expect_err("a mismatch must not be accepted");
        assert!(format!("{err:#}").contains("did not arrive intact"), "{err:#}");
        assert!(!msi.exists(), "a file that failed its check must be deleted");

        // And a checksum file that never names our asset.
        std::fs::write(&msi, b"abc").expect("write again");
        let other = format!("{real}  something-else.msi\n");
        let err = accept(&msi, &other, "kuvatin-2.13.0-x86_64.msi").expect_err("wrong name");
        assert!(format!("{err:#}").contains("could not check"), "{err:#}");
        assert!(!msi.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p kuvatin -- update::apply::tests::a_file_is_only_accepted`
Expected: FAIL to compile, "cannot find function `accept`".

- [ ] **Step 3: Implement `accept` and `stage`**

Add to `crates/kuvatin/src/update/apply.rs`:

```rust
use super::{asset_name, asset_urls, fetch, verify};
use std::sync::atomic::AtomicBool;

/// What a finished download left behind.
#[derive(Debug, Clone)]
pub struct Staged {
    pub msi: PathBuf,
    pub helper: PathBuf,
}

/// Does this file match what the release says it should be? A file that fails
/// is deleted, so a later run cannot pick it up.
fn accept(msi: &Path, checksum_file: &str, name: &str) -> Result<()> {
    let expected = match verify::expected_hash(checksum_file, name) {
        Some(h) => h,
        None => {
            let _ = std::fs::remove_file(msi);
            bail!("could not check the download: the checksum file does not name {name}");
        }
    };
    let actual = match verify::sha256_file(msi) {
        Ok(a) => a,
        Err(e) => {
            let _ = std::fs::remove_file(msi);
            return Err(e).context("could not check the download");
        }
    };
    if actual != expected {
        let _ = std::fs::remove_file(msi);
        bail!("the download did not arrive intact");
    }
    Ok(())
}

/// Download `version`'s installer and its checksum, check it, and copy this
/// executable in beside it to do the installing. Anything it wrote is removed
/// if any step fails.
pub fn stage(
    version: &str,
    cancel: &AtomicBool,
    on_progress: &mut dyn FnMut(fetch::Progress),
) -> Result<Staged> {
    let dir = stage_dir()?;
    sweep(&dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("could not write to {}", dir.display()))?;

    let name = asset_name(version);
    let (msi_url, sha_url) = asset_urls(version);
    let msi = dir.join(&name);

    let staged = (|| -> Result<Staged> {
        fetch::get_to_file(&msi_url, &msi, cancel, on_progress)?;
        let checksum = fetch::get_to_string(&sha_url, 4096)?;
        accept(&msi, &checksum, &name)?;

        let running = std::env::current_exe().context("could not find this executable")?;
        let helper = dir.join("kuvatin-updater.exe");
        std::fs::copy(&running, &helper).with_context(|| {
            format!("could not copy this executable to {}", helper.display())
        })?;
        Ok(Staged { msi, helper })
    })();

    if staged.is_err() {
        sweep(&dir);
    }
    staged
}
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p kuvatin -- update::apply`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "A staged installer is one that matched its published digest"
```

---

### Task 8: The helper: wait, install, relaunch

**Files:**
- Modify: `crates/kuvatin/src/update/apply.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/kuvatin/src/update/apply.rs`:

```rust
    #[test]
    fn waiting_on_a_process_that_has_already_gone_is_success_not_failure() {
        // A pid that cannot be opened has exited (or never existed), which is
        // exactly the state the helper is waiting for.
        assert!(wait_for_exit(0xFFFF_FFF0, std::time::Duration::from_millis(50)));
    }

    #[test]
    fn waiting_on_ourselves_gives_up_at_the_deadline() {
        let started = std::time::Instant::now();
        let waited = wait_for_exit(std::process::id(), std::time::Duration::from_millis(200));
        assert!(!waited, "we are still running, so the wait must time out");
        assert!(started.elapsed() >= std::time::Duration::from_millis(150));
    }

    #[test]
    fn the_command_line_it_runs_names_the_installer_and_nothing_else() {
        let msi = Path::new(r"C:\Users\x\AppData\Local\Temp\kuvatin\update\k.msi");
        let args = msiexec_args(msi);
        assert_eq!(args[0], "/i");
        assert_eq!(Path::new(&args[1]), msi);
        assert!(args.contains(&"/qb".to_string()), "{args:?}");
        assert!(args.contains(&"/norestart".to_string()), "{args:?}");
        // No REINSTALLMODE: the package's MajorUpgrade handles an upgrade, and
        // forcing a reinstall mode here would fight it.
        assert!(!args.iter().any(|a| a.starts_with("REINSTALLMODE")), "{args:?}");
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin -- update::apply::tests::waiting_on_a_process`
Expected: FAIL to compile, "cannot find function `wait_for_exit`".

- [ ] **Step 3: Implement**

Add to `crates/kuvatin/src/update/apply.rs`:

```rust
use std::time::Duration;

/// How long the helper waits for the app to go before installing anyway.
const WAIT_FOR_APP: Duration = Duration::from_secs(30);

/// Wait for a process to exit. `true` when it has gone (including when it was
/// already gone), `false` when the deadline passed first.
#[cfg(windows)]
pub fn wait_for_exit(pid: u32, limit: Duration) -> bool {
    use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, SYNCHRONIZE};

    unsafe {
        let Ok(handle) = OpenProcess(SYNCHRONIZE, false, pid) else {
            // Not openable: it has exited, or it never existed. Either way
            // there is nothing left holding the files we are replacing.
            return true;
        };
        let waited = WaitForSingleObject(handle, limit.as_millis() as u32);
        let _ = CloseHandle(handle);
        waited == WAIT_OBJECT_0
    }
}

#[cfg(not(windows))]
pub fn wait_for_exit(_pid: u32, _limit: Duration) -> bool {
    true
}

/// `/qb` shows a small progress window, so an elevation prompt and a slow
/// install are not a silent freeze.
fn msiexec_args(msi: &Path) -> Vec<String> {
    vec![
        "/i".to_string(),
        msi.display().to_string(),
        "/qb".to_string(),
        "/norestart".to_string(),
    ]
}

/// Start the staged copy as the updater, then the caller quits. `relaunch` is
/// the executable to start afterwards, normally this one's own path.
pub fn hand_off(staged: &Staged, relaunch: &Path) -> Result<()> {
    std::process::Command::new(&staged.helper)
        .arg("--apply-update")
        .arg(&staged.msi)
        .arg("--after")
        .arg(std::process::id().to_string())
        .arg("--relaunch")
        .arg(relaunch)
        .spawn()
        .with_context(|| format!("could not start {}", staged.helper.display()))?;
    Ok(())
}

/// The `--apply-update` mode. Returns the process exit code: 0 when the
/// update installed, 1 when it did not.
pub fn run_helper(msi: &Path, after: u32, relaunch: Option<&Path>) -> i32 {
    crate::applog::log(&format!(
        "update: waiting for process {after}, then installing {}",
        msi.display()
    ));
    if !wait_for_exit(after, WAIT_FOR_APP) {
        crate::applog::log("update: gave up waiting; installing anyway");
    }

    let status = std::process::Command::new("msiexec")
        .args(msiexec_args(msi))
        .status();
    let outcome = match status {
        Ok(s) => install_outcome(s.code().unwrap_or(-1) as u32),
        Err(e) => {
            let said = format!("could not start the installer: {e}");
            crate::applog::log(&format!("update: {said}"));
            report_failure(&said);
            return 1;
        }
    };
    crate::applog::log(&format!("update: {}", describe(outcome)));

    if outcome != Installed::Yes {
        report_failure(&describe(outcome));
        // The app is gone, so put it back the way it was.
        if let Some(exe) = relaunch {
            let _ = std::process::Command::new(exe).spawn();
        }
        return 1;
    }

    let _ = std::fs::remove_file(msi);
    if let Some(exe) = relaunch {
        if let Err(e) = std::process::Command::new(exe).spawn() {
            let said = format!(
                "the update installed, but Kuvatin did not start again: {e}. \
                 Start it from the Start menu."
            );
            crate::applog::log(&format!("update: {said}"));
            report_failure(&said);
            return 1;
        }
    }
    0
}

/// No window is left by this point, so failures go to a message box as well
/// as the log.
#[cfg(windows)]
fn report_failure(text: &str) {
    use windows::core::HSTRING;
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
    let body = HSTRING::from(text);
    let title = HSTRING::from("Kuvatin update");
    unsafe {
        MessageBoxW(None, &body, &title, MB_OK | MB_ICONWARNING);
    }
}

#[cfg(not(windows))]
fn report_failure(text: &str) {
    eprintln!("{text}");
}
```

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test -p kuvatin -- update::apply`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "The staged copy waits for the app, installs, and starts it again"
```

---

### Task 9: The `--apply-update` mode

**Files:**
- Modify: `crates/kuvatin/src/cli.rs`
- Modify: `crates/kuvatin/src/main.rs`

- [ ] **Step 1: Write the failing tests**

Add to the `tests` module in `crates/kuvatin/src/cli.rs`:

```rust
    #[test]
    fn apply_update_carries_the_installer_the_pid_and_the_relaunch() {
        let mode = mode_of(&[
            "--apply-update",
            r"C:\tmp\k.msi",
            "--after",
            "4321",
            "--relaunch",
            r"C:\Program Files\Kuvatin\kuvatin.exe",
        ]);
        assert_eq!(
            mode,
            Mode::ApplyUpdate {
                msi: PathBuf::from(r"C:\tmp\k.msi"),
                after: 4321,
                relaunch: Some(PathBuf::from(r"C:\Program Files\Kuvatin\kuvatin.exe")),
            }
        );
    }

    #[test]
    fn apply_update_can_be_told_not_to_start_anything_afterwards() {
        // What the pipeline test uses: install, then stop, so no window opens
        // on the runner.
        let mode = mode_of(&["--apply-update", r"C:\tmp\k.msi", "--after", "1"]);
        assert_eq!(
            mode,
            Mode::ApplyUpdate {
                msi: PathBuf::from(r"C:\tmp\k.msi"),
                after: 1,
                relaunch: None,
            }
        );
    }

    #[test]
    fn apply_update_needs_the_process_it_waits_for() {
        assert!(parse_err(&["--apply-update", r"C:\tmp\k.msi"]));
    }
```

- [ ] **Step 2: Run them and watch them fail**

Run: `cargo test -p kuvatin -- cli::tests::apply_update`
Expected: FAIL to compile, "no variant named `ApplyUpdate`".

- [ ] **Step 3: Implement the flags**

In `crates/kuvatin/src/cli.rs`, add to `struct Cli` after `unregister_all_users`:

```rust
    /// Install an update and exit. Kuvatin starts a copy of itself with this
    /// while it closes: an installer cannot replace a running executable.
    #[arg(
        long,
        value_name = "MSI",
        requires = "after",
        conflicts_with_all = ["register", "unregister", "unregister_all_users", "preset", "sequence_mp4", "print_extensions"]
    )]
    pub apply_update: Option<PathBuf>,

    /// The process id --apply-update waits for before installing.
    #[arg(long, value_name = "PID", requires = "apply_update")]
    pub after: Option<u32>,

    /// What --apply-update starts once the install is done. Left out, it
    /// installs and stops.
    #[arg(long, value_name = "EXE", requires = "apply_update")]
    pub relaunch: Option<PathBuf>,
```

Add to `enum Mode`:

```rust
    /// Install a staged update. This runs from a copy of the executable in
    /// the staging folder, never from the installed path.
    ApplyUpdate {
        msi: PathBuf,
        after: u32,
        relaunch: Option<PathBuf>,
    },
```

Add to `into_mode`, as the first branch after `unregister_all_users`:

```rust
        } else if let Some(msi) = self.apply_update {
            match self.after {
                Some(after) => Mode::ApplyUpdate {
                    msi,
                    after,
                    relaunch: self.relaunch,
                },
                None => Mode::Invalid("--apply-update needs --after"),
            }
```

- [ ] **Step 4: Run them and watch them pass**

Run: `cargo test -p kuvatin -- cli::`
Expected: every cli test passes.

- [ ] **Step 5: Dispatch it**

In `crates/kuvatin/src/main.rs`, add a branch to the `match` over `mode`,
**after** `applog::install_panic_hook()` (unlike `UnregisterAllUsers`, this one
runs as a normal user in a normal profile, so logging and the panic hook are
wanted):

```rust
        Mode::ApplyUpdate {
            msi,
            after,
            relaunch,
        } => std::process::exit(update::apply::run_helper(&msi, after, relaunch.as_deref())),
```

- [ ] **Step 6: Run the suite**

Run: `cargo test -p kuvatin`
Expected: everything passes.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "A copy of Kuvatin can be asked to install an update"
```

---

### Task 10: Let the interface ask whether there is unsaved work

**Files:**
- Modify: `crates/kuvatin-video/src/project.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/kuvatin-video/src/project.rs`:

```rust
    #[test]
    fn a_project_says_whether_it_has_unsaved_changes() {
        let mut project = Project::new(|_f| {}).expect("project");
        assert!(!project.is_dirty(), "a new project has nothing to lose");
        project.set_canvas_size(1280, 720);
        assert!(project.is_dirty(), "an edit is an unsaved change");
    }
```

- [ ] **Step 2: Run it and watch it fail**

Run (GStreamer on PATH): `cargo test -p kuvatin-video -- a_project_says_whether`
Expected: FAIL to compile, "no method named `is_dirty`".

- [ ] **Step 3: Implement**

Next to the other accessors on `Project` in `crates/kuvatin-video/src/project.rs`:

```rust
    /// Whether anything has changed since the last save. The flag is already
    /// kept for the project file; this lets the window ask before it closes
    /// itself for an update.
    pub fn is_dirty(&self) -> bool {
        self.dirty.get()
    }
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p kuvatin-video -- a_project_says_whether`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "A project can say whether it has unsaved changes"
```

---

### Task 11: The dialog

**Files:**
- Modify: `crates/kuvatin/ui/app.slint`

- [ ] **Step 1: Add the properties and callbacks**

Next to `update-available` (around `crates/kuvatin/ui/app.slint:289`):

```slint
    // 0 none, 1 confirm, 2 working, 3 failed.
    in-out property <int> update-phase: 0;
    // 0..1, or negative when the server did not say how big the file is.
    in property <float> update-progress: -1;
    in property <string> update-step: "";
    in property <string> update-error: "";
    in property <bool> update-unsaved: false;
    callback update-open();
    callback update-start();
    callback update-cancel();
    callback update-save-first();
```

- [ ] **Step 2: Point the badge at the dialog**

Replace the badge's action and label (around `crates/kuvatin/ui/app.slint:537`
and `:548`):

```slint
                    accessible-label: "Update available: version " + root.update-available + ". Opens the update dialog.";
```

```slint
                        clicked => { root.update-open(); }
```

- [ ] **Step 3: Add the dialog**

Beside the other modals (after the `confirm-open-project` block):

```slint
        if root.update-phase == 1 : Modal {
            card-width: 430px; card-height: root.update-unsaved ? 224px : 190px; card-spacing: 12px;
            title: "Update Kuvatin?";
            default-action => { root.update-phase = 2; root.update-start(); }
            Text {
                text: "Kuvatin " + root.update-available + " is available";
                color: Theme.ink; font-size: 15px; font-weight: 700;
            }
            Text {
                text: "You are running " + root.app-version + ". Kuvatin will close, install the update, and open again. Windows will ask for administrator rights once.";
                color: Theme.muted; font-size: 12px; wrap: word-wrap; vertical-stretch: 1;
            }
            if root.update-unsaved : Text {
                text: "The timeline has changes that have not been saved. Closing loses them.";
                color: Theme.danger; font-size: 12px; wrap: word-wrap;
            }
            HorizontalLayout {
                spacing: 10px;
                DialogButton { text: "What is new"; clicked => { root.open-releases(); } }
                Rectangle { horizontal-stretch: 1; }
                DialogButton { text: "Not now"; clicked => { root.update-phase = 0; } }
                if root.update-unsaved : DialogButton {
                    text: "Save first…"; min-width: 104px;
                    clicked => { root.update-phase = 0; root.update-save-first(); }
                }
                DialogButton {
                    text: "Update"; primary: true; min-width: 92px;
                    clicked => { root.update-phase = 2; root.update-start(); }
                }
            }
        }

        if root.update-phase == 2 : Modal {
            card-width: 430px; card-height: 156px; card-spacing: 12px;
            title: "Updating Kuvatin";
            // No default-action: a progress dialog must not be dismissed by
            // leaning on Return.
            Text { text: "Updating Kuvatin"; color: Theme.ink; font-size: 15px; font-weight: 700; }
            Text {
                text: root.update-step; color: Theme.muted; font-size: 12px;
                wrap: word-wrap; vertical-stretch: 1;
            }
            Rectangle {
                height: 6px; border-radius: 3px; background: Theme.well;
                Rectangle {
                    x: 0; height: parent.height; border-radius: 3px; background: Theme.accent;
                    width: root.update-progress < 0 ? parent.width : parent.width * root.update-progress;
                    opacity: root.update-progress < 0 ? 0.45 : 1.0;
                }
            }
            HorizontalLayout {
                spacing: 10px;
                Rectangle { horizontal-stretch: 1; }
                DialogButton { text: "Cancel"; clicked => { root.update-cancel(); } }
            }
        }

        if root.update-phase == 3 : Modal {
            card-width: 430px; card-height: 186px; card-spacing: 12px;
            title: "The update did not go through";
            default-action => { root.update-phase = 0; }
            Text {
                text: "The update did not go through";
                color: Theme.ink; font-size: 15px; font-weight: 700;
            }
            Text {
                text: root.update-error; color: Theme.muted; font-size: 12px;
                wrap: word-wrap; vertical-stretch: 1;
            }
            HorizontalLayout {
                spacing: 10px;
                Rectangle { horizontal-stretch: 1; }
                DialogButton { text: "Close"; clicked => { root.update-phase = 0; } }
                DialogButton {
                    text: "Download page"; primary: true; min-width: 120px;
                    clicked => { root.update-phase = 0; root.open-releases(); }
                }
            }
        }
```

- [ ] **Step 4: Make Escape close the dialogs that may be closed**

In the window's key handler (around `crates/kuvatin/ui/app.slint:334`), add
alongside the other modal cases, and **before** any catch-all:

```slint
        if (root.update-phase == 1 || root.update-phase == 3) { root.update-phase = 0; return EventResult.accept; }
```

Escape is deliberately not wired for phase 2: a download in flight is stopped
with Cancel, which also stops the worker.

- [ ] **Step 5: Add the one property the dialog needs**

`app-version` does not exist yet. Add it next to `update-available`:

```slint
    in property <string> app-version;
```

Task 12 sets it from `update::CURRENT`.

The colours above are ones the palette already has. It carries no amber, so
the unsaved-changes line uses `Theme.danger`, which is what this palette means
by "you are about to lose something", and the progress groove uses
`Theme.well`, the sunken-surface colour. Do not add a colour to
`crates/kuvatin/ui/theme.slint` for this dialog.

- [ ] **Step 6: Build the interface**

Run: `cargo build -p kuvatin`
Expected: compiles. Slint errors name the line; fix them before moving on.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "The update badge opens a dialog instead of a web page"
```

---

### Task 12: Wire the dialog to the work

**Files:**
- Modify: `crates/kuvatin/src/gui/updates.rs`
- Modify: `crates/kuvatin/src/gui/mod.rs` (only if `wire` needs the project handle)

- [ ] **Step 1: Sweep at startup and show the running version**

At the top of `wire` in `crates/kuvatin/src/gui/updates.rs`:

```rust
    // Whatever a previous update left staged, including the copy of the
    // executable that did the installing.
    crate::update::apply::sweep_stage();
    ui.set_app_version(update::CURRENT.into());
```

- [ ] **Step 2: Open the dialog**

Add to `wire`, after the `on_check_updates_now` block:

```rust
    {
        let ui_weak = ui.as_weak();
        ui.on_update_open(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_update_error("".into());
                ui.set_update_phase(1);
            }
        });
    }
```

- [ ] **Step 3: Start, report and hand off**

Add to `wire`:

```rust
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let settings = settings.clone();
        let cancel = cancel.clone();
        let ui_weak = ui.as_weak();
        ui.on_update_start(move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let version = settings.lock().unwrap().latest_seen.clone();
            if version.is_empty() {
                fail(&ui, "there is no version to install".into());
                return;
            }
            cancel.store(false, Ordering::Relaxed);
            ui.set_update_step("Starting the download".into());
            ui.set_update_progress(-1.0);

            let cancel = cancel.clone();
            let ui_weak = ui.as_weak();
            std::thread::spawn(move || {
                let progress_ui = ui_weak.clone();
                let mut on_progress = move |p: update::fetch::Progress| {
                    let step = match p.total {
                        Some(total) => format!(
                            "Downloading {:.1} of {:.1} MB",
                            p.done as f64 / 1_048_576.0,
                            total as f64 / 1_048_576.0
                        ),
                        None => format!("Downloading {:.1} MB", p.done as f64 / 1_048_576.0),
                    };
                    let fraction = p.fraction().unwrap_or(-1.0);
                    let _ = progress_ui.upgrade_in_event_loop(move |ui| {
                        ui.set_update_step(step.into());
                        ui.set_update_progress(fraction);
                    });
                };
                let staged = update::apply::stage(&version, &cancel, &mut on_progress);
                let _ = ui_weak.upgrade_in_event_loop(move |ui| match staged {
                    Ok(staged) => {
                        ui.set_update_step("Closing to install".into());
                        let exe = std::env::current_exe().unwrap_or_default();
                        match update::apply::hand_off(&staged, &exe) {
                            Ok(()) => {
                                crate::applog::log("update: handed off to the staged copy");
                                let _ = slint::quit_event_loop();
                            }
                            Err(e) => fail(&ui, format!("{e:#}")),
                        }
                    }
                    Err(e) => {
                        let said = format!("{e:#}");
                        if said.contains("cancelled") {
                            ui.set_update_phase(0);
                        } else {
                            crate::applog::log(&format!("update failed: {said}"));
                            fail(&ui, said);
                        }
                    }
                });
            });
        });
    }
    {
        let cancel = cancel.clone();
        let ui_weak = ui.as_weak();
        ui.on_update_cancel(move || {
            cancel.store(true, Ordering::Relaxed);
            if let Some(ui) = ui_weak.upgrade() {
                ui.set_update_step("Stopping".into());
            }
        });
    }
```

And the small helper, next to `apply_status`:

```rust
/// Put the dialog into its failed state with a sentence the user can act on.
fn fail(ui: &AppWindow, message: String) {
    ui.set_update_error(message.into());
    ui.set_update_phase(3);
}
```

Add the imports at the top of the file:

```rust
use std::sync::atomic::{AtomicBool, Ordering};
```

- [ ] **Step 4: Wire "Save first" and the unsaved flag**

`update-save-first` reuses what the window already does. In
`crates/kuvatin/src/gui/updates.rs`:

```rust
    {
        let ui_weak = ui.as_weak();
        ui.on_update_save_first(move || {
            if let Some(ui) = ui_weak.upgrade() {
                ui.invoke_video_save_project(true);
            }
        });
    }
```

`update-unsaved` is set where the dialog opens, from the project handle the
video wiring owns. In `on_update_open`, before setting the phase:

```rust
                ui.set_update_unsaved(crate::gui::video::has_unsaved_changes());
```

If no such accessor exists, add one to the video wiring module that reads the
shared `Project` through the handle it already holds and returns
`project.is_dirty()`, defaulting to `false` when the project is not created
yet. Do not reach into the project from this module directly.

- [ ] **Step 5: Build and run the suite**

Run: `cargo test -p kuvatin` and `cargo build -p kuvatin`
Expected: both clean.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "Clicking Update downloads it, checks it, and hands over"
```

---

### Task 13: Keep the helper's words ASCII

**Files:**
- Modify: `crates/kuvatin/src/update/apply.rs`

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `crates/kuvatin/src/update/apply.rs`:

```rust
    /// Same guard as the uninstall report, for the same reason: these lines
    /// reach a log and a message box through Windows tooling that does not
    /// always read UTF-8.
    #[test]
    fn every_message_this_module_can_produce_is_ascii() {
        let dir = std::env::temp_dir().join("kuvatin-ascii-check");
        let msi = dir.join("kuvatin-9.9.9-x86_64.msi");
        let mut said: Vec<String> = Vec::new();
        for code in [0u32, 3010, 1602, 1223, 1603] {
            said.push(describe(install_outcome(code)));
        }
        said.push(
            accept(&msi, "", "kuvatin-9.9.9-x86_64.msi")
                .expect_err("no checksum")
                .to_string(),
        );
        said.push(format!("{:#}", stage_dir().map(|d| d.display().to_string()).unwrap_or_default()));
        for line in &said {
            assert!(line.is_ascii(), "{line:?}");
        }
    }
```

- [ ] **Step 2: Run it**

Run: `cargo test -p kuvatin -- update::apply::tests::every_message`
Expected: PASS if the earlier tasks used ASCII, FAIL naming the offender if
not. Fix the string rather than the test.

Note: the staging path contains the user's name, which can hold any character,
so the assertion above only checks the part this module writes. If the path
line fails on a machine whose user name is not ASCII, drop that one line from
the test and leave a comment saying why.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "The update messages stay ASCII, like the uninstall report"
```

---

### Task 14: Prove the real path on the runner

**Files:**
- Modify: `.github/workflows/release.yml`

- [ ] **Step 1: Find where the installer is already installed**

Run: `grep -n "msiexec" .github/workflows/release.yml`
Expected: the install step near line 508 and the uninstall test near line 792.
The new step goes **after** the install-and-verify step and **before** the
uninstall test, so it runs against a real installed product.

- [ ] **Step 2: Add the step**

Insert into the `build` job, after the step that installs the MSI and checks
the menu registered:

```yaml
      - name: Update test (the staged helper installs over a real install)
        if: env.PACKAGE == 'true'
        shell: pwsh
        run: |
          # The app copies itself into a staging folder and lets the copy do
          # the installing, because an installer cannot replace a running
          # executable. This drives that copy against the installer we just
          # built: same version, so MajorUpgrade's AllowSameVersionUpgrades
          # is what makes it a legal reinstall.
          $msi = (Get-ChildItem target\wix\kuvatin-*-x86_64.msi | Select-Object -First 1).FullName
          $installed = "$env:ProgramFiles\Kuvatin\kuvatin.exe"
          if (-not (Test-Path $installed)) { throw "no installed kuvatin.exe to copy: the install step did not run" }
          $stage = Join-Path $env:TEMP 'kuvatin\update'
          New-Item -ItemType Directory -Force $stage | Out-Null
          $helper = Join-Path $stage 'kuvatin-updater.exe'
          Copy-Item $installed $helper -Force

          # --after wants a process that has already exited, which is what the
          # helper will find in the real flow by the time it looks.
          $dead = Start-Process cmd -ArgumentList '/c','exit' -PassThru -Wait
          # No --relaunch: nothing should open a window on the runner.
          $p = Start-Process $helper -ArgumentList '--apply-update',"`"$msi`"",'--after',$dead.Id -PassThru -Wait
          if ($p.ExitCode -ne 0) { throw "the staged updater exited $($p.ExitCode)" }

          if (-not (Test-Path $installed)) { throw 'the update removed the app instead of replacing it' }
          $product = Get-CimInstance Win32_Product -Filter "Name='Kuvatin'" -ErrorAction SilentlyContinue
          if (-not $product) { throw 'Kuvatin is no longer registered as installed' }
          Write-Host "updater ran clean; Kuvatin $($product.Version) is installed"

          # It deletes the installer it used and leaves its own copy for the
          # app to sweep on next start.
          if (Test-Path $msi) { Write-Host 'note: the built MSI is still in target\wix (the helper copies, it does not move)' }
          Get-Content "$env:LOCALAPPDATA\Kuvatin\kuvatin.log" -ErrorAction SilentlyContinue |
            Select-String 'update:' | Select-Object -Last 6
```

- [ ] **Step 3: Check the workflow parses**

Run: `python -c "import yaml,io; yaml.safe_load(io.open('.github/workflows/release.yml', encoding='utf-8')); print('yaml ok')"`
Expected: `yaml ok`.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "The pipeline drives the staged updater against a real install"
```

---

### Task 15: Say so in the changelog and the README

**Files:**
- Modify: `CHANGELOG.md`
- Modify: `README.md`

- [ ] **Step 1: Add the changelog entry**

Under `## [Unreleased]`, in the `### Added` section (create it above `### Fixed`
if it is not there):

```markdown
- **Updates install themselves.** Clicking the update badge now offers to
  install the new version instead of opening a web page: Kuvatin downloads the
  installer, checks it against the checksum published with it, then closes,
  installs and opens again. The update check is still opt-in and still the
  only thing that reaches the network unless you ask.
```

- [ ] **Step 2: Note what the check does and does not prove**

In `README.md`, in the section describing the update check (find it with
`grep -n "update check" README.md`), add:

```markdown
The installer is downloaded over HTTPS and checked against the `.sha256`
published beside it. That proves the download arrived intact. It is not proof
that the release is genuine: the installer is unsigned and the checksum sits
beside it on the same server. Signing the installer is a separate job.
```

- [ ] **Step 3: Run the whole suite one last time**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p kuvatin -p kuvatin-core
cargo test -p kuvatin-video -- removing_a_clip_straight_after_adding_it_does_not_crash removing_a_clip_added_while_playing_does_not_crash removing_two_clips_back_to_back_while_playing_does_not_crash
```

Expected: all clean.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "The changelog and README describe the in-app update"
```

---

## Self-review notes

- **Spec coverage.** Check (Task 1), download with progress and cancel
  (Task 4), checksum reading and hashing (Tasks 2 and 3), staging and sweeping
  (Task 5), exit-code rule (Task 6), verify-before-run (Task 7), the helper
  (Task 8), the mode (Task 9), the dirty flag (Task 10), the dialog and its
  three phases (Task 11), the wiring including save-first (Task 12), ASCII
  (Task 13), the pipeline proof (Task 14), documentation (Task 15).
- **Names used consistently:** `asset_urls`, `asset_name`, `expected_hash`,
  `sha256_file`, `Progress`, `get_to_file`, `get_to_string`, `stage_dir`,
  `sweep`, `sweep_stage`, `Staged`, `accept`, `stage`, `install_outcome`,
  `Installed`, `describe`, `wait_for_exit`, `msiexec_args`, `hand_off`,
  `run_helper`, `Mode::ApplyUpdate`, `update-phase`, `update-progress`,
  `update-step`, `update-error`, `update-unsaved`, `update-open`,
  `update-start`, `update-cancel`, `update-save-first`.
- **Known soft spot.** Task 12 needs an accessor for "does the project have
  unsaved changes" that the updates module can call without reaching into the
  video wiring's internals. If one does not exist, Task 12 Step 4 says to add
  it there rather than widen this module.

---

### Task 16: Open the window 20% larger, without running off the screen

Unrelated to the update, added to this plan at the user's request: the window
opens too small for its contents.

**Files:**
- Modify: `crates/kuvatin/ui/app.slint:52-53`
- Modify: `crates/kuvatin/src/gui/mod.rs` (around `AppWindow::new()`, line 111)

- [ ] **Step 1: Write the failing test for the fitting rule**

The size that actually opens is a pure decision over two rectangles, so test
that rather than the window. Add a new module
`crates/kuvatin/src/gui/window_size.rs` holding only its tests to start:

```rust
//! How big the window opens: what it asks for, unless the desktop is smaller.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asks_for_the_preferred_size_when_the_desktop_has_room() {
        assert_eq!(opening_size(1584, 1008, 2560, 1400), (1584, 1008));
    }

    #[test]
    fn shrinks_to_the_work_area_rather_than_hanging_off_it() {
        // 1920x1080 with a taskbar: the work area is shorter than the window
        // wants, and a window taller than the desktop cannot be moved back
        // into view by dragging its title bar.
        assert_eq!(opening_size(1584, 1008, 1920, 1032), (1584, 1032));
        assert_eq!(opening_size(1584, 1008, 1366, 768), (1366, 768));
    }

    #[test]
    fn never_goes_below_what_the_window_can_be_dragged_to() {
        // The floor is app.slint's min-width / min-height.
        assert_eq!(opening_size(1584, 1008, 400, 300), (MIN_W, MIN_H));
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -p kuvatin -- window_size`
Expected: FAIL to compile, "cannot find function `opening_size`".

- [ ] **Step 3: Implement the rule**

Above the tests in `crates/kuvatin/src/gui/window_size.rs`:

```rust
/// app.slint's `min-width` / `min-height`. Below this the layout breaks, so
/// a small desktop gets a window it can scroll rather than one it cannot use.
pub const MIN_W: u32 = 860;
pub const MIN_H: u32 = 560;

/// What the window should open at: what it asked for, capped to the desktop's
/// usable area, floored at the size the layout needs.
pub fn opening_size(want_w: u32, want_h: u32, area_w: u32, area_h: u32) -> (u32, u32) {
    (
        want_w.min(area_w).max(MIN_W),
        want_h.min(area_h).max(MIN_H),
    )
}
```

Register it in `crates/kuvatin/src/gui/mod.rs` next to the other submodules:

```rust
mod window_size;
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -p kuvatin -- window_size`
Expected: 3 passed.

- [ ] **Step 5: Ask for 20% more in the interface**

In `crates/kuvatin/ui/app.slint`, lines 52-53:

```slint
    preferred-width: 1584px;
    preferred-height: 1008px;
```

Leave `min-width` and `min-height` alone: they say how small the window may be
dragged, and raising them would lock out smaller screens.

- [ ] **Step 6: Apply the cap at startup**

In `crates/kuvatin/src/gui/mod.rs`, right after `let ui = AppWindow::new()?;`:

```rust
    // preferred-width/height in app.slint is what we ask for. A 1080p desktop
    // has less usable height than that once the taskbar is out, and a window
    // taller than the desktop cannot be dragged back into view.
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::RECT;
        use windows::Win32::UI::WindowsAndMessaging::{
            SystemParametersInfoW, SPI_GETWORKAREA, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
        };
        let mut area = RECT::default();
        let got = unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&mut area as *mut RECT).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        };
        if got.is_ok() {
            let (aw, ah) = (
                (area.right - area.left).max(0) as u32,
                (area.bottom - area.top).max(0) as u32,
            );
            let (w, h) = window_size::opening_size(1584, 1008, aw, ah);
            ui.window()
                .set_size(slint::LogicalSize::new(w as f32, h as f32));
        }
    }
```

- [ ] **Step 7: Look at it**

Run: `cargo run -p kuvatin`
Expected: the window opens noticeably larger than before and fully on screen,
with its bottom edge above the taskbar. Close it.

- [ ] **Step 8: Note it in the changelog**

Under `## [Unreleased]`, in `### Fixed`:

```markdown
- The window opens larger, so the timeline and the inspector both fit without
  resizing it first. On a screen too small for that it opens as large as the
  desktop allows.
```

- [ ] **Step 9: Commit**

```bash
git add -A
git commit -m "The window opens large enough for its contents"
```
