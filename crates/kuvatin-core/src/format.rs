use serde::{Deserialize, Serialize};

/// File extensions accepted as image INPUTS, lower-case, without the dot —
/// the ONE list the file dialogs, the folder scanner, the video-mode still
/// classifier and the Explorer registration all read (five copies used to
/// disagree: `.tif` converted via right-click but couldn't be added through
/// the dialog; `.jfif` got the menu and then "no image files").
pub const INPUT_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "jfif", "webp", "bmp", "tiff", "tif", "gif",
];

/// Whether `ext` (any case, no dot) is an accepted image input.
pub fn is_input_extension(ext: &str) -> bool {
    INPUT_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    Png,
    Jpeg,
    Webp,
    Bmp,
    Tiff,
    Gif,
}

impl OutputFormat {
    pub fn extension(self) -> &'static str {
        match self {
            OutputFormat::Png => "png",
            OutputFormat::Jpeg => "jpg",
            OutputFormat::Webp => "webp",
            OutputFormat::Bmp => "bmp",
            OutputFormat::Tiff => "tiff",
            OutputFormat::Gif => "gif",
        }
    }

    /// True if `quality` (0-100) is meaningful for this format ALONE. Caution:
    /// PNG returns false here, yet a Job with `png: PngOptimize::Lossy` (the
    /// default "Compress PNG" preset) DOES consume quality via libimagequant —
    /// gate UI controls on [`crate::pipeline::Job::uses_quality`], not this.
    pub fn uses_quality(self) -> bool {
        matches!(self, OutputFormat::Jpeg | OutputFormat::Webp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_maps_jpeg_to_jpg() {
        assert_eq!(OutputFormat::Jpeg.extension(), "jpg");
        assert_eq!(OutputFormat::Webp.extension(), "webp");
    }

    #[test]
    fn input_extensions_cover_every_output_and_the_jpeg_aliases() {
        for f in [
            OutputFormat::Png,
            OutputFormat::Jpeg,
            OutputFormat::Webp,
            OutputFormat::Bmp,
            OutputFormat::Tiff,
            OutputFormat::Gif,
        ] {
            assert!(is_input_extension(f.extension()), "{:?}", f);
        }
        assert!(is_input_extension("JFIF"));
        assert!(is_input_extension("tif"));
        assert!(!is_input_extension("exr"), "EXR is a sequence frame, not an image input");
        assert!(!is_input_extension("txt"));
    }

    #[test]
    fn quality_only_for_lossy() {
        assert!(OutputFormat::Jpeg.uses_quality());
        assert!(OutputFormat::Webp.uses_quality());
        assert!(!OutputFormat::Png.uses_quality());
    }
}
