//! What a download has to prove before it is allowed to run.

/// The digest a `sha256sum`-style file gives for `asset_name`, if it names it
/// and the digest is 64 hex characters. Lower-cased, so callers can compare
/// with `==`.
#[allow(dead_code)] // Only the tests call this so far; the downloader is next.
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
}
