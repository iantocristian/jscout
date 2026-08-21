//! Protocol-v1 wire structs for the pi-ai gateway. One JSON object per line;
//! every message carries `protocol`, `id`, and `kind`. These mirror
//! gateway/src exactly; version skew is negotiated through `hello`/`ready`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outbound {
    Hello,
    Capabilities {
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Complete(Box<CompleteRequest>),
    Cancel {
        target_id: String,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompleteRequest {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub tool: SubmitTool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_options: Option<ProviderOptions>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: &'static str,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubmitTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderOptions {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[expect(dead_code)] // cancel acknowledgement fields are consumed in G4
pub enum Inbound {
    Ready {
        id: String,
        versions: GatewayVersions,
    },
    CapabilitiesResult {
        id: String,
        providers: ProviderSummary,
        #[serde(default)]
        model: Option<ModelCapabilities>,
    },
    Started {
        id: String,
        provider: String,
        model: String,
        api: String,
        #[serde(default)]
        base_url: Option<String>,
        billing_path: String,
        auth_source: String,
    },
    Result {
        id: String,
        tool_call: ToolCall,
        stop_reason: String,
        usage: Usage,
        #[serde(default = "one_attempt")]
        attempts: u64,
        #[serde(default)]
        response_model: Option<String>,
    },
    Error {
        id: String,
        error: RemoteError,
    },
    Canceled {
        id: String,
        #[serde(default)]
        reason: Option<String>,
    },
    CancelResult {
        id: String,
        target_id: String,
        active: bool,
    },
    ShutdownResult {
        id: String,
    },
}

impl Inbound {
    pub fn id(&self) -> &str {
        match self {
            Inbound::Ready { id, .. }
            | Inbound::CapabilitiesResult { id, .. }
            | Inbound::Started { id, .. }
            | Inbound::Result { id, .. }
            | Inbound::Error { id, .. }
            | Inbound::Canceled { id, .. }
            | Inbound::CancelResult { id, .. }
            | Inbound::ShutdownResult { id } => id,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GatewayVersions {
    pub gateway: String,
    pub pi_ai: String,
    pub node: String,
    pub protocol: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderSummary {
    pub builtin: u64,
    #[serde(default)]
    pub custom: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelCapabilities {
    pub provider: String,
    pub model: String,
    pub api: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    pub reasoning: bool,
    pub supports_service_tier: bool,
    pub supports_tools: bool,
    #[serde(default)]
    pub billing_path: Option<String>,
    #[serde(default)]
    pub auth_configured: bool,
    #[serde(default)]
    pub auth_type: Option<String>,
    #[serde(default)]
    pub auth_source: Option<String>,
}

const fn one_attempt() -> u64 {
    1
}

#[derive(Debug, Clone, Deserialize)]
pub struct ToolCall {
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: Option<u64>,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub total_tokens: u64,
    pub cost_total: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "retained for ledger diagnostics")
    )]
    pub retryable: bool,
    #[serde(default)]
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "retained for ledger diagnostics")
    )]
    pub capacity: bool,
}

/// Serialize one outbound message as a protocol line (no trailing newline).
pub fn encode(id: &str, message: &Outbound) -> serde_json::Result<String> {
    #[derive(Serialize)]
    struct Envelope<'a> {
        protocol: u32,
        id: &'a str,
        #[serde(flatten)]
        message: &'a Outbound,
    }
    serde_json::to_string(&Envelope {
        protocol: PROTOCOL_VERSION,
        id,
        message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_protocol_envelopes_and_decodes_gateway_messages() {
        let line = encode("r1", &Outbound::Hello).expect("encode");
        assert_eq!(line, r#"{"protocol":1,"id":"r1","kind":"hello"}"#);

        let decoded: Inbound = serde_json::from_str(
            r#"{"protocol":1,"id":"r1","kind":"ready",
                "versions":{"gateway":"0.1.0","pi_ai":"0.84.1","node":"22.19.0","protocol":1}}"#,
        )
        .expect("decode ready");
        assert_eq!(decoded.id(), "r1");
        let Inbound::Ready { versions, .. } = decoded else {
            panic!("expected ready");
        };
        assert_eq!(versions.protocol, 1);

        let error: Inbound = serde_json::from_str(
            r#"{"protocol":1,"id":"x","kind":"error",
                "error":{"code":"capacity","message":"rate limit","retryable":true,"capacity":true}}"#,
        )
        .expect("decode error");
        let Inbound::Error { error, .. } = error else {
            panic!("expected error");
        };
        assert!(error.retryable && error.capacity);
    }
}
