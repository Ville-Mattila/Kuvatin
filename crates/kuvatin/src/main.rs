#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod cli;
mod collect;
mod gui;
mod quickrun;
mod shell;

use clap::Parser;
use cli::{Cli, Mode};

fn main() -> anyhow::Result<()> {
    let mode = Cli::parse().into_mode();
    match mode {
        Mode::Register => shell::register()?,
        Mode::Unregister => shell::unregister()?,
        Mode::QuickRun { preset, paths } => {
            let failures = quickrun::run(&preset, &paths)?;
            if failures > 0 {
                std::process::exit(1);
            }
        }
        Mode::Gui { paths } => gui::run(paths)?,
    }
    Ok(())
}
