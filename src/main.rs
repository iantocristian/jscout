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
mod docs;
mod embed;
mod entity;
mod file_role;
mod formats;
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
mod progress;
mod publication;
mod query;
mod recon;
mod runtime_identity;
mod rust_lang;
mod scout;
mod scouting;
mod search;
mod search_scope;
mod semantic;
mod semantic_query;
mod setup;
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
    run(cli)
}

fn run(cli: Cli) -> Result<()> {
    let progress_label = (!cli.no_progress)
        .then(|| cli.command.progress_label())
        .flatten();
    let start_progress = |configured_interval| {
        progress_label.and_then(|(label, immediate)| {
            progress::Reporter::start(
                label,
                immediate,
                cli.progress_interval_seconds.unwrap_or(configured_interval),
            )
        })
    };
    let (progress, result) = match cli.command {
        Command::Config { command } => {
            // Config init must also work without an existing valid configuration.
            let progress = start_progress(progress::DEFAULT_INTERVAL_SECONDS);
            (progress, run_config_command(command, cli.config.as_deref()))
        }
        command => {
            let runtime = config::RuntimeConfig::load(command.root(), cli.config.as_deref())?;
            let progress = start_progress(runtime.effective.diagnostics.progress_interval_seconds);
            let legacy_keys = runtime.legacy_environment_keys();
            if !legacy_keys.is_empty() {
                eprintln!(
                    "warning: legacy environment configuration supplied {}; migrate these settings to {}",
                    legacy_keys.join(", "),
                    config::FILE_NAME,
                );
            }
            let result = run_command(
                command,
                &runtime,
                if cli
                    .experimental_tsr
                    .unwrap_or(runtime.effective.experimental.tsr)
                {
                    checker::Backend::Tsr {
                        diagnostics: cli.tsr_diagnostics,
                    }
                } else {
                    checker::Backend::TypeScript
                },
                cli.checker_jobs.unwrap_or(runtime.effective.checker.jobs),
            );
            (progress, result)
        }
    };
    if let Some(progress) = progress {
        progress.finish(result.is_ok());
    }
    result
}

#[cfg(test)]
use cli::{ConfigCommand, ScoutCommand};
#[cfg(test)]
use commands::{
    effective_search_response_byte_limit, or_configured, render_cli_neighborhood,
    render_semantic_memory_text, resolve_flag, resolve_search_provider,
};

#[cfg(test)]
mod main_tests;
