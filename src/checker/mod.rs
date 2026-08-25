mod enrich;
mod package_gate;
pub mod process;
pub mod protocol;

pub(crate) use enrich::target_fingerprint;
pub use enrich::{EnrichOptions, EnrichReport, enrich, is_terminal_partial_failure};

/// Stable identity for the checker semantics and fixed watcher policy. It is
/// intentionally independent of repository state, project membership, paths,
/// timeouts, and the per-generation carry-free override.
pub fn watch_policy_fingerprint() -> String {
    fn component(hasher: &mut blake3::Hasher, value: &[u8]) {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }

    let mut hasher = blake3::Hasher::new();
    for value in [
        b"jscout-checker-watch-policy-v1".as_slice(),
        enrich::CHECKER_SEMANTICS_FINGERPRINT,
        enrich::PROJECT_PLAN_FINGERPRINT_DOMAIN,
        enrich::ENRICH_PLAN_FINGERPRINT_DOMAIN,
        package_gate::POLICY_FINGERPRINT_DOMAIN.as_bytes(),
    ] {
        component(&mut hasher, value);
    }
    component(
        &mut hasher,
        protocol::PROTOCOL_VERSION.to_string().as_bytes(),
    );
    component(
        &mut hasher,
        enrich::INFERRED_ROOT_CAP.to_string().as_bytes(),
    );
    component(
        &mut hasher,
        b"selection=files:;packages:;members:;roles:;max_occurrences:none;include_all:false;carry=project-validated;drift-flush=daily",
    );
    hasher.finalize().to_hex().to_string()
}

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

pub fn resolve_sidecar(cli: Option<&Path>, configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = cli {
        return existing(path, "--sidecar-path");
    }
    if let Some(path) = configured {
        return existing(path, "sidecars.checker");
    }
    let executable = std::env::current_exe()?;
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
        "TypeScript checker sidecar not found: configure sidecars.checker, pass --sidecar-path, or install checker/src/main.mjs beside the jscout binary"
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

pub fn launch(
    root: &Path,
    sidecar: Option<&Path>,
    configured_sidecar: Option<&Path>,
    node: &str,
) -> Result<process::ProcessChecker> {
    let node = crate::llm::config::resolve_node_setting(node, "the TypeScript checker sidecar")?;
    crate::llm::config::verify_node_version(&node)?;
    let sidecar = resolve_sidecar(sidecar, configured_sidecar)?;
    Ok(process::ProcessChecker::spawn(&node, &sidecar, root)?)
}

pub fn doctor(
    root: &Path,
    sidecar: Option<&Path>,
    configured_sidecar: Option<&Path>,
    node: &str,
    timeout: std::time::Duration,
) -> Result<()> {
    let node = crate::llm::config::resolve_node_setting(node, "the TypeScript checker sidecar")?;
    let node_version = crate::llm::config::verify_node_version(&node)?;
    let sidecar = resolve_sidecar(sidecar, configured_sidecar)?;
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
        let reasons = if project.purpose_reasons.is_empty() {
            String::new()
        } else {
            format!("; evidence: {}", project.purpose_reasons.join(", "))
        };
        println!(
            "  {} ({} files; purpose: {}{})",
            project.project_id, project.file_count, project.purpose, reasons
        );
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

#[cfg(test)]
mod policy_tests {
    #[test]
    fn watch_policy_identity_is_stable_lowercase_hex() {
        let fingerprint = super::watch_policy_fingerprint();
        assert_eq!(fingerprint.len(), 64);
        assert!(
            fingerprint
                .bytes()
                .all(|byte| { byte.is_ascii_digit() || matches!(byte, b'a'..=b'f') })
        );
        assert_eq!(fingerprint, super::watch_policy_fingerprint());
    }
}
