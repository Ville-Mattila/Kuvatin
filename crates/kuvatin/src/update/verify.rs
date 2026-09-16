//! What a download has to prove before it is allowed to run.

use anyhow::Result;
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
    anyhow::bail!("hashing is Windows-only")
}

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
        assert!(digest
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn says_so_when_the_file_is_not_there() {
        let missing = std::env::temp_dir().join("kuvatin-no-such-file-9e1f.bin");
        assert!(sha256_file(&missing).is_err());
    }
}
