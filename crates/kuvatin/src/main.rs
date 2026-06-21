mod cli;
mod collect;
mod shell;

use clap::Parser;
use cli::{Cli, Mode};

fn main() -> anyhow::Result<()> {
    let mode = Cli::parse().into_mode();
    match mode {
        Mode::Register => shell::register()?,
        Mode::Unregister => shell::unregister()?,
        Mode::QuickRun { preset, paths } => {
            println!("quickrun preset={preset} files={}", paths.len());
        }
        Mode::Gui { paths } => {
            println!("gui files={}", paths.len());
        }
    }
    Ok(())
}
