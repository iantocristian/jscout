use std::collections::BTreeSet;
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
        checkers: usize,
    },
}

impl Backend {
    pub const fn name(self) -> &'static str {
        match self {
            Self::TypeScript => "typescript",
            Self::Tsr { .. } => "tsr-embedded",
        }
    }

    pub const fn checkers_per_project(self) -> usize {
        match self {
            Self::TypeScript => 1,
            Self::Tsr { checkers, .. } => checkers,
        }
    }
}

pub enum Checker {
    TypeScript(ProcessChecker),
    Tsr(Box<EmbeddedChecker>),
}

impl Checker {
    pub fn embedded(root: &Path, diagnostics: bool, checkers: usize) -> Result<Self, CheckerError> {
        let mut checker = EmbeddedChecker::new(root, diagnostics)?;
        checker.configure_checkers(checkers)?;
        Ok(Self::Tsr(Box::new(checker)))
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

    pub(super) fn take_project_plan(&mut self) -> Option<super::embedded::ProjectPlan> {
        match self {
            Self::TypeScript(_) => None,
            Self::Tsr(checker) => checker.take_project_plan(),
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
    ) -> Result<BTreeSet<String>, CheckerError> {
        match self {
            Self::TypeScript(_) => Ok(BTreeSet::new()),
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
