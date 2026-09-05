//! Explicit repository onboarding. No install hooks, global configuration, or daemons.
mod client;
mod probe;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

use crate::{agent, cli::Command, commands, config, mcp};
pub use client::Client;

#[derive(Debug, Serialize)]
struct Launch {
    command: PathBuf,
    args: Vec<String>,
}

impl Launch {
    fn watch_command(&self) -> Result<String> {
        let mut args = self.args.clone();
        let command = args
            .len()
            .checked_sub(2)
            .context("MCP launch arguments missing")?;
        args[command] = "watch".to_string();
        Ok(std::iter::once(path_text(&self.command)?)
            .chain(args)
            .map(|value| format!("'{}'", value.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join(" "))
    }
    fn current(root: &Path, runtime: &config::RuntimeConfig) -> Result<Self> {
        // npm's wrapper identifies the installed launcher and its exact Node runtime.
        // Registering the platform binary alone would lose bundled sidecar discovery.
        let mut launch = match (
            std::env::var_os("JSCOUT_BUNDLED_NODE"),
            std::env::var_os("JSCOUT_BUNDLED_LAUNCHER"),
        ) {
            (Some(node), Some(wrapper)) => {
                let node = PathBuf::from(node)
                    .canonicalize()
                    .context("resolve npm Node runtime")?;
                let wrapper = PathBuf::from(wrapper)
                    .canonicalize()
                    .context("resolve npm launcher")?;
                Self {
                    command: node,
                    args: vec![path_text(&wrapper)?],
                }
            }
            (None, None) => Self {
                command: std::env::current_exe()?,
                args: Vec::new(),
            },
            _ => bail!(
                "incomplete npm launcher discovery; rerun setup through the installed jscout command"
            ),
        };
        if runtime.config_explicit {
            launch.args.extend([
                "--config".to_string(),
                path_text(
                    runtime
                        .config_path
                        .as_deref()
                        .context("explicit configuration path missing")?,
                )?,
            ]);
        }
        launch.args.extend(["mcp".to_string(), path_text(root)?]);
        Ok(launch)
    }
}

fn path_text(path: &Path) -> Result<String> {
    path.to_str()
        .map(str::to_string)
        .context("MCP configuration requires UTF-8 paths")
}

pub fn run(
    root: &Path,
    client: Client,
    print_only: bool,
    runtime: &config::RuntimeConfig,
) -> Result<()> {
    let root = root.canonicalize()?;
    ensure!(root.is_dir(), "setup root must be a directory");
    let launch = Launch::current(&root, runtime)?;
    let entry = client.entry(&launch, runtime)?;
    if print_only {
        print!("{}", client.render(&entry)?);
        return Ok(());
    }

    let legacy = runtime.legacy_environment_keys();
    ensure!(
        legacy.is_empty(),
        "before setup, migrate legacy environment settings into .jscout.toml: {}; the agent client may not inherit your shell's policy",
        legacy.join(", ")
    );

    // Detect conflicts before creating a configuration, skill, or database.
    let (registration, registered_entry) = client.prepare(&root, &entry)?;
    let destination = client.skill_destination();
    let skill = root.join(destination.relative_path());
    client::check_local_target(&root, &skill)?;
    let profile = mcp::ToolProfile::parse(&runtime.effective.mcp.profile)?;
    mcp::ensure_effective_surface(
        profile,
        runtime.effective.docs.enabled,
        &runtime.effective.mcp.tools,
    )?;
    let tier = match profile {
        mcp::ToolProfile::Baseline => agent::Tier::Core,
        mcp::ToolProfile::Structural => agent::Tier::Full,
    };
    let existing_skill = client::read_optional(&skill)?;
    if existing_skill
        .as_deref()
        .is_some_and(|text| text != tier.guide())
    {
        eprintln!(
            "keeping existing agent skill unchanged: {}; use agent-guide --update to replace it deliberately",
            skill.display()
        );
    }
    if !runtime.config_loaded {
        let path = root.join(config::FILE_NAME);
        client::check_local_target(&root, &path)?;
        client::FileUpdate::new(path.clone(), None, "version = 1\n".to_string()).write(&root)?;
        println!("created minimal configuration: {}", path.display());
    }
    if existing_skill.is_none() {
        client::FileUpdate::new(skill.clone(), None, tier.guide().to_string()).write(&root)?;
        println!("installed agent skill: {}", skill.display());
    }
    // Reload after creating the minimal file so MCP reports the same configuration.
    let runtime = config::RuntimeConfig::load(
        Some(&root),
        runtime
            .config_explicit
            .then_some(runtime.config_path.as_deref())
            .flatten(),
    )?;
    commands::run_command(
        Command::Index {
            root: root.clone(),
            database: None,
            dependencies: Vec::new(),
            no_dependencies: false,
        },
        &runtime,
    )?;

    // Start the exact command that will be registered, before publishing client config.
    // This checks local MCP readiness only; it never calls an embedding or LLM provider.
    let tools = probe::verify(&launch, client, &registered_entry, &root, &runtime)?;
    registration.write(&root)?;
    println!(
        "MCP configuration ready: {}",
        root.join(client.config_path()).display()
    );
    println!(
        "verified MCP initialization and {} tools: {}",
        tools.len(),
        tools.join(", ")
    );
    println!(
        "ready: index and MCP server verified; restart your client and approve/trust this repository if prompted"
    );
    println!(
        "keep the index fresh in another terminal: {}",
        launch.watch_command()?
    );
    if runtime.effective.index.dependencies != runtime.effective.watch.dependencies {
        println!(
            "index.dependencies and watch.dependencies differ; align them or pass matching --deps to retain the same dependency corpus"
        );
    }
    println!("setup did not start a watcher, generate embeddings, or run LLM scouting");
    if runtime.effective.embedding.provider.is_some() {
        println!(
            "optional vector readiness was not tested; run jscout embed and jscout docs embed when wanted"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests;
