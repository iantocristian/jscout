#![recursion_limit = "256"]
// Test fixtures favor reorderable inputs over last-use clone removal.
#![cfg_attr(not(test), warn(clippy::redundant_clone))]

mod agent;
mod calls;
mod checker;
mod chunk;
mod cli;
mod commands;
mod compact;
mod config;
mod dependency;
mod embed;
mod entity;
mod file_role;
mod fs_ops;
mod graph;
mod heur;
mod indexer;
mod inference;
mod io_policy;
mod llm;
mod mcp;
mod origin;
mod package_exports;
mod parse;
mod query;
mod recon;
mod scout;
mod scouting;
mod search;
mod semantic;
mod semantic_query;
mod stats;
mod store;
mod structural;
mod surface;
#[cfg(test)]
mod test_fs;
mod value_flow;
mod walk;
mod watch;
mod workspace;

use anyhow::Result;
use clap::Parser;

use cli::{Cli, Command};
use commands::{run_command, run_config_command};

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Config { command } => run_config_command(command, cli.config.as_deref()),
        command => {
            let runtime = config::RuntimeConfig::load(command.root(), cli.config.as_deref())?;
            let legacy_keys = runtime.legacy_environment_keys();
            if !legacy_keys.is_empty() {
                eprintln!(
                    "warning: legacy environment configuration supplied {}; migrate these settings to {}",
                    legacy_keys.join(", "),
                    config::FILE_NAME,
                );
            }
            run_command(command, &runtime)
        }
    }
}

#[cfg(test)]
use cli::{ConfigCommand, ScoutCommand};
#[cfg(test)]
use commands::{
    effective_search_response_byte_limit, or_configured, render_cli_neighborhood,
    render_semantic_memory_text, resolve_flag,
};

#[cfg(test)]
mod main_tests;
