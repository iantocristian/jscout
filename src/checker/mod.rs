mod enrich;
mod package_gate;
pub mod process;
pub mod protocol;

pub(crate) use enrich::target_fingerprint;
pub use enrich::{EnrichOptions, EnrichReport, enrich, is_terminal_partial_failure};

/// Stable identity for the checker semantics and the actual watcher selection
/// policy. It is intentionally independent of repository state, project
/// membership, paths, timeouts, dirty affinity, and the per-generation
/// carry-free override.
pub fn watch_policy_fingerprint(options: &EnrichOptions<'_>) -> String {
    fn component(hasher: &mut blake3::Hasher, value: &[u8]) {
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value);
    }
    fn selector(hasher: &mut blake3::Hasher, name: &str, values: &[String]) {
        component(hasher, name.as_bytes());
        let mut normalized = values.to_vec();
        normalized.sort();
        normalized.dedup();
        component(hasher, &(normalized.len() as u64).to_le_bytes());
        for value in normalized {
            component(hasher, value.as_bytes());
        }
    }

    let mut hasher = blake3::Hasher::new();
    for value in [
        b"jscout-checker-watch-policy-v2".as_slice(),
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
    selector(&mut hasher, "files", &options.files);
    selector(&mut hasher, "packages", &options.packages);
    selector(&mut hasher, "members", &options.members);
    selector(&mut hasher, "roles", &options.roles);
    component(
        &mut hasher,
        options
            .max_occurrences
            .map_or_else(|| "none".to_string(), |limit| limit.to_string())
            .as_bytes(),
    );
    component(&mut hasher, options.include_all.to_string().as_bytes());
    component(&mut hasher, options.dry_run.to_string().as_bytes());
    component(&mut hasher, b"carry=project-validated;drift-flush=daily");
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
    use std::time::Duration;

    use super::EnrichOptions;

    fn options() -> EnrichOptions<'static> {
        EnrichOptions {
            database: None,
            sidecar: None,
            node: "node",
            timeout: Duration::from_secs(300),
            files: Vec::new(),
            packages: Vec::new(),
            members: Vec::new(),
            roles: Vec::new(),
            max_occurrences: None,
            include_all: false,
            dry_run: false,
            carry_forward: true,
            force_full: false,
            dirty_files: Vec::new(),
        }
    }

    #[test]
    fn watch_policy_identity_is_stable_lowercase_hex() {
        let fingerprint = super::watch_policy_fingerprint(&options());
        assert_eq!(fingerprint.len(), 64);
        assert!(
            fingerprint
                .bytes()
                .all(|byte| { byte.is_ascii_digit() || matches!(byte, b'a'..=b'f') })
        );
        assert_eq!(fingerprint, super::watch_policy_fingerprint(&options()));
    }

    #[test]
    fn watch_policy_identity_tracks_normalized_selection_not_generation_state() {
        let baseline = super::watch_policy_fingerprint(&options());
        for changed in [
            {
                let mut options = options();
                options.files.push("src/a.ts".into());
                options
            },
            {
                let mut options = options();
                options.packages.push("workspace-a".into());
                options
            },
            {
                let mut options = options();
                options.members.push("insert".into());
                options
            },
            {
                let mut options = options();
                options.roles.push("production".into());
                options
            },
            {
                let mut options = options();
                options.max_occurrences = Some(512);
                options
            },
            {
                let mut options = options();
                options.include_all = true;
                options
            },
            {
                let mut options = options();
                options.dry_run = true;
                options
            },
        ] {
            assert_ne!(baseline, super::watch_policy_fingerprint(&changed));
        }

        let mut unordered = options();
        unordered.files = vec!["b.ts".into(), "a.ts".into(), "a.ts".into()];
        let mut normalized = options();
        normalized.files = vec!["a.ts".into(), "b.ts".into()];
        assert_eq!(
            super::watch_policy_fingerprint(&unordered),
            super::watch_policy_fingerprint(&normalized)
        );

        let mut generation = options();
        generation.force_full = true;
        generation.carry_forward = false;
        generation.dirty_files.push("src/changed.ts".into());
        generation.timeout = Duration::from_secs(1);
        assert_eq!(baseline, super::watch_policy_fingerprint(&generation));
    }
}
