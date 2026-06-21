use serde::{Deserialize, Serialize};

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

    /// True if `quality` (0-100) is meaningful for this format.
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
    fn quality_only_for_lossy() {
        assert!(OutputFormat::Jpeg.uses_quality());
        assert!(OutputFormat::Webp.uses_quality());
        assert!(!OutputFormat::Png.uses_quality());
    }
}
