use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 4;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outbound {
    Hello,
    Capabilities,
    PlanMembersBegin {
        total_files: usize,
        refresh_config: bool,
    },
    PlanMembersAdd {
        files: Vec<String>,
    },
    PlanMembersFinish,
    PlanMembersNext {
        cursor: String,
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
    PlanMembersReady {
        id: String,
        total_files: usize,
    },
    PlanMembersAddResult {
        id: String,
        received_files: usize,
    },
    PlanMembersPage {
        id: String,
        page: MemberPlanPage,
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
            | Self::PlanMembersReady { id, .. }
            | Self::PlanMembersAddResult { id, .. }
            | Self::PlanMembersPage { id, .. }
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
pub struct MemberPlanPage {
    #[serde(default)]
    pub typescript: Option<TypeScriptIdentity>,
    #[serde(default)]
    pub totals: Option<MemberPlanTotals>,
    #[serde(default)]
    pub files: Vec<FileOwnership>,
    #[serde(default)]
    pub projects: Vec<ProjectSummary>,
    #[serde(default)]
    pub configuration_problems: Vec<ConfigurationProblem>,
    #[serde(default)]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MemberPlanTotals {
    pub files: usize,
    pub projects: usize,
    pub configuration_problems: usize,
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

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemberBatchResult {
    pub project_id: String,
    pub typescript: TypeScriptIdentity,
    pub checker_input_fingerprint: String,
    pub results: Vec<MemberProjectResult>,
    pub resources: ResourceUsage,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct MemberProjectResult {
    pub indexed_hash: String,
    pub source_hash: String,
    pub answer: ProjectAnswer,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct ResourceUsage {
    pub rss_bytes: u64,
    pub heap_used_bytes: u64,
    pub heap_total_bytes: u64,
}

/// Display metadata only; never retain an unbounded compiler type spelling.
pub const MAX_RECEIVER_TYPE_BYTES: usize = 4 * 1024;
pub const RECEIVER_TYPE_TRUNCATION: &str = "... [truncated]";

pub fn bounded_receiver_type(text: &str) -> String {
    if text.len() <= MAX_RECEIVER_TYPE_BYTES {
        return text.to_owned();
    }
    let mut end = MAX_RECEIVER_TYPE_BYTES - RECEIVER_TYPE_TRUNCATION.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut display = String::with_capacity(MAX_RECEIVER_TYPE_BYTES);
    display.push_str(&text[..end]);
    display.push_str(RECEIVER_TYPE_TRUNCATION);
    display
}

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize)]
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

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
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
    fn receiver_display_is_bounded_by_utf8_bytes_with_an_explicit_marker() {
        for text in [
            String::new(),
            "ShortType".into(),
            "x".repeat(MAX_RECEIVER_TYPE_BYTES),
            "😀".repeat(MAX_RECEIVER_TYPE_BYTES / 4),
        ] {
            assert_eq!(bounded_receiver_type(&text), text);
        }
        for text in [
            "x".repeat(MAX_RECEIVER_TYPE_BYTES + 1),
            "😀".repeat(2048),
            format!("{}é", "x".repeat(MAX_RECEIVER_TYPE_BYTES - 1)),
        ] {
            let display = bounded_receiver_type(&text);
            assert!(display.len() <= MAX_RECEIVER_TYPE_BYTES);
            let prefix = display.strip_suffix(RECEIVER_TYPE_TRUNCATION).unwrap();
            assert!(text.starts_with(prefix));
            assert_eq!(bounded_receiver_type(&display), display);
        }
    }

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
        assert_eq!(frame["protocol"], 4);
        assert_eq!(frame["query"]["receiver_start"], 10);
        assert_eq!(frame["query"]["property_end"], 27);
    }
}
