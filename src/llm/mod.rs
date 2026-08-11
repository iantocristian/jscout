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
    pub response_model: Option<String>,
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
}

/// `jscout llm doctor`: report every layer needed for a model call. With no
/// override it diagnoses the same plan-backed model scouting uses.
pub fn doctor(model: Option<&str>, gateway_path: Option<&std::path::Path>) -> anyhow::Result<()> {
    let node = config::resolve_node()?;
    println!("node: {}", node.display());
    let version = std::process::Command::new(&node)
        .arg("--version")
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .unwrap_or_else(|error| format!("unavailable ({error})"));
    println!("node version: {version} (required >= 22.19.0)");

    let gateway_file = config::resolve_gateway(gateway_path)?;
    println!("gateway: {}", gateway_file.display());

    let mut gateway = process::ProcessGateway::spawn(&node, &gateway_file)?;
    println!(
        "gateway version: {} (protocol {}), pi-ai {}, gateway node {}",
        gateway.versions.gateway,
        gateway.versions.protocol,
        gateway.versions.pi_ai,
        gateway.versions.node
    );

    let requested_model = Some(config::resolve_model(model)?);
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
        }
        (Some(spec), None) => {
            anyhow::bail!(
                "model {} is not known to the gateway; check the provider id and model id",
                spec.spec
            );
        }
        (None, _) => {
            println!(
                "model: none selected (pass --model or set {}, e.g. {})",
                config::MODEL_ENV,
                config::MODEL_EXAMPLE
            );
        }
    }
    println!("doctor: ok");
    Ok(())
}
