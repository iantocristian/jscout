mod enrich;
pub mod process;
pub mod protocol;

pub(crate) use enrich::target_fingerprint;
pub use enrich::{EnrichOptions, enrich};

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub const SIDECAR_ENV: &str = "JSCOUT_CHECKER_SIDECAR";

pub fn resolve_sidecar(cli: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = cli {
        return existing(path, "--sidecar-path");
    }
    if let Ok(value) = env::var(SIDECAR_ENV)
        && !value.trim().is_empty()
    {
        return existing(Path::new(value.trim()), SIDECAR_ENV);
    }
    let executable = env::current_exe()?;
    let Some(binary_dir) = executable.parent() else {
        bail!("could not locate the checker sidecar relative to the jscout binary");
    };
    let mut candidates = vec![binary_dir.join("checker/src/main.mjs")];
    if matches!(
        binary_dir.file_name().and_then(|name| name.to_str()),
        Some("debug" | "release")
    ) && binary_dir
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        == Some("target")
        && let Some(repository) = binary_dir.parent().and_then(Path::parent)
    {
        candidates.push(repository.join("checker/src/main.mjs"));
    }
    if let Some(path) = candidates.into_iter().find(|path| path.is_file()) {
        return Ok(path);
    }
    bail!(
        "TypeScript checker sidecar not found: pass --sidecar-path, set {SIDECAR_ENV}, or install checker/src/main.mjs beside the jscout binary"
    )
}

fn existing(path: &Path, source: &str) -> Result<PathBuf> {
    if path.is_file() {
        Ok(path.to_path_buf())
    } else {
        bail!(
            "{source} does not name an existing checker sidecar file: {}",
            path.display()
        )
    }
}

pub fn launch(root: &Path, sidecar: Option<&Path>) -> Result<process::ProcessChecker> {
    let node = crate::llm::config::resolve_node_for("the TypeScript checker sidecar")?;
    crate::llm::config::verify_node_version(&node)?;
    let sidecar = resolve_sidecar(sidecar)?;
    Ok(process::ProcessChecker::spawn(&node, &sidecar, root)?)
}

pub fn doctor(root: &Path, sidecar: Option<&Path>, timeout: std::time::Duration) -> Result<()> {
    let node = crate::llm::config::resolve_node_for("the TypeScript checker sidecar")?;
    let node_version = crate::llm::config::verify_node_version(&node)?;
    let sidecar = resolve_sidecar(sidecar)?;
    println!("node: {} ({node_version})", node.display());
    println!("checker sidecar: {}", sidecar.display());
    let mut checker = process::ProcessChecker::spawn(&node, &sidecar, root)?;
    let capabilities = checker.capabilities(timeout)?;
    println!(
        "checker sidecar version: {} (protocol {}), node {}",
        checker.versions.sidecar, checker.versions.protocol, checker.versions.node
    );
    println!(
        "TypeScript: {} ({})",
        capabilities.typescript.version, capabilities.typescript.source
    );
    println!("configured projects: {}", capabilities.projects.len());
    for project in capabilities.projects {
        println!("  {} ({} files)", project.project_id, project.file_count);
    }
    println!(
        "configuration problems: {}",
        capabilities.configuration_problems.len()
    );
    for problem in capabilities.configuration_problems {
        println!(
            "  {} [{}]: {}",
            problem.project_id, problem.code, problem.message
        );
    }
    println!("ready: {}", capabilities.question);
    Ok(())
}
