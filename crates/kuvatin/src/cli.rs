use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "kuvatin", about = "Batch image converter / resizer / cropper")]
pub struct Cli {
    /// Run a named preset headlessly over the given files.
    #[arg(long, value_name = "NAME")]
    pub preset: Option<String>,

    /// Register the Explorer context-menu entries and exit.
    #[arg(long)]
    pub register: bool,

    /// Remove the Explorer context-menu entries and exit.
    #[arg(long)]
    pub unregister: bool,

    /// Image files or folders to operate on.
    #[arg(value_name = "PATH")]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub enum Mode {
    Register,
    Unregister,
    QuickRun { preset: String, paths: Vec<PathBuf> },
    Gui { paths: Vec<PathBuf> },
}

impl Cli {
    pub fn into_mode(self) -> Mode {
        if self.register {
            Mode::Register
        } else if self.unregister {
            Mode::Unregister
        } else if let Some(preset) = self.preset {
            Mode::QuickRun { preset, paths: self.paths }
        } else {
            Mode::Gui { paths: self.paths }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode_of(args: &[&str]) -> Mode {
        Cli::parse_from(std::iter::once("kuvatin").chain(args.iter().copied())).into_mode()
    }

    #[test]
    fn no_args_is_gui() {
        assert_eq!(mode_of(&[]), Mode::Gui { paths: vec![] });
    }

    #[test]
    fn files_only_is_gui_with_paths() {
        assert_eq!(
            mode_of(&["a.png", "b.jpg"]),
            Mode::Gui { paths: vec!["a.png".into(), "b.jpg".into()] }
        );
    }

    #[test]
    fn preset_is_quickrun() {
        assert_eq!(
            mode_of(&["--preset", "Convert to WebP", "a.png"]),
            Mode::QuickRun { preset: "Convert to WebP".into(), paths: vec!["a.png".into()] }
        );
    }

    #[test]
    fn register_flag() {
        assert_eq!(mode_of(&["--register"]), Mode::Register);
    }
}
