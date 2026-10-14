use std::path::Path;
use std::time::Duration;

use super::embedded::EmbeddedChecker;
use super::process::{CheckerError, ProcessChecker};
use super::protocol::{
    Capabilities, MemberBatchResult, MemberPlanResult, MemberQuery, PROTOCOL_VERSION,
    ProjectValidationResult,
};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    TypeScript,
    Tsr {
        diagnostics: bool,
    },
}

impl Backend {
    pub const fn name(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Tsr { .. } => "tsr-embedded",
        }
    }
}

pub enum Checker {
    TypeScript(ProcessChecker),
    Tsr(Box<EmbeddedChecker>),
}

impl Checker {
    pub fn embedded(root: &Path, diagnostics: bool) -> Result<Self, CheckerError> {
        Ok(Self::Tsr(Box::new(EmbeddedChecker::new(
            root,
            diagnostics,
        )?)))
    }

    pub fn protocol_version(&self) -> u32 {
        match self {
            Self::TypeScript(checker) => checker.versions.protocol,
            Self::Tsr(_) => PROTOCOL_VERSION,
        }
    }

    pub fn register_interrupts(&mut self) -> Result<(), CheckerError> {
        match self {
            Self::TypeScript(checker) => checker.register_interrupts(),
            Self::Tsr(checker) => checker.register_interrupts(),
        }
    }

    pub fn capabilities(&mut self, timeout: Duration) -> Result<Capabilities, CheckerError> {
        match self {
            Self::TypeScript(checker) => checker.capabilities(timeout),
            Self::Tsr(checker) => checker.capabilities(timeout),
        }
    }

    pub fn plan_members(
        &mut self,
        files: Vec<String>,
        timeout: Duration,
    ) -> Result<MemberPlanResult, CheckerError> {
        match self {
            Self::TypeScript(checker) => checker.plan_members(files, timeout),
            Self::Tsr(checker) => checker.plan_members(files, timeout, true),
        }
    }

    pub fn plan_members_cached(
        &mut self,
        files: Vec<String>,
        timeout: Duration,
    ) -> Result<MemberPlanResult, CheckerError> {
        match self {
            Self::TypeScript(checker) => checker.plan_members_cached(files, timeout),
            Self::Tsr(checker) => checker.plan_members(files, timeout, false),
        }
    }

    pub fn resolve_members(
        &mut self,
        project_id: String,
        project_files: Vec<String>,
        queries: Vec<MemberQuery>,
        timeout: Duration,
    ) -> Result<MemberBatchResult, CheckerError> {
        match self {
            Self::TypeScript(checker) => {
                checker.resolve_members(project_id, project_files, queries, timeout)
            }
            Self::Tsr(checker) => {
                checker.resolve_members(project_id, project_files, queries, timeout)
            }
        }
    }

    pub fn prepare_project(
        &mut self,
        project_id: &str,
        project_files: &[String],
        queries: &[MemberQuery],
        timeout: Duration,
    ) -> Result<(), CheckerError> {
        match self {
            Self::TypeScript(_) => Ok(()),
            Self::Tsr(checker) => {
                checker.prepare_project(project_id, project_files, queries, timeout)
            }
        }
    }

    pub fn validate_project(
        &mut self,
        project_id: String,
        fingerprint: String,
        timeout: Duration,
    ) -> Result<ProjectValidationResult, CheckerError> {
        match self {
            Self::TypeScript(checker) => checker.validate_project(project_id, fingerprint, timeout),
            Self::Tsr(checker) => checker.validate_project(project_id, fingerprint, timeout),
        }
    }
}
