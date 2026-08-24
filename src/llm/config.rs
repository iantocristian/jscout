//! CLI/environment resolution for the gateway: model, node runtime, gateway
//! entry point, and request policy. The default stays on the explicit
//! ChatGPT-plan-backed provider; CLI/environment settings may override it.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub const DEFAULT_MODEL: &str = "openai-codex:gpt-5.6-terra";
pub const MODEL_EXAMPLE: &str = DEFAULT_MODEL;
pub const MINIMUM_NODE_VERSION: (u64, u64, u64) = (22, 19, 0);
pub const MINIMUM_NODE_VERSION_TEXT: &str = "22.19.0";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSpec {
    pub spec: String,
    pub provider: String,
    pub model_id: String,
}

impl ModelSpec {
    pub fn parse(value: &str) -> Result<Self> {
        let spec = value.trim();
        let Some(separator) = spec.find(':') else {
            bail!("model must use provider:model form, e.g. {MODEL_EXAMPLE}; received `{spec}`");
        };
        if separator == 0 || separator == spec.len() - 1 {
            bail!("model must use provider:model form, e.g. {MODEL_EXAMPLE}; received `{spec}`");
        }
        Ok(Self {
            spec: spec.to_string(),
            provider: spec[..separator].to_string(),
            model_id: spec[separator + 1..].to_string(),
        })
    }
}

/// Resolve a CLI override over the already resolved repository setting.
/// Environment compatibility belongs to the top-level configuration loader,
/// so operational callers do not reread process state here.
pub fn resolve_model_setting(cli: Option<&str>, configured: &str) -> Result<ModelSpec> {
    ModelSpec::parse(cli.unwrap_or(configured))
}

pub fn resolve_reasoning_setting(cli: Option<&str>, configured: Option<&str>) -> Option<String> {
    cli.or(configured).map(str::to_string)
}

pub fn resolve_gateway_setting(cli: Option<&Path>, configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = cli {
        return existing_file(path, "--gateway-path");
    }
    if let Some(path) = configured {
        return existing_file(path, "sidecars.gateway");
    }
    for candidate in companion_candidates() {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "pi-ai gateway not found: configure sidecars.gateway, pass --gateway-path, or install the companion gateway beside the jscout binary"
    );
}

fn companion_candidates() -> Vec<PathBuf> {
    let Ok(exe) = env::current_exe() else {
        return Vec::new();
    };
    companion_candidates_for(&exe)
}

fn companion_candidates_for(exe: &Path) -> Vec<PathBuf> {
    let Some(binary_dir) = exe.parent() else {
        return Vec::new();
    };
    let mut candidates = vec![binary_dir.join("gateway/src/main.mjs")];
    // Development-only fallback. Installed binaries must not discover an
    // unrelated gateway in an arbitrary parent directory.
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
        candidates.push(repository.join("gateway/src/main.mjs"));
    }
    candidates
}

fn existing_file(path: &Path, source: &str) -> Result<PathBuf> {
    if path.is_file() {
        Ok(path.to_path_buf())
    } else {
        bail!(
            "{source} does not name an existing gateway file: {}",
            path.display()
        )
    }
}

pub fn resolve_node_setting(value: &str, sidecar: &str) -> Result<PathBuf> {
    let configured = PathBuf::from(value);
    if configured.components().count() > 1 || configured.is_absolute() {
        if configured.is_file() {
            return Ok(configured);
        }
        bail!(
            "sidecars.node does not name an existing node executable: {}",
            configured.display()
        );
    }
    let path_var = env::var_os("PATH").context("PATH is not set; cannot locate node")?;
    for directory in env::split_paths(&path_var) {
        let candidate = directory.join(value);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "node executable `{value}` not found on PATH: install Node >= {MINIMUM_NODE_VERSION_TEXT} or configure sidecars.node; {sidecar} cannot run without it"
    )
}

/// Run the selected Node executable and enforce the gateway's runtime floor.
/// The returned text is safe to print: it is parsed from Node's version-only
/// stdout, and stderr is deliberately excluded from diagnostics.
pub fn verify_node_version(node: &Path) -> Result<String> {
    let output = Command::new(node)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to run {} --version", node.display()))?;
    if !output.status.success() {
        bail!(
            "{} --version failed; install Node >= {MINIMUM_NODE_VERSION_TEXT} or configure sidecars.node",
            node.display()
        );
    }
    let reported = std::str::from_utf8(&output.stdout)
        .context("node --version returned non-UTF-8 output")?
        .trim();
    let parsed = parse_node_version(reported)?;
    if parsed < MINIMUM_NODE_VERSION {
        bail!(
            "Node {reported} is unsupported; install Node >= {MINIMUM_NODE_VERSION_TEXT} or configure sidecars.node"
        );
    }
    Ok(reported.to_string())
}

fn parse_node_version(reported: &str) -> Result<(u64, u64, u64)> {
    let version = reported.strip_prefix('v').unwrap_or(reported);
    let mut parts = version.split('.');
    let major = parse_node_version_part(parts.next(), reported)?;
    let minor = parse_node_version_part(parts.next(), reported)?;
    let patch = parse_node_version_part(parts.next(), reported)?;
    if parts.next().is_some() {
        bail!("could not parse node --version output `{reported}`");
    }
    Ok((major, minor, patch))
}

fn parse_node_version_part(part: Option<&str>, reported: &str) -> Result<u64> {
    part.and_then(|value| value.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("could not parse node --version output `{reported}`"))
}

/// Per-command request policy shared by scouting commands.
#[derive(Debug, Clone)]
pub struct RequestPolicy {
    pub timeout: Duration,
    pub max_calls: usize,
    pub context_bytes: usize,
    pub max_concurrency: usize,
}

impl RequestPolicy {
    pub fn new(timeout_secs: u64, max_calls: usize, context_bytes: usize) -> Result<Self> {
        if timeout_secs == 0 {
            bail!("--timeout must be greater than zero seconds");
        }
        if max_calls == 0 {
            bail!("--max-calls must be greater than zero");
        }
        if context_bytes == 0 {
            bail!("--context-bytes must be greater than zero");
        }
        Ok(Self {
            timeout: Duration::from_secs(timeout_secs),
            max_calls,
            context_bytes,
            max_concurrency: 1,
        })
    }

    pub fn with_max_concurrency(mut self, max_concurrency: usize) -> Result<Self> {
        if max_concurrency == 0 {
            bail!("llm.max_concurrency must be greater than zero");
        }
        self.max_concurrency = max_concurrency;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_resolution_prefers_cli_over_resolved_repository_setting() {
        let spec = resolve_model_setting(Some("openai-codex:gpt-5.6-sol"), "openai:gpt-5.4")
            .expect("cli model");
        assert_eq!(
            (spec.provider.as_str(), spec.model_id.as_str()),
            ("openai-codex", "gpt-5.6-sol")
        );
        assert!(ModelSpec::parse("missing-separator").is_err());
        assert!(ModelSpec::parse(":model").is_err());
        assert!(ModelSpec::parse("provider:").is_err());

        assert_eq!(
            resolve_model_setting(None, "openai:gpt-5.4")
                .expect("repository model")
                .spec,
            "openai:gpt-5.4"
        );
        assert_eq!(
            resolve_model_setting(None, DEFAULT_MODEL)
                .expect("default model")
                .spec,
            DEFAULT_MODEL
        );
    }

    #[test]
    fn request_policy_rejects_zero_budgets() -> anyhow::Result<()> {
        assert!(RequestPolicy::new(0, 1, 1).is_err());
        assert!(RequestPolicy::new(1, 0, 1).is_err());
        assert!(RequestPolicy::new(1, 1, 0).is_err());
        let policy = RequestPolicy::new(300, 8, 200_000).expect("valid policy");
        assert_eq!(policy.timeout, Duration::from_secs(300));
        assert_eq!(policy.max_concurrency, 1);
        assert!(policy.clone().with_max_concurrency(0).is_err());
        assert_eq!(policy.with_max_concurrency(8)?.max_concurrency, 8);
        Ok(())
    }

    #[test]
    fn node_version_parser_enforces_complete_numeric_semver() {
        assert_eq!(parse_node_version("v22.19.0").unwrap(), (22, 19, 0));
        assert_eq!(parse_node_version("24.1.3").unwrap(), (24, 1, 3));
        for invalid in ["", "v22", "v22.19", "v22.19.x", "v22.19.0.1"] {
            assert!(parse_node_version(invalid).is_err(), "accepted {invalid}");
        }
        assert!((22, 18, 9) < MINIMUM_NODE_VERSION);
        assert!((22, 19, 0) >= MINIMUM_NODE_VERSION);
    }

    #[test]
    fn gateway_discovery_uses_only_installed_or_explicit_dev_layouts() {
        assert_eq!(
            companion_candidates_for(Path::new("/opt/jscout/jscout")),
            vec![PathBuf::from("/opt/jscout/gateway/src/main.mjs")]
        );
        assert_eq!(
            companion_candidates_for(Path::new("/repo/target/release/jscout")),
            vec![
                PathBuf::from("/repo/target/release/gateway/src/main.mjs"),
                PathBuf::from("/repo/gateway/src/main.mjs"),
            ]
        );
    }
}
