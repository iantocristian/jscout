//! Gateway boundary for generative model calls.
//!
//! Rust owns prompts, schemas, validation, persistence, and lifecycle; the
//! Node sidecar owns providers, credentials, and request execution. No
//! provider SDK is called from Rust, and no model call happens during
//! indexing or watch.

pub mod config;
pub mod process;
pub mod protocol;

use std::fmt;
use std::time::Duration;

use protocol::{CompleteRequest, ModelCapabilities, ProviderSummary, RemoteError, ToolCall, Usage};

/// Provider/model/billing identity the gateway resolved before any network
/// wait. `auth_source` is a category label, never a credential value.
#[derive(Debug, Clone)]
pub struct StartedInfo {
    pub provider: String,
    pub model: String,
    pub api: String,
    pub base_url: Option<String>,
    pub billing_path: String,
    pub auth_source: String,
}

#[derive(Debug, Clone)]
pub struct CompletionOutcome {
    pub started: StartedInfo,
    pub tool_call: ToolCall,
    pub stop_reason: String,
    pub usage: Usage,
    pub attempts: u64,
    pub response_model: Option<String>,
}

/// One independently timed model request in a bounded scouting batch.
#[derive(Clone, Copy)]
pub struct CompletionTask<'a> {
    pub request: &'a CompleteRequest,
    pub timeout: Duration,
}

#[derive(Debug)]
pub enum GatewayError {
    /// Launch failure: missing node, missing gateway file, exec error.
    Spawn(String),
    /// Framing/versioning violation; the connection is no longer trustworthy.
    Protocol(String),
    /// Local I/O failure on the child's stdio.
    Io(String),
    /// The child exited or closed its stream mid-request.
    ChildExited(String),
    /// No reply within the request budget plus grace.
    Timeout(Duration),
    /// Terminal cancellation acknowledged by the gateway.
    Canceled(String),
    /// Stable, sanitized gateway/provider error.
    Remote(RemoteError),
}

impl GatewayError {
    pub fn from_remote(error: RemoteError) -> Self {
        GatewayError::Remote(error)
    }

    /// Stable code for the run ledger.
    pub fn code(&self) -> String {
        match self {
            GatewayError::Spawn(_) => "spawn".into(),
            GatewayError::Protocol(_) => "protocol".into(),
            GatewayError::Io(_) => "io".into(),
            GatewayError::ChildExited(_) => "child_exited".into(),
            GatewayError::Timeout(_) => "timeout".into(),
            GatewayError::Canceled(_) => "canceled".into(),
            GatewayError::Remote(remote) => remote.code.clone(),
        }
    }
}

impl fmt::Display for GatewayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GatewayError::Spawn(message)
            | GatewayError::Protocol(message)
            | GatewayError::Io(message)
            | GatewayError::ChildExited(message) => write!(formatter, "{message}"),
            GatewayError::Timeout(duration) => {
                write!(formatter, "no gateway reply within {duration:?}")
            }
            GatewayError::Canceled(reason) => {
                write!(formatter, "request canceled{}", reason_suffix(reason))
            }
            GatewayError::Remote(remote) => {
                write!(
                    formatter,
                    "gateway error [{}]: {}",
                    remote.code, remote.message
                )
            }
        }
    }
}

fn reason_suffix(reason: &str) -> String {
    if reason.is_empty() {
        String::new()
    } else {
        format!(" ({reason})")
    }
}

impl std::error::Error for GatewayError {}

/// The seam scouting code depends on; the process client implements it and
/// tests substitute a scripted double.
pub trait LlmGateway {
    fn capabilities(
        &mut self,
        model: Option<&str>,
    ) -> Result<(ProviderSummary, Option<ModelCapabilities>), GatewayError>;

    fn complete(
        &mut self,
        request: &CompleteRequest,
        timeout: Duration,
    ) -> Result<CompletionOutcome, GatewayError>;

    /// Complete an already bounded set of independent scouting requests.
    /// Test doubles and non-process implementations retain deterministic
    /// serial behavior unless they explicitly provide concurrent transport.
    fn complete_batch(
        &mut self,
        tasks: &[CompletionTask<'_>],
    ) -> Vec<Result<CompletionOutcome, GatewayError>> {
        tasks
            .iter()
            .map(|task| self.complete(task.request, task.timeout))
            .collect()
    }
}

/// `jscout llm doctor`: report every layer needed for a model call. With no
/// override it diagnoses the same plan-backed model scouting uses.
pub fn doctor(
    model: Option<&str>,
    gateway_path: Option<&std::path::Path>,
    runtime: &crate::config::RuntimeConfig,
) -> anyhow::Result<()> {
    let node = config::resolve_node_setting(&runtime.effective.sidecars.node, "the pi-ai gateway")?;
    println!("node: {}", node.display());
    let version = config::verify_node_version(&node)?;
    println!(
        "node version: {version} (required >= {})",
        config::MINIMUM_NODE_VERSION_TEXT
    );

    let gateway_file = config::resolve_gateway_setting(
        gateway_path,
        runtime.effective.sidecars.gateway.as_deref(),
    )?;
    println!("gateway: {}", gateway_file.display());

    let mut gateway = process::ProcessGateway::launch(gateway_path, runtime)?;
    println!(
        "gateway version: {} (protocol {}), pi-ai {}, gateway node {}",
        gateway.versions.gateway,
        gateway.versions.protocol,
        gateway.versions.pi_ai,
        gateway.versions.node
    );

    let requested_model = Some(config::resolve_model_setting(
        model,
        &runtime.effective.llm.model,
    )?);
    let (providers, capabilities) =
        gateway.capabilities(requested_model.as_ref().map(|spec| spec.spec.as_str()))?;
    println!(
        "providers: {} builtin, {} custom{}",
        providers.builtin,
        providers.custom.len(),
        if providers.custom.is_empty() {
            String::new()
        } else {
            format!(" ({})", providers.custom.join(", "))
        }
    );
    match (&requested_model, capabilities) {
        (Some(spec), Some(model)) => {
            println!(
                "model {}:{} ({}): api {}, tools {}, context {}, max output {}, reasoning {}, service tier {}",
                model.provider,
                model.model,
                spec.spec,
                model.api,
                if model.supports_tools {
                    "supported"
                } else {
                    "unsupported"
                },
                model
                    .context_window
                    .map_or_else(|| "unknown".into(), |value| value.to_string()),
                model
                    .max_tokens
                    .map_or_else(|| "unknown".into(), |value| value.to_string()),
                if model.reasoning {
                    "supported"
                } else {
                    "unsupported"
                },
                if model.supports_service_tier {
                    "supported"
                } else {
                    "unsupported"
                },
            );
            println!(
                "endpoint: {}",
                model.base_url.as_deref().unwrap_or("unknown")
            );
            println!(
                "billing path: {} (configured routing only; no provider request made)",
                model.billing_path.as_deref().unwrap_or("unknown")
            );
            if !model.auth_configured {
                anyhow::bail!(
                    "no usable authentication configuration was found for provider {}; configure its API-key environment variable or pi-ai OAuth auth store before scouting",
                    model.provider
                );
            }
            println!(
                "auth: configured (type {}, source {}); credential validity, quota, and billing were not checked against the provider",
                model.auth_type.as_deref().unwrap_or("unknown"),
                model.auth_source.as_deref().unwrap_or("unknown")
            );
        }
        (Some(spec), None) => {
            anyhow::bail!(
                "model {} is not known to the gateway; check the provider id and model id",
                spec.spec
            );
        }
        (None, _) => {
            println!(
                "model: none selected (pass --model or set llm.model in .jscout.toml, e.g. {})",
                config::MODEL_EXAMPLE
            );
        }
    }
    println!("doctor: local runtime, model, and auth configuration ok (no model request made)");
    Ok(())
}
