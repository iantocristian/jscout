//! CLI/environment resolution for the gateway: model, node runtime, gateway
//! entry point, and request policy. The default stays on the explicit
//! ChatGPT-plan-backed provider; CLI/environment settings may override it.

use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};

pub const MODEL_ENV: &str = "JSCOUT_LLM_MODEL";
pub const REASONING_ENV: &str = "JSCOUT_LLM_REASONING";
pub const GATEWAY_ENV: &str = "JSCOUT_PI_AI_GATEWAY";
pub const NODE_ENV: &str = "JSCOUT_NODE";
pub const DEFAULT_MODEL: &str = "openai-codex:gpt-5.6-terra";
pub const MODEL_EXAMPLE: &str = DEFAULT_MODEL;

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

/// `--model`, then `JSCOUT_LLM_MODEL`, then the plan-backed default.
pub fn resolve_model(cli: Option<&str>) -> Result<ModelSpec> {
    let configured = env::var(MODEL_ENV).ok();
    resolve_model_values(cli, configured.as_deref())
}

fn resolve_model_values(cli: Option<&str>, configured: Option<&str>) -> Result<ModelSpec> {
    if let Some(value) = cli {
        return ModelSpec::parse(value);
    }
    if let Some(value) = configured
        && !value.trim().is_empty()
    {
        return ModelSpec::parse(value);
    }
    ModelSpec::parse(DEFAULT_MODEL)
}

/// `--reasoning`, then `JSCOUT_LLM_REASONING`; None means provider default.
pub fn resolve_reasoning(cli: Option<&str>) -> Option<String> {
    if let Some(value) = cli {
        return Some(value.to_string());
    }
    match env::var(REASONING_ENV) {
        Ok(value) if !value.trim().is_empty() => Some(value.trim().to_string()),
        _ => None,
    }
}

/// Gateway entry file: `--gateway-path`, then `JSCOUT_PI_AI_GATEWAY`, then the
/// companion checkout/installation next to the running binary. Both settings
/// name a file path, never a shell command string.
pub fn resolve_gateway(cli: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = cli {
        return existing_file(path, "--gateway-path");
    }
    if let Ok(value) = env::var(GATEWAY_ENV)
        && !value.trim().is_empty()
    {
        return existing_file(Path::new(value.trim()), GATEWAY_ENV);
    }
    for candidate in companion_candidates() {
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "pi-ai gateway not found: pass --gateway-path, set {GATEWAY_ENV}, or install the \
         companion gateway (gateway/src/main.mjs) beside the jscout binary; \
         run `jscout llm doctor` after installing"
    );
}

fn companion_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = env::current_exe() {
        let mut ancestors = exe.parent();
        // Installed layout: gateway/ beside the binary. Development layout:
        // target/{debug,release}/jscout with gateway/ at the repository root.
        for _ in 0..3 {
            if let Some(dir) = ancestors {
                candidates.push(dir.join("gateway/src/main.mjs"));
                ancestors = dir.parent();
            }
        }
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

/// Node runtime: `JSCOUT_NODE`, then `node` on PATH.
pub fn resolve_node() -> Result<PathBuf> {
    if let Ok(value) = env::var(NODE_ENV)
        && !value.trim().is_empty()
    {
        let path = PathBuf::from(value.trim());
        if path.is_file() {
            return Ok(path);
        }
        bail!(
            "{NODE_ENV} does not name an existing node executable: {}",
            path.display()
        );
    }
    let path_var = env::var_os("PATH").context("PATH is not set; cannot locate node")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join("node");
        if candidate.is_file() {
            return Ok(candidate);
        }
    }
    bail!(
        "node not found on PATH: install Node >= 22.19.0 or set {NODE_ENV}; \
         the pi-ai gateway is a Node sidecar and cannot run without it"
    )
}

/// Per-command request policy shared by scouting commands.
#[derive(Debug, Clone)]
pub struct RequestPolicy {
    pub timeout: Duration,
    pub max_calls: usize,
    pub context_bytes: usize,
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
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_resolution_prefers_cli_then_env_then_plan_default() {
        let spec = resolve_model_values(Some("openai-codex:gpt-5.6-sol"), Some("openai:gpt-5.4"))
            .expect("cli model");
        assert_eq!(
            (spec.provider.as_str(), spec.model_id.as_str()),
            ("openai-codex", "gpt-5.6-sol")
        );
        assert!(ModelSpec::parse("missing-separator").is_err());
        assert!(ModelSpec::parse(":model").is_err());
        assert!(ModelSpec::parse("provider:").is_err());

        assert_eq!(
            resolve_model_values(None, Some("openai:gpt-5.4"))
                .expect("environment model")
                .spec,
            "openai:gpt-5.4"
        );
        assert_eq!(
            resolve_model_values(None, None)
                .expect("default model")
                .spec,
            DEFAULT_MODEL
        );
    }

    #[test]
    fn request_policy_rejects_zero_budgets() {
        assert!(RequestPolicy::new(0, 1, 1).is_err());
        assert!(RequestPolicy::new(1, 0, 1).is_err());
        assert!(RequestPolicy::new(1, 1, 0).is_err());
        let policy = RequestPolicy::new(300, 8, 200_000).expect("valid policy");
        assert_eq!(policy.timeout, Duration::from_secs(300));
    }
}
