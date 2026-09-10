use clap::Parser;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "kuvatin",
    version,
    about = "Batch image converter / resizer / cropper"
)]
pub struct Cli {
    /// Run a named preset headlessly over the given files.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["sequence_mp4", "register", "unregister"])]
    pub preset: Option<String>,

    /// Render the numbered image sequence each PATH belongs to (or every
    /// sequence in a folder PATH) to an MP4 next to it, headlessly.
    #[arg(long, conflicts_with_all = ["register", "unregister"])]
    pub sequence_mp4: bool,

    /// Frame rate for --sequence-mp4 (one file = one frame).
    #[arg(
        long,
        default_value_t = 30,
        value_name = "FPS",
        requires = "sequence_mp4"
    )]
    pub fps: u32,

    /// Register the Explorer context-menu entries and exit.
    #[arg(long, conflicts_with = "unregister")]
    pub register: bool,

    /// Remove the Explorer context-menu entries and exit.
    #[arg(long)]
    pub unregister: bool,

    /// Never show dialogs (the installer runs --register/--unregister with this).
    #[arg(long)]
    pub quiet: bool,

    /// Print the file extensions the Explorer menu attaches to, one per line,
    /// and exit (the sparse-package build reads this).
    #[arg(long, conflicts_with_all = ["preset", "sequence_mp4", "register", "unregister"])]
    pub print_extensions: bool,

    /// Image files or folders to operate on.
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub enum Mode {
    Register,
    Unregister,
    PrintExtensions,
    QuickRun {
        preset: String,
        paths: Vec<PathBuf>,
    },
    SequenceMp4 {
        paths: Vec<PathBuf>,
        fps: u32,
    },
    Gui {
        paths: Vec<PathBuf>,
    },
    /// A flag combination clap can't express: a headless mode with no PATH.
    Invalid(&'static str),
}

impl Cli {
    pub fn into_mode(self) -> Mode {
        if self.register {
            Mode::Register
        } else if self.unregister {
            Mode::Unregister
        } else if self.print_extensions {
            Mode::PrintExtensions
        } else if self.sequence_mp4 {
            if self.paths.is_empty() {
                Mode::Invalid("--sequence-mp4 needs at least one frame or folder PATH")
            } else {
                Mode::SequenceMp4 {
                    paths: self.paths,
                    fps: self.fps,
                }
            }
        } else if let Some(preset) = self.preset {
            if self.paths.is_empty() {
                Mode::Invalid("--preset needs at least one file or folder PATH")
            } else {
                Mode::QuickRun {
                    preset,
                    paths: self.paths,
                }
            }
        } else {
            Mode::Gui { paths: self.paths }
        }
    }
}

/// Explorer expands `%V` for a drive root to `C:\`, and under MSVC argv rules
/// the trailing backslash escapes the closing quote — the process receives
/// `C:"`. Repair exactly that shape back to `C:\`; everything else passes
/// through untouched.
pub fn repair_drive_root(arg: &OsStr) -> OsString {
    if let Some(s) = arg.to_str() {
        let b = s.as_bytes();
        if b.len() == 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'"' {
            return OsString::from(format!("{}:\\", &s[..1]));
        }
    }
    arg.to_os_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_of(args: &[&str]) -> Mode {
        Cli::parse_from(std::iter::once("kuvatin").chain(args.iter().copied())).into_mode()
    }

    fn parse_err(args: &[&str]) -> bool {
        Cli::try_parse_from(std::iter::once("kuvatin").chain(args.iter().copied())).is_err()
    }

    #[test]
    fn no_args_is_gui() {
        assert_eq!(mode_of(&[]), Mode::Gui { paths: vec![] });
    }

    #[test]
    fn files_only_is_gui_with_paths() {
        assert_eq!(
            mode_of(&["a.png", "b.jpg"]),
            Mode::Gui {
                paths: vec!["a.png".into(), "b.jpg".into()]
            }
        );
    }

    #[test]
    fn preset_is_quickrun() {
        assert_eq!(
            mode_of(&["--preset", "Convert to WebP", "a.png"]),
            Mode::QuickRun {
                preset: "Convert to WebP".into(),
                paths: vec!["a.png".into()]
            }
        );
    }

    #[test]
    fn register_flag() {
        assert_eq!(mode_of(&["--register"]), Mode::Register);
        assert_eq!(mode_of(&["--unregister", "--quiet"]), Mode::Unregister);
        assert_eq!(mode_of(&["--print-extensions"]), Mode::PrintExtensions);
        assert!(parse_err(&["--print-extensions", "--register"]));
    }

    #[test]
    fn sequence_mp4_defaults_to_30_fps_and_takes_fps() {
        assert_eq!(
            mode_of(&["--sequence-mp4", "frame_0001.png"]),
            Mode::SequenceMp4 {
                paths: vec!["frame_0001.png".into()],
                fps: 30
            }
        );
        assert_eq!(
            mode_of(&["--sequence-mp4", "--fps", "24", "C:/renders"]),
            Mode::SequenceMp4 {
                paths: vec!["C:/renders".into()],
                fps: 24
            }
        );
    }

    /// Modes are mutually exclusive and --fps only means something with
    /// --sequence-mp4; nonsense is a parse error, not a silent choice.
    #[test]
    fn conflicting_or_dangling_flags_are_rejected() {
        assert!(parse_err(&["--preset", "X", "--sequence-mp4", "a.png"]));
        assert!(parse_err(&["--register", "--unregister"]));
        assert!(parse_err(&["--preset", "X", "--register"]));
        assert!(parse_err(&["--fps", "24", "a.png"]));
    }

    /// A headless mode without any PATH is reported, not silently a no-op.
    #[test]
    fn headless_modes_need_a_path() {
        assert!(matches!(mode_of(&["--preset", "X"]), Mode::Invalid(_)));
        assert!(matches!(mode_of(&["--sequence-mp4"]), Mode::Invalid(_)));
    }

    #[test]
    fn repairs_the_drive_root_quote_artifact() {
        assert_eq!(
            repair_drive_root(OsStr::new("C:\"")),
            OsString::from("C:\\")
        );
        assert_eq!(
            repair_drive_root(OsStr::new("d:\"")),
            OsString::from("d:\\")
        );
        // Anything else is untouched — including a genuine three-char argument.
        assert_eq!(
            repair_drive_root(OsStr::new("C:\\")),
            OsString::from("C:\\")
        );
        assert_eq!(
            repair_drive_root(OsStr::new("ab\"")),
            OsString::from("ab\"")
        );
        assert_eq!(
            repair_drive_root(OsStr::new("C:\\dir\"")),
            OsString::from("C:\\dir\"")
        );
    }
}
