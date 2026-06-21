mod cli;
mod collect;

use clap::Parser;
use cli::{Cli, Mode};

fn main() -> anyhow::Result<()> {
    let mode = Cli::parse().into_mode();
    match mode {
        Mode::Register => println!("register: not yet implemented"),
        Mode::Unregister => println!("unregister: not yet implemented"),
        Mode::QuickRun { preset, paths } => {
            println!("quickrun preset={preset} files={}", paths.len());
        }
        Mode::Gui { paths } => {
            println!("gui files={}", paths.len());
        }
    }
    Ok(())
}
