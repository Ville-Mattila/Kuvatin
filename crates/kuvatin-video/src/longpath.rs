//! Paths past Windows' old 260-character limit.
//!
//! GStreamer opens files through the C runtime, which refuses a path of
//! MAX_PATH or more unless the process opted in to long paths and the machine
//! allows them; the discoverer then says only "Resource not found". A long
//! path is handed over in its 8.3 short form instead, which names the same
//! file in far fewer characters, and turned back into its long form wherever
//! a URI becomes a path a person sees again (a reopened project's media bin).
//! Where a volume keeps no short names the path is passed on unchanged, so
//! nothing that worked before stops working.

use std::path::{Path, PathBuf};

/// Paths shorter than this go through untouched. MAX_PATH is 260 with the
/// terminating NUL; a directory has to leave room for an 8.3 name besides
/// (248), so anything from there on is shortened.
#[cfg(windows)]
const LIMIT: usize = 248;
/// The C runtime's own limit, which GStreamer's file elements meet.
#[cfg(windows)]
const MAX_PATH: usize = 260;

/// The `file://` URI GStreamer can open for `path`.
pub fn file_uri(path: &Path) -> Result<String, gstreamer::glib::Error> {
    gstreamer::glib::filename_to_uri(openable(path), None).map(|u| u.to_string())
}

/// `path` in a form the C runtime can open: itself when it is short enough,
/// else its 8.3 short form. A file that does not exist yet (an export's
/// output) has its folder shortened and its own name kept.
pub fn openable(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if utf16_len(path) < LIMIT {
            return path.to_path_buf();
        }
        // A shortened form still past the limit (a volume that keeps 8.3
        // names for only some of the folders, or none) helps nothing.
        let fits = |p: &PathBuf| utf16_len(p) < MAX_PATH;
        if let Some(short) = win::short(path).filter(fits) {
            return short;
        }
        if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
            if let Some(short) = win::short(dir).map(|d| d.join(name)).filter(fits) {
                return short;
            }
        }
        path.to_path_buf()
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// The long form of a path that may have been shortened by [`openable`]:
/// what the user named, for anything shown to them. Unchanged when it holds
/// no short names or no longer exists.
pub fn expanded(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        if !path.as_os_str().to_string_lossy().contains('~') {
            return path;
        }
        win::long(&path).unwrap_or(path)
    }
    #[cfg(not(windows))]
    {
        path
    }
}

#[cfg(windows)]
fn utf16_len(path: &Path) -> usize {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str().encode_wide().count()
}

#[cfg(windows)]
mod win {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::{Path, PathBuf};

    #[link(name = "kernel32")]
    extern "system" {
        fn GetShortPathNameW(long: *const u16, short: *mut u16, len: u32) -> u32;
        fn GetLongPathNameW(short: *const u16, long: *mut u16, len: u32) -> u32;
    }

    /// The `\\?\` form, which the path functions accept past MAX_PATH. It
    /// takes the path literally, so `.` and `..` are resolved first.
    fn verbatim(path: &Path) -> Vec<u16> {
        let full = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let s = full.as_os_str().to_string_lossy();
        let v = if s.starts_with(r"\\?\") {
            s.into_owned()
        } else if let Some(unc) = s.strip_prefix(r"\\") {
            format!(r"\\?\UNC\{unc}")
        } else {
            format!(r"\\?\{s}")
        };
        std::ffi::OsStr::new(&v)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Back from the `\\?\` form to the one the C runtime and GLib expect.
    fn plain(wide: &[u16]) -> PathBuf {
        let s = OsString::from_wide(wide).to_string_lossy().into_owned();
        let s = if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else if let Some(rest) = s.strip_prefix(r"\\?\") {
            rest.to_string()
        } else {
            s
        };
        PathBuf::from(s)
    }

    type Convert = unsafe extern "system" fn(*const u16, *mut u16, u32) -> u32;

    fn call(f: Convert, path: &Path) -> Option<PathBuf> {
        let input = verbatim(path);
        let mut buf = vec![0u16; 512];
        loop {
            // SAFETY: `input` is NUL-terminated and `buf` holds `buf.len()`
            // u16s, the size passed.
            let n = unsafe { f(input.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
            if n == 0 {
                return None; // no such path, or no short names on the volume
            }
            if n < buf.len() {
                return Some(plain(&buf[..n]));
            }
            buf.resize(n + 1, 0); // n is the size needed, NUL included
        }
    }

    /// The 8.3 form, or None when the volume keeps no short names (the call
    /// then hands back the long path unchanged, which helps nothing).
    pub(super) fn short(path: &Path) -> Option<PathBuf> {
        let s = call(GetShortPathNameW, path)?;
        (super::utf16_len(&s) < super::utf16_len(path)).then_some(s)
    }

    pub(super) fn long(path: &Path) -> Option<PathBuf> {
        call(GetLongPathNameW, path)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// A folder whose path is well past 260 characters, made the way any
    /// program would (std adds the `\\?\` form itself).
    fn deep_dir(tag: &str) -> PathBuf {
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-tmp")
            .join(format!("{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base
            .join("a".repeat(100))
            .join("b".repeat(100))
            .join("c".repeat(100));
        std::fs::create_dir_all(&dir).expect("deep dir");
        dir
    }

    #[test]
    fn short_paths_pass_through_untouched() {
        let p = Path::new(r"C:\clips\IMG_0001.MOV");
        assert_eq!(openable(p), p);
        assert_eq!(expanded(p.to_path_buf()), p);
    }

    #[test]
    fn a_long_path_is_shortened_and_expanded_back() {
        let dir = deep_dir("longpath-roundtrip");
        let file = dir.join("a clip with a long name.png");
        std::fs::write(&file, b"x").expect("write");
        let open = openable(&file);
        if open == file {
            eprintln!("this volume keeps no 8.3 names; nothing to shorten");
            return;
        }
        assert!(utf16_len(&open) < 260, "{}", open.display());
        assert_eq!(
            expanded(open.clone()),
            file.canonicalize()
                .map(|p| { PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\")) })
                .unwrap()
        );
        // An export target that isn't there yet keeps its own name.
        let out = dir.join("export.mp4");
        let open_out = openable(&out);
        assert_eq!(open_out.file_name(), out.file_name());
        assert!(utf16_len(&open_out) < 260, "{}", open_out.display());
    }
}
