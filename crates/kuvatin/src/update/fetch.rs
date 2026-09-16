//! Fetching over HTTPS with WinHTTP, the way the check already talks to
//! GitHub: system proxy, system trust store, no extra crates.

use anyhow::{bail, Result};

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

#[allow(dead_code)] // Only the tests call this so far; the transfers are next.
fn within_ceiling(bytes: u64, ceiling: u64) -> Result<()> {
    if bytes > ceiling {
        bail!("the download is too large ({bytes} bytes, limit {ceiling})");
    }
    Ok(())
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
}
