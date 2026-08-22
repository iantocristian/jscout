use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 3;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outbound {
    Hello,
    Capabilities,
    PlanMembers {
        files: Vec<String>,
    },
    #[cfg(test)]
    ResolveMember {
        query: MemberQuery,
    },
    ResolveMembers {
        project_id: String,
        project_files: Vec<String>,
        queries: Vec<MemberQuery>,
    },
    ValidateProject {
        project_id: String,
        fingerprint: String,
    },
    Cancel {
        target_id: String,
    },
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
    PlanMembersResult {
        id: String,
        result: MemberPlanResult,
    },
    #[cfg(test)]
    ResolveMemberResult {
        id: String,
        result: MemberResult,
    },
    ResolveMembersResult {
        id: String,
        result: MemberBatchResult,
    },
    ValidateProjectResult {
        id: String,
        result: ProjectValidationResult,
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
            | Self::PlanMembersResult { id, .. }
            | Self::ResolveMembersResult { id, .. }
            | Self::ValidateProjectResult { id, .. }
            | Self::Error { id, .. }
            | Self::Canceled { id, .. }
            | Self::CancelResult { id, .. }
            | Self::ShutdownResult { id } => id,
            #[cfg(test)]
            Self::ResolveMemberResult { id, .. } => id,
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
    #[serde(default = "default_project_purpose")]
    pub purpose: String,
    #[serde(default)]
    pub purpose_reasons: Vec<String>,
    #[serde(default)]
    pub membership_fingerprint: String,
    #[serde(default)]
    pub config_fingerprint: String,
}

fn default_project_purpose() -> String {
    "general".into()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConfigurationProblem {
    pub project_id: String,
    pub code: String,
    pub message: String,
}

#[cfg(test)]
#[derive(Debug, Clone, Deserialize)]
pub struct MemberResult {
    #[expect(
        dead_code,
        reason = "single-member protocol probe validates this wire field's type"
    )]
    pub indexed_hash: String,
    #[expect(
        dead_code,
        reason = "single-member protocol probe validates this wire field's type"
    )]
    pub source_hash: String,
    #[expect(
        dead_code,
        reason = "single-member protocol probe validates this wire field's type"
    )]
    pub typescript: TypeScriptIdentity,
    pub projects: Vec<ProjectAnswer>,
    #[serde(default)]
    #[expect(
        dead_code,
        reason = "single-member protocol probe validates this wire field's type"
    )]
    pub configuration_problems: Vec<ConfigurationProblem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemberPlanResult {
    pub typescript: TypeScriptIdentity,
    pub files: Vec<FileOwnership>,
    pub projects: Vec<ProjectSummary>,
    #[serde(default)]
    pub configuration_problems: Vec<ConfigurationProblem>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FileOwnership {
    pub file: String,
    pub project_ids: Vec<String>,
    #[serde(default)]
    pub excluded_project_ids: Vec<String>,
    #[serde(default)]
    pub tooling_fallback: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemberBatchResult {
    pub project_id: String,
    pub typescript: TypeScriptIdentity,
    pub checker_input_fingerprint: String,
    pub results: Vec<MemberProjectResult>,
    pub resources: ResourceUsage,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemberProjectResult {
    pub indexed_hash: String,
    pub source_hash: String,
    pub answer: ProjectAnswer,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ResourceUsage {
    pub rss_bytes: u64,
    pub heap_used_bytes: u64,
    pub heap_total_bytes: u64,
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
    #[serde(default)]
    pub error: Option<RemoteError>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DeclarationSite {
    pub file: Option<String>,
    pub outside_root: bool,
    pub start: i64,
    pub end: i64,
    pub source_hash: String,
    /// Sidecar-computed provenance of the declaration file: `repo`, `types`
    /// (`node_modules/@types`), `lib` (the TypeScript standard library),
    /// `vendored` (other `node_modules`), or `outside`. Absent from older
    /// sidecars, which is treated as `repo` so mapping behavior is unchanged.
    #[serde(default)]
    pub context: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CheckerInputFile {
    pub path: String,
    pub source_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectValidationResult {
    pub project_id: String,
    pub fingerprint: Option<String>,
    pub valid: bool,
    #[serde(default)]
    pub inputs: Vec<CheckerInputFile>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
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
        assert_eq!(frame["protocol"], 3);
        assert_eq!(frame["query"]["receiver_start"], 10);
        assert_eq!(frame["query"]["property_end"], 27);
    }
}
