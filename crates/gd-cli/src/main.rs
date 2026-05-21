#![warn(clippy::pedantic)]
#![allow(
    clippy::module_name_repetitions,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate
)]

mod cli;
mod commands;
mod tui;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Command};
use gd_core::db::KeyStore;

fn main() -> Result<()> {
    let cli = Cli::parse();
    let data_dir = cli.data_dir.as_deref();

    if !cli.query.is_empty() {
        let query = cli.query.join(" ");
        let mut store = KeyStore::open(data_dir)?;
        return commands::jump::run(&mut store, &query);
    }

    match cli.command {
        Some(Command::Jump { ref query }) => {
            let query_str = query.join(" ");
            let mut store = KeyStore::open(data_dir)?;
            commands::jump::run(&mut store, &query_str)
        }
        Some(Command::Hook { ref path }) => {
            let mut store = KeyStore::open(data_dir)?;
            commands::hook::run(&mut store, path)
        }
        Some(Command::Link { ref alias, ref path }) => {
            let mut store = KeyStore::open(data_dir)?;
            commands::link::add(&mut store, alias, path)
        }
        Some(Command::Unlink { ref alias }) => {
            let mut store = KeyStore::open(data_dir)?;
            commands::link::remove(&mut store, alias)
        }
        Some(Command::List) => {
            let store = KeyStore::open(data_dir)?;
            commands::list::run(&store, cli.json)
        }
        Some(Command::Clean) => {
            let mut store = KeyStore::open(data_dir)?;
            commands::clean::run(&mut store)
        }
        Some(Command::Export) => {
            let store = KeyStore::open(data_dir)?;
            commands::export::run(&store)
        }
        Some(Command::Init { ref shell }) => {
            commands::init::run(shell.as_deref())
        }
        Some(Command::Doctor) => {
            let store = KeyStore::open(data_dir)?;
            commands::doctor::run(&store);
            Ok(())
        }
        Some(Command::Setup) => {
            commands::setup::run()
        }
        Some(Command::Update) => {
            commands::update::run()
        }
        Some(Command::Boost { ref path, weight }) => {
            let mut store = KeyStore::open(data_dir)?;
            commands::boost::add(&mut store, path.as_ref(), weight)
        }
        Some(Command::Unboost { ref path }) => {
            let mut store = KeyStore::open(data_dir)?;
            commands::boost::remove(&mut store, path)
        }
        None => {
            match dirs::home_dir() {
                Some(home) => {
                    println!("{}", home.display());
                    Ok(())
                }
                None => {
                    eprintln!("gd: cannot determine home directory");
                    std::process::exit(1);
                }
            }
        }
    }
}
