//! In-process compiler adapter. Results escape as owned protocol records;
//! compiler handles and AST owners live only for one selected project.
mod diagnostics;
pub(crate) mod host;
mod projects;
mod query;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use crate::checker::process::{self, CheckerError};
use crate::checker::protocol::{
    Capabilities, MemberBatchResult, MemberPlanResult, MemberProjectResult, MemberQuery,
    ProjectAnswer, ProjectValidationResult, RemoteError, ResourceUsage, TypeScriptIdentity,
};

pub fn error(code: &str, message: impl Into<String>) -> CheckerError {
    CheckerError::Remote {
        code: code.into(),
        message: message.into(),
    }
}

pub fn path_text(path: &Path) -> Result<String, CheckerError> {
    path.to_str()
        .map(|path| path.replace(std::path::MAIN_SEPARATOR, "/"))
        .ok_or_else(|| error("unsupported_path", "checker paths must be UTF-8"))
}

pub fn relative(root: &Path, path: &Path) -> Result<String, CheckerError> {
    let path = path
        .strip_prefix(root)
        .map_err(|_| error("outside_root", "path escapes the repository"))?;
    if path.as_os_str().is_empty() {
        Ok(".".into())
    } else {
        path_text(path)
    }
}

pub fn query_file(root: &Path, name: &str) -> Result<PathBuf, CheckerError> {
    let path = Path::new(name);
    if name.is_empty()
        || path.is_absolute()
        || path.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(error(
            "outside_root",
            "query file must be a repository-relative path",
        ));
    }
    let canonical = std::fs::canonicalize(root.join(path))
        .map_err(|e| error("query_file_missing", format!("{name}: {e}")))?;
    if !canonical.starts_with(root) {
        return Err(error(
            "outside_root",
            "query resolves outside the repository",
        ));
    }
    if !canonical.is_file() {
        return Err(error("query_file_missing", "query path is not a file"));
    }
    Ok(canonical)
}

pub fn identity() -> TypeScriptIdentity {
    static VERSION: OnceLock<String> = OnceLock::new();
    let version = VERSION.get_or_init(|| {
        let mut hash = blake3::Hasher::new();
        for bytes in [
            include_bytes!("../../../Cargo.lock").as_slice(),
            include_bytes!("mod.rs"),
            include_bytes!("diagnostics.rs"),
            include_bytes!("projects.rs"),
            include_bytes!("query.rs"),
            include_bytes!("host.rs"),
        ] {
            hash.update(&(bytes.len() as u64).to_le_bytes());
            hash.update(bytes);
        }
        for (name, bytes) in tsr_bundled::LIBRARIES {
            hash.update(name.as_bytes());
            hash.update(bytes);
        }
        format!("tsr-0.4.0+2009f6ec.{}", &hash.finalize().to_hex()[..16])
    });
    TypeScriptIdentity {
        version: version.clone(),
        source: "tsr-embedded".into(),
    }
}

pub struct Request {
    deadline: Instant,
    timeout: Duration,
    diagnostics: diagnostics::Diagnostics,
}

impl Request {
    fn new(timeout: Duration, diagnostics: bool) -> Result<Arc<Self>, CheckerError> {
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or_else(|| error("protocol", "checker timeout is too large"))?;
        Ok(Arc::new(Self {
            deadline,
            timeout,
            diagnostics: diagnostics::Diagnostics::new(diagnostics),
        }))
    }

    pub fn check(&self) -> Result<(), CheckerError> {
        if process::cancellation_pending() {
            return Err(CheckerError::Canceled(
                "embedded checker operation superseded or interrupted".into(),
            ));
        }
        if Instant::now() >= self.deadline {
            return Err(CheckerError::Timeout(self.timeout));
        }
        Ok(())
    }
}

pub struct EmbeddedChecker {
    root: PathBuf,
    discovery: Option<projects::Discovery>,
    planned_project: Option<Arc<projects::Project>>,
    built: Option<query::BuiltProject>,
    returned_batch: bool,
    preparing: bool,
    prepared: Option<BTreeMap<String, MemberProjectResult>>,
    interrupts_registered: bool,
    diagnostics: bool,
    failed_files: BTreeMap<String, RemoteError>,
}

/// Immutable configured-project seeds for one enrichment invocation. Workers
/// fork their observations into independent live hosts before loading a program;
/// neither a compiler session nor its mutable filesystem cache is shared.
pub(super) struct ProjectPlan {
    root: PathBuf,
    projects: BTreeMap<String, Arc<projects::Project>>,
}

impl ProjectPlan {
    pub(super) fn checker(
        &self,
        project_id: &str,
        diagnostics: bool,
    ) -> Result<EmbeddedChecker, CheckerError> {
        let mut checker = EmbeddedChecker::new(&self.root, diagnostics)?;
        if !project_id.starts_with("inferred:") {
            checker.planned_project = Some(
                self.projects
                    .get(project_id)
                    .ok_or_else(|| {
                        error("project_not_found", "project missing from admitted plan")
                    })?
                    .clone(),
            );
        }
        Ok(checker)
    }
}

impl EmbeddedChecker {
    pub fn new(root: &Path, diagnostics: bool) -> Result<Self, CheckerError> {
        let root = std::fs::canonicalize(root).map_err(|e| error("outside_root", e.to_string()))?;
        if !root.is_dir() {
            return Err(error("outside_root", "repository root is not a directory"));
        }
        Ok(Self {
            root,
            discovery: None,
            planned_project: None,
            built: None,
            returned_batch: false,
            preparing: false,
            prepared: None,
            interrupts_registered: false,
            diagnostics,
            failed_files: BTreeMap::new(),
        })
    }

    pub(super) fn take_project_plan(&mut self) -> Option<ProjectPlan> {
        let discovery = self.discovery.take()?;
        Some(ProjectPlan {
            root: self.root.clone(),
            projects: discovery
                .projects
                .into_iter()
                .filter(|(id, _)| !id.starts_with("inferred:"))
                .map(|(id, project)| (id, Arc::new(project)))
                .collect(),
        })
    }

    pub fn register_interrupts(&mut self) -> Result<(), CheckerError> {
        if !self.interrupts_registered {
            process::register_embedded_interrupts()?;
            self.interrupts_registered = true;
        }
        Ok(())
    }

    fn request<T>(
        &mut self,
        timeout: Duration,
        task: impl FnOnce(&mut Self, &Arc<Request>) -> Result<T, CheckerError>,
    ) -> Result<T, CheckerError> {
        let request = Request::new(timeout, self.diagnostics)?;
        if let Some(built) = &self.built {
            built.project.host.set_request(request.clone());
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            request.check()?;
            task(self, &request)
        }));
        let result = request.check().and_then(|()| match result {
            Ok(result) => result,
            Err(payload) => {
                let detail = payload
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| payload.downcast_ref::<&str>().copied())
                    .unwrap_or("non-string panic payload");
                Err(error(
                    "checker_failure",
                    format!("embedded TSR panicked: {detail}"),
                ))
            }
        });
        let result = result.map_err(|error| request.diagnostics.annotate(error));
        if result.is_err() {
            if let Some(built) = self.built.take() {
                built.session.retire();
            }
            self.discovery = None;
            self.planned_project = None;
            self.returned_batch = false;
            self.prepared = None;
            self.failed_files.clear();
        }
        result
    }

    pub fn capabilities(&mut self, timeout: Duration) -> Result<Capabilities, CheckerError> {
        self.request(timeout, |this, request| {
            request.diagnostics.operation("project-discovery");
            let discovery = projects::discover(&this.root, request)?;
            let result = Capabilities {
                typescript: identity(),
                projects: discovery
                    .projects
                    .values()
                    .map(|p| p.summary.clone())
                    .collect(),
                configuration_problems: discovery.problems.clone(),
                question: "member-declaration-at-occurrence".into(),
            };
            this.discovery = Some(discovery);
            Ok(result)
        })
    }

    pub fn plan_members(
        &mut self,
        mut files: Vec<String>,
        timeout: Duration,
        refresh: bool,
    ) -> Result<MemberPlanResult, CheckerError> {
        self.request(timeout, |this, request| {
            request.diagnostics.operation("project-planning");
            files.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
            if files.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(error("protocol", "duplicate plan file"));
            }
            if refresh || this.discovery.is_none() {
                this.discovery = Some(projects::discover(&this.root, request)?);
            }
            let discovery = this.discovery.as_mut().expect("discovered projects");
            let files = projects::plan(&this.root, discovery, &files, request)?;
            Ok(MemberPlanResult {
                typescript: identity(),
                files,
                projects: discovery
                    .projects
                    .values()
                    .map(|p| p.summary.clone())
                    .collect(),
                configuration_problems: discovery.problems.clone(),
            })
        })
    }

    /// Resolve selected queries before exposing any batch. TSR can defer host
    /// lookups until type printing, so the complete observed manifest is known
    /// only after this pass. Answers are owned and reused, not checked twice.
    /// Return failed files so callers also invalidate any previously staged answers.
    pub fn prepare_project(
        &mut self,
        project_id: &str,
        project_files: &[String],
        queries: &[MemberQuery],
        timeout: Duration,
    ) -> Result<BTreeSet<String>, CheckerError> {
        if self.built.is_some() || queries.is_empty() {
            return Err(error(
                "protocol",
                "prepare_project requires a fresh checker and selected queries",
            ));
        }
        self.preparing = true;
        let result = (|| {
            let mut prepared = BTreeMap::new();
            crate::progress::stage(
                format!("checker preparation: {project_id}"),
                Some(queries.len()),
            );
            for batch in queries.chunks(128) {
                let result = self.resolve_members(
                    project_id.into(),
                    project_files.to_vec(),
                    batch.to_vec(),
                    timeout,
                )?;
                for (query, answer) in batch.iter().zip(result.results) {
                    prepared.insert(query_key(query)?, answer);
                }
                crate::progress::advance(batch.len());
            }
            let fingerprint = &self.built.as_ref().expect("prepared program").fingerprint;
            for answer in prepared.values_mut() {
                answer
                    .answer
                    .checker_input_fingerprint
                    .clone_from(fingerprint);
            }
            self.prepared = Some(prepared);
            Ok(self.failed_files.keys().cloned().collect())
        })();
        self.preparing = false;
        result
    }

    pub fn resolve_members(
        &mut self,
        project_id: String,
        project_files: Vec<String>,
        queries: Vec<MemberQuery>,
        timeout: Duration,
    ) -> Result<MemberBatchResult, CheckerError> {
        self.request(timeout, |this, request| {
            request
                .diagnostics
                .project(&project_id, "project-discovery");
            if queries.is_empty() || queries.len() > 512 {
                return Err(error(
                    "protocol",
                    "resolve_members requires 1 to 512 queries",
                ));
            }
            if let Some(built) = &this.built {
                if built.project.summary.project_id != project_id {
                    return Err(error(
                        "project_switch",
                        "one checker session may resolve only one project",
                    ));
                }
                if project_id.starts_with("inferred:") {
                    let roots = project_files
                        .iter()
                        .map(|file| query_file(&this.root, file))
                        .collect::<Result<BTreeSet<_>, _>>()?;
                    if roots.len() != project_files.len() || roots != built.project.roots {
                        return Err(error(
                            "project_mismatch",
                            "inferred membership changed within a session",
                        ));
                    }
                }
            } else {
                let project = if project_id.starts_with("inferred:") {
                    projects::inferred_from_request(
                        &this.root,
                        project_id.clone(),
                        project_files,
                        request,
                    )?
                } else if let Some(project) = this.planned_project.take() {
                    if project.summary.project_id != project_id {
                        return Err(error(
                            "project_mismatch",
                            "project differs from admitted plan",
                        ));
                    }
                    project.fork(request)?
                } else {
                    let mut discovery = projects::discover(&this.root, request)?;
                    discovery.projects.remove(&project_id).ok_or_else(|| {
                        error(
                            "project_not_found",
                            format!("project not found or invalid: {project_id}"),
                        )
                    })?
                };
                this.built = Some(query::build(&this.root, project, request)?);
                this.discovery = None;
            }
            let built = this.built.as_mut().expect("loaded project");
            let previous_fingerprint = built.fingerprint.clone();
            let mut by_file: BTreeMap<String, Vec<usize>> = BTreeMap::new();
            for (index, query) in queries.iter().enumerate() {
                request.diagnostics.query(query, "validate-source");
                request.check()?;
                let path = query_file(&this.root, &query.file)?;
                if !built.project.roots.contains(&path) {
                    return Err(error(
                        "project_mismatch",
                        format!("{} is not owned by {project_id}", query.file),
                    ));
                }
                let raw =
                    std::fs::read(&path).map_err(|e| error("query_file_missing", e.to_string()))?;
                if blake3::hash(&raw).to_hex().as_str() != query.indexed_hash
                    || built.project.host.raw(&path)?.as_ref() != raw
                {
                    return Err(error(
                        "hash_mismatch",
                        format!("source changed after indexing or loading: {}", query.file),
                    ));
                }
                by_file.entry(query.file.clone()).or_default().push(index);
            }
            let mut results = vec![None; queries.len()];
            for (file, indices) in &by_file {
                // Quarantine also overrides earlier prepared successes from
                // this file, even when replay uses different batch boundaries.
                let mut failure = this.failed_files.get(file).cloned();
                for &index in indices {
                    if failure.is_some() {
                        break;
                    }
                    let result = match &this.prepared {
                        Some(prepared) => {
                            request
                                .diagnostics
                                .query(&queries[index], "prepared-answer");
                            prepared
                                .get(&query_key(&queries[index])?)
                                .cloned()
                                .ok_or_else(|| {
                                    error(
                                        "protocol",
                                        "query was not included in project preparation",
                                    )
                                })
                        }
                        None => built.resolve(&this.root, &queries[index], request),
                    };
                    match result {
                        Ok(result) => results[index] = Some(result),
                        Err(CheckerError::Remote { code, message })
                            if code == "checker_failure"
                                || project_id.starts_with("inferred:")
                                    && ["invalid_span", "span_mismatch"]
                                        .contains(&code.as_str()) =>
                        {
                            let CheckerError::Remote { code, message } = request
                                .diagnostics
                                .annotate(CheckerError::Remote { code, message })
                            else {
                                unreachable!("annotation preserves remote errors");
                            };
                            // A returned compiler error can leave query caches
                            // incomplete. Keep the loaded program, but use a new
                            // checker before attempting a healthy sibling file.
                            if code == "checker_failure" {
                                built.restart_checker(request)?;
                            }
                            let error = RemoteError { code, message };
                            this.failed_files.insert(file.clone(), error.clone());
                            failure = Some(error);
                            break;
                        }
                        Err(failure) => return Err(failure),
                    }
                }
                if let Some(failure) = failure {
                    for &index in indices {
                        results[index] =
                            Some(failed_result(&project_id, &queries[index], failure.clone()));
                    }
                }
            }
            request.diagnostics.project(&project_id, "input-manifest");
            built.refresh_manifest()?;
            if !this.preparing && this.returned_batch && built.fingerprint != previous_fingerprint {
                return Err(error(
                    "checker_input_changed",
                    "compiler observed additional inputs after publishing its first batch",
                ));
            }
            let results = results
                .into_iter()
                .map(|result| {
                    let mut result = result.expect("every query resolved or failed");
                    result
                        .answer
                        .checker_input_fingerprint
                        .clone_from(&built.fingerprint);
                    result
                })
                .collect();
            let result = MemberBatchResult {
                project_id,
                typescript: identity(),
                checker_input_fingerprint: built.fingerprint.clone(),
                results,
                resources: ResourceUsage::default(),
            };
            // In-process answers have no JSON transport frame. In particular,
            // one large printed type must not fail an already prepared project.
            this.returned_batch = !this.preparing;
            Ok(result)
        })
    }

    pub fn validate_project(
        &mut self,
        project_id: String,
        fingerprint: String,
        timeout: Duration,
    ) -> Result<ProjectValidationResult, CheckerError> {
        self.request(timeout, |this, request| {
            request
                .diagnostics
                .project(&project_id, "validate-project-inputs");
            let built = this
                .built
                .as_ref()
                .filter(|built| built.project.summary.project_id == project_id)
                .ok_or_else(|| {
                    error(
                        "project_not_loaded",
                        "validate_project must follow resolution in the same session",
                    )
                })?;
            let mut valid = built.fingerprint == fingerprint;
            for input in &built.inputs {
                request.check()?;
                valid &= host::fresh_signature(Path::new(&input.path), &input.source_hash)
                    .as_deref()
                    == Some(input.source_hash.as_str());
            }
            Ok(ProjectValidationResult {
                project_id,
                fingerprint: valid.then(|| built.fingerprint.clone()),
                valid,
                inputs: built.inputs.clone(),
            })
        })
    }
}

fn failed_result(
    project_id: &str,
    query: &MemberQuery,
    failure: RemoteError,
) -> MemberProjectResult {
    MemberProjectResult {
        indexed_hash: query.indexed_hash.clone(),
        source_hash: query.indexed_hash.clone(),
        answer: ProjectAnswer {
            project_id: project_id.into(),
            status: "failed".into(),
            receiver_type: None,
            declarations: Vec::new(),
            checker_input_fingerprint: String::new(),
            error: Some(failure),
        },
    }
}

fn query_key(query: &MemberQuery) -> Result<String, CheckerError> {
    serde_json::to_string(query).map_err(|e| error("protocol", e.to_string()))
}

impl Drop for EmbeddedChecker {
    fn drop(&mut self) {
        if self.interrupts_registered {
            process::unregister_embedded_interrupts();
        }
    }
}

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
