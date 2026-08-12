use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outbound {
    Hello,
    Capabilities,
    ResolveMember { query: MemberQuery },
    ValidateInputs { entries: Vec<InputValidation> },
    Cancel { target_id: String },
    Shutdown,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemberQuery {
    pub file: String,
    pub indexed_hash: String,
    pub call_start: i64,
    pub call_end: i64,
    pub receiver_start: i64,
    pub receiver_end: i64,
    pub property_start: i64,
    pub property_end: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct InputValidation {
    pub file: String,
    pub project_id: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Inbound {
    Ready {
        id: String,
        versions: Versions,
    },
    CapabilitiesResult {
        id: String,
        capabilities: Capabilities,
    },
    ResolveMemberResult {
        id: String,
        result: MemberResult,
    },
    ValidateInputsResult {
        id: String,
        result: ValidationResult,
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
            Self::Ready { id, .. }
            | Self::CapabilitiesResult { id, .. }
            | Self::ResolveMemberResult { id, .. }
            | Self::ValidateInputsResult { id, .. }
            | Self::Error { id, .. }
            | Self::Canceled { id, .. }
            | Self::CancelResult { id, .. }
            | Self::ShutdownResult { id } => id,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Versions {
    pub sidecar: String,
    pub node: String,
    pub protocol: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TypeScriptIdentity {
    pub version: String,
    pub source: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Capabilities {
    pub typescript: TypeScriptIdentity,
    pub projects: Vec<ProjectSummary>,
    pub configuration_problems: Vec<ConfigurationProblem>,
    pub question: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProjectSummary {
    pub project_id: String,
    pub file_count: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConfigurationProblem {
    pub project_id: String,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemberResult {
    pub indexed_hash: String,
    pub source_hash: String,
    pub typescript: TypeScriptIdentity,
    pub projects: Vec<ProjectAnswer>,
    #[serde(default)]
    pub configuration_problems: Vec<ConfigurationProblem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectAnswer {
    pub project_id: String,
    pub status: String,
    #[serde(default)]
    pub receiver_type: Option<String>,
    #[serde(default)]
    pub declarations: Vec<DeclarationSite>,
    pub checker_input_fingerprint: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeclarationSite {
    pub file: Option<String>,
    pub outside_root: bool,
    pub start: i64,
    pub end: i64,
    pub source_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidationResult {
    pub valid: bool,
    pub results: Vec<ValidationEntryResult>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidationEntryResult {
    pub project_id: String,
    pub file: String,
    pub fingerprint: Option<String>,
    pub valid: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoteError {
    pub code: String,
    pub message: String,
}

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
    fn protocol_frames_are_versioned_and_span_bound() {
        let line = encode(
            "r2",
            &Outbound::ResolveMember {
                query: MemberQuery {
                    file: "src/main.ts".into(),
                    indexed_hash: "b3".into(),
                    call_start: 10,
                    call_end: 30,
                    receiver_start: 10,
                    receiver_end: 20,
                    property_start: 21,
                    property_end: 27,
                },
            },
        )
        .expect("frame");
        let frame: serde_json::Value = serde_json::from_str(&line).expect("json");
        assert_eq!(frame["protocol"], 1);
        assert_eq!(frame["query"]["receiver_start"], 10);
        assert_eq!(frame["query"]["property_end"], 27);
    }
}
