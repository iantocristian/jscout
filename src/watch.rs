use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
#[cfg(unix)]
use std::ffi::OsString;
use std::fmt;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use notify::{RecursiveMode, Watcher};
use rusqlite::Connection;

use crate::{checker, config, docs, embed, formats, indexer, store, structural, walk};

const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_RETRY_INITIAL: Duration = Duration::from_millis(500);
const DEFAULT_RETRY_MAX: Duration = Duration::from_secs(30);
const CHECKER_DRIFT_FLUSH_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const OPTIONAL_PHASE_POLL: Duration = Duration::from_millis(100);
const MAX_INCREMENTAL_SOURCE_PATHS: usize = 256;

pub struct WatchOptions<'a> {
    pub database: Option<&'a Path>,
    pub embed_on_change: bool,
    pub provider: Option<&'a embed::Provider>,
    pub embed_product_only: bool,
    pub dependencies: &'a [String],
    pub docs_include: &'a [String],
    pub docs_exclude: &'a [String],
    /// Effective indexing-time provenance policy. This is already gated by
    /// documentation corpus admission (`docs.enabled`).
    pub docs_freshness: bool,
    pub enrich_on_change: bool,
    pub enrich_timeout: Duration,
    pub checker_sidecar: Option<&'a Path>,
    pub checker_node: &'a str,
    pub timing: bool,
    pub debug: bool,
    pub debounce: Duration,
    pub reconcile_interval: Duration,
    /// Fingerprint of the loaded non-secret runtime configuration baseline.
    /// CLI overrides are rendered separately in the effective watch flags.
    pub config_fingerprint: &'a str,
    pub config_loaded: bool,
    /// Exact startup-resolved configuration path. The default repository path
    /// remains optional; an explicit path must continue to exist on reload.
    pub config_path: Option<&'a Path>,
    pub config_explicit: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DocsIndexingPolicy {
    include: Vec<String>,
    exclude: Vec<String>,
    freshness: bool,
}

impl DocsIndexingPolicy {
    fn from_options(options: &WatchOptions<'_>) -> Self {
        Self {
            include: options.docs_include.to_vec(),
            exclude: options.docs_exclude.to_vec(),
            freshness: options.docs_freshness,
        }
    }

    fn load(root: &Path, options: &WatchOptions<'_>) -> Result<Self> {
        let explicit_path = if options.config_explicit {
            Some(
                options
                    .config_path
                    .context("explicit watch configuration path is missing")?,
            )
        } else {
            None
        };
        let settings = config::load_docs_indexing_settings(root, explicit_path)?;
        Ok(Self {
            include: settings.effective_include().to_vec(),
            exclude: settings.effective_exclude().to_vec(),
            freshness: settings.effective_freshness(),
        })
    }

    fn corpus_options(&self) -> docs::corpus::CorpusOptions {
        docs::corpus::CorpusOptions {
            include: self.include.clone(),
            exclude: self.exclude.clone(),
            ..Default::default()
        }
    }

    fn index_options(&self, options: &WatchOptions<'_>) -> indexer::IndexOptions {
        indexer::IndexOptions {
            dependencies: options.dependencies.to_vec(),
            docs_include: self.include.clone(),
            docs_exclude: self.exclude.clone(),
            docs_freshness: self.freshness,
            timing: options.timing,
            debug: options.debug,
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Refresh,
    Embed,
    Enrich,
    SemanticEmbed,
}

impl fmt::Display for Phase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Refresh => "refresh",
            Self::Embed => "embed",
            Self::Enrich => "enrich",
            Self::SemanticEmbed => "semantic-embed",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum RefreshScope {
    Incremental,
    Full,
}

impl fmt::Display for RefreshScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Incremental => "incremental",
            Self::Full => "full",
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
struct DirtySignal {
    scope: RefreshScope,
    reasons: BTreeSet<String>,
    source_paths: BTreeSet<String>,
    checker_source_paths: BTreeSet<String>,
    checker_refresh_required: bool,
    force_full_enrichment: bool,
}

impl DirtySignal {
    fn full(reason: impl Into<String>) -> Self {
        Self {
            scope: RefreshScope::Full,
            reasons: [reason.into()].into(),
            source_paths: BTreeSet::new(),
            checker_source_paths: BTreeSet::new(),
            checker_refresh_required: true,
            force_full_enrichment: false,
        }
    }

    fn source(reason: impl Into<String>, path: impl Into<String>, checker_eligible: bool) -> Self {
        let path = path.into();
        Self {
            scope: RefreshScope::Incremental,
            reasons: [reason.into()].into(),
            source_paths: [path.clone()].into(),
            checker_source_paths: checker_eligible.then_some(path).into_iter().collect(),
            checker_refresh_required: checker_eligible,
            force_full_enrichment: false,
        }
    }

    fn documentation(reason: impl Into<String>) -> Self {
        Self {
            scope: RefreshScope::Incremental,
            reasons: [reason.into()].into(),
            source_paths: BTreeSet::new(),
            checker_source_paths: BTreeSet::new(),
            // Preserve G24's established shared-snapshot behavior: docs
            // changes refresh checker publication. Only checker-ineligible
            // code formats use the no-provider rebind path.
            checker_refresh_required: true,
            force_full_enrichment: false,
        }
    }

    fn rust_membership(reason: impl Into<String>) -> Self {
        Self {
            scope: RefreshScope::Incremental,
            reasons: [reason.into()].into(),
            source_paths: BTreeSet::new(),
            checker_source_paths: BTreeSet::new(),
            checker_refresh_required: false,
            force_full_enrichment: false,
        }
    }

    /// Schedule a complete-inventory incremental refresh when an event cannot
    /// name one source file. The inventory pass still discovers every added,
    /// removed, or moved code/document file; a destructive canonical reset is
    /// unnecessary unless a known boundary explicitly requests one.
    fn inventory(reason: impl Into<String>) -> Self {
        Self {
            scope: RefreshScope::Incremental,
            reasons: [reason.into()].into(),
            source_paths: BTreeSet::new(),
            checker_source_paths: BTreeSet::new(),
            checker_refresh_required: true,
            force_full_enrichment: false,
        }
    }

    fn checker_drift_flush() -> Self {
        Self {
            scope: RefreshScope::Incremental,
            reasons: ["checker-drift-flush".to_string()].into(),
            source_paths: BTreeSet::new(),
            checker_source_paths: BTreeSet::new(),
            checker_refresh_required: true,
            force_full_enrichment: true,
        }
    }

    fn reconciliation() -> Self {
        Self {
            // Incremental refresh still walks and hashes the complete code and
            // documentation inventory. It differs from full refresh only by
            // retaining unchanged canonical rows instead of rebuilding them.
            scope: RefreshScope::Incremental,
            reasons: ["periodic-reconciliation".to_string()].into(),
            source_paths: BTreeSet::new(),
            checker_source_paths: BTreeSet::new(),
            checker_refresh_required: true,
            force_full_enrichment: false,
        }
    }

    fn merge(&mut self, other: Self) {
        self.scope = self.scope.max(other.scope);
        self.reasons.extend(other.reasons);
        self.source_paths.extend(other.source_paths);
        self.checker_source_paths.extend(other.checker_source_paths);
        self.checker_refresh_required |= other.checker_refresh_required;
        self.force_full_enrichment |= other.force_full_enrichment;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Work {
    generation: u64,
    phase: Phase,
    refresh_scope: RefreshScope,
    force_full_enrichment: bool,
    rebind_checker: bool,
}

#[derive(Clone, Copy, Debug)]
struct Retry {
    work: Work,
    due: Duration,
}

#[derive(Debug, PartialEq, Eq)]
enum FinishState {
    Continue,
    Complete,
    Partial,
    Retry { after: Duration },
    Superseded,
}

/// Pure generation state. The driver supplies monotonic time and executes the
/// returned work, so debounce/retry/supersession tests do not sleep or touch
/// the filesystem.
struct Coordinator {
    desired_generation: u64,
    completed_generation: u64,
    last_dirty_at: Duration,
    refresh_immediate: bool,
    active: Option<Work>,
    ready: Option<Work>,
    retry: Option<Retry>,
    retry_attempts: BTreeMap<Phase, u32>,
    dirty_reasons: BTreeSet<String>,
    dirty_source_paths: BTreeSet<String>,
    checker_dirty_source_paths: BTreeSet<String>,
    checker_refresh_required: bool,
    refresh_scope: RefreshScope,
    debounce: Duration,
    retry_initial: Duration,
    retry_max: Duration,
    embed: bool,
    enrich: bool,
    partial_generation: bool,
    force_full_enrichment: bool,
}

impl Coordinator {
    fn new(debounce: Duration, embed: bool, enrich: bool) -> Self {
        let mut coordinator = Self {
            desired_generation: 0,
            completed_generation: 0,
            last_dirty_at: Duration::ZERO,
            refresh_immediate: false,
            active: None,
            ready: None,
            retry: None,
            retry_attempts: BTreeMap::new(),
            dirty_reasons: BTreeSet::new(),
            dirty_source_paths: BTreeSet::new(),
            checker_dirty_source_paths: BTreeSet::new(),
            checker_refresh_required: false,
            refresh_scope: RefreshScope::Full,
            debounce,
            retry_initial: DEFAULT_RETRY_INITIAL,
            retry_max: DEFAULT_RETRY_MAX,
            embed,
            enrich,
            partial_generation: false,
            force_full_enrichment: false,
        };
        coordinator.mark_dirty(Duration::ZERO, DirtySignal::full("startup"));
        coordinator.refresh_immediate = true;
        coordinator
    }

    fn mark_dirty(&mut self, now: Duration, signal: DirtySignal) {
        // Checker affinity spans generations. A source edit observed while an
        // older enrichment is running must remain dirty until a non-superseded
        // checker publication succeeds. Documentation and checker-ineligible
        // code signals carry no checker source paths and never enter this backlog.
        if self.enrich {
            self.checker_dirty_source_paths
                .extend(signal.checker_source_paths.iter().cloned());
            self.checker_refresh_required |= signal.checker_refresh_required;
        }
        // A failed structural refresh has not consumed its inventory/config
        // requirement. If a newer source event supersedes its parked retry,
        // carry the old scope and reasons into the successor generation.
        let preserve_refresh_requirement = self.retry.is_some_and(|retry| {
            retry.work.generation == self.desired_generation && retry.work.phase == Phase::Refresh
        });
        let preserve_checker_flush_requirement = self.force_full_enrichment;
        let current_generation_has_work = self
            .active
            .is_some_and(|work| work.generation == self.desired_generation)
            || self
                .ready
                .is_some_and(|work| work.generation == self.desired_generation)
            || self
                .retry
                .is_some_and(|retry| retry.work.generation == self.desired_generation);
        if self.desired_generation == self.completed_generation || current_generation_has_work {
            self.desired_generation += 1;
            self.partial_generation = false;
            if !preserve_refresh_requirement {
                self.dirty_reasons.clear();
                self.dirty_source_paths.clear();
                self.refresh_scope = RefreshScope::Incremental;
            }
            if preserve_checker_flush_requirement {
                self.dirty_reasons.insert("checker-drift-flush".to_string());
            }
        }
        self.last_dirty_at = now;
        self.refresh_scope = self.refresh_scope.max(signal.scope);
        self.force_full_enrichment |= signal.force_full_enrichment;
        self.dirty_reasons.extend(
            signal
                .reasons
                .into_iter()
                .filter(|reason| !reason.starts_with("source:")),
        );
        let mut source_overflow = false;
        for path in signal.source_paths {
            if self.dirty_source_paths.contains(&path) {
                continue;
            }
            if self.dirty_source_paths.len() == MAX_INCREMENTAL_SOURCE_PATHS {
                source_overflow = true;
                continue;
            }
            self.dirty_reasons.insert(format!("source:{path}"));
            self.dirty_source_paths.insert(path);
        }
        if source_overflow {
            self.refresh_scope = RefreshScope::Full;
            self.dirty_reasons.insert("mass-source-change".to_string());
        }
        self.refresh_immediate = false;
        self.ready = None;
        self.retry = None;
        self.retry_attempts.clear();
    }

    fn mark_reconciliation(&mut self, now: Duration) {
        debug_assert!(self.is_clean());
        self.mark_dirty(now, DirtySignal::reconciliation());
        self.refresh_immediate = true;
    }

    fn mark_checker_drift_flush(&mut self, now: Duration) {
        debug_assert!(self.is_clean());
        debug_assert!(self.enrich);
        self.mark_dirty(now, DirtySignal::checker_drift_flush());
        self.refresh_immediate = true;
    }

    fn next_work(&mut self, now: Duration) -> Option<Work> {
        if self.active.is_some() {
            return None;
        }
        if self
            .ready
            .is_some_and(|work| work.generation != self.desired_generation)
        {
            self.ready = None;
        }
        if self
            .retry
            .is_some_and(|retry| retry.work.generation != self.desired_generation)
        {
            self.retry = None;
        }
        if let Some(work) = self.ready.take() {
            self.active = Some(work);
            return Some(work);
        }
        if let Some(retry) = self.retry {
            if now < retry.due {
                return None;
            }
            self.retry = None;
            self.active = Some(retry.work);
            return Some(retry.work);
        }
        if self.desired_generation <= self.completed_generation {
            return None;
        }
        if !self.refresh_immediate && now < self.last_dirty_at.saturating_add(self.debounce) {
            return None;
        }
        self.refresh_immediate = false;
        let work = Work {
            generation: self.desired_generation,
            phase: Phase::Refresh,
            refresh_scope: self.refresh_scope,
            force_full_enrichment: self.force_full_enrichment,
            rebind_checker: self.enrich && !self.needs_enrichment(),
        };
        self.active = Some(work);
        Some(work)
    }

    fn next_deadline(&self) -> Option<Duration> {
        if self.active.is_some() || self.ready.is_some() {
            return Some(Duration::ZERO);
        }
        if let Some(retry) = self.retry {
            return Some(retry.due);
        }
        (self.desired_generation > self.completed_generation).then(|| {
            if self.refresh_immediate {
                Duration::ZERO
            } else {
                self.last_dirty_at.saturating_add(self.debounce)
            }
        })
    }

    fn finish_refresh(&mut self, work: Work) -> FinishState {
        self.clear_active(work);
        if work.generation != self.desired_generation {
            return FinishState::Superseded;
        }
        // A returned outcome means the refresh successfully published the
        // indexable corpus. Non-retryable file reads and deterministic
        // extraction skips are diagnostics, not phase failures. Retryable
        // reads and other phase errors return Err and enter finish_error.
        self.retry_attempts.remove(&Phase::Refresh);
        self.advance(work)
    }

    fn finish_optional(&mut self, work: Work) -> FinishState {
        self.clear_active(work);
        if work.generation != self.desired_generation {
            return FinishState::Superseded;
        }
        self.retry_attempts.remove(&work.phase);
        if work.phase == Phase::Enrich {
            self.checker_dirty_source_paths.clear();
            self.checker_refresh_required = false;
        }
        self.advance(work)
    }

    fn finish_enrichment_success(&mut self, work: Work) -> FinishState {
        debug_assert_eq!(work.phase, Phase::Enrich);
        self.clear_active(work);
        if work.generation != self.desired_generation {
            return FinishState::Superseded;
        }
        self.retry_attempts.remove(&work.phase);
        self.checker_dirty_source_paths.clear();
        self.checker_refresh_required = false;
        self.advance(work)
    }

    fn finish_optional_partial(&mut self, work: Work) -> FinishState {
        self.clear_active(work);
        if work.generation != self.desired_generation {
            return FinishState::Superseded;
        }
        self.retry_attempts.remove(&work.phase);
        self.partial_generation = true;
        self.advance(work)
    }

    fn finish_error(&mut self, now: Duration, work: Work) -> FinishState {
        self.clear_active(work);
        if work.generation != self.desired_generation {
            return FinishState::Superseded;
        }
        self.schedule_retry(now, work)
    }

    fn clear_active(&mut self, work: Work) {
        debug_assert_eq!(self.active, Some(work));
        self.active = None;
    }

    fn schedule_retry(&mut self, now: Duration, work: Work) -> FinishState {
        let attempts = self.retry_attempts.entry(work.phase).or_default();
        *attempts += 1;
        let multiplier = 1u32
            .checked_shl((*attempts - 1).min(16))
            .unwrap_or(u32::MAX);
        let after = self
            .retry_initial
            .saturating_mul(multiplier)
            .min(self.retry_max);
        self.retry = Some(Retry {
            work,
            due: now.saturating_add(after),
        });
        FinishState::Retry { after }
    }

    fn advance(&mut self, work: Work) -> FinishState {
        let next = match work.phase {
            Phase::Refresh if self.embed => Some(Phase::Embed),
            Phase::Refresh if self.needs_enrichment() => Some(Phase::Enrich),
            Phase::Embed if self.needs_enrichment() => Some(Phase::Enrich),
            Phase::Embed => Some(Phase::SemanticEmbed),
            Phase::Enrich if self.embed => Some(Phase::SemanticEmbed),
            _ => None,
        };
        if let Some(phase) = next {
            self.ready = Some(Work {
                generation: work.generation,
                phase,
                refresh_scope: work.refresh_scope,
                force_full_enrichment: work.force_full_enrichment,
                rebind_checker: work.rebind_checker,
            });
            FinishState::Continue
        } else {
            self.completed_generation = work.generation;
            self.dirty_reasons.clear();
            self.dirty_source_paths.clear();
            let partial = std::mem::take(&mut self.partial_generation);
            if !partial {
                self.force_full_enrichment = false;
            }
            if partial {
                FinishState::Partial
            } else {
                FinishState::Complete
            }
        }
    }

    fn is_superseded(&self, work: Work) -> bool {
        self.desired_generation != work.generation
    }

    fn needs_enrichment(&self) -> bool {
        self.enrich
            && (self.checker_refresh_required
                || self.force_full_enrichment
                || !self.checker_dirty_source_paths.is_empty())
    }

    fn is_clean(&self) -> bool {
        self.desired_generation == self.completed_generation
            && self.active.is_none()
            && self.ready.is_none()
            && self.retry.is_none()
    }
}

struct EventClassifier {
    root: PathBuf,
    excluded: BTreeSet<PathBuf>,
    git_controls: BTreeSet<PathBuf>,
    docs_freshness: bool,
    config_exact: BTreeSet<PathBuf>,
    external_exact: BTreeSet<PathBuf>,
    external_prefixes: BTreeSet<PathBuf>,
    source_policy: RefCell<walk::SourcePathPolicy>,
    documentation_policy: RefCell<docs::corpus::DocumentationPathPolicy>,
    documentation_options: docs::corpus::CorpusOptions,
}

impl EventClassifier {
    #[cfg(test)]
    fn new(
        root: &Path,
        database: &Path,
        documentation: &docs::corpus::CorpusOptions,
    ) -> Result<Self> {
        Self::new_with_docs_freshness(root, database, documentation, true)
    }

    fn new_with_docs_freshness(
        root: &Path,
        database: &Path,
        documentation: &docs::corpus::CorpusOptions,
        docs_freshness: bool,
    ) -> Result<Self> {
        let mut excluded = BTreeSet::new();
        excluded.insert(database.to_path_buf());
        for suffix in ["-wal", "-shm", "-journal"] {
            excluded.insert(PathBuf::from(format!("{}{suffix}", database.display())));
        }
        Ok(Self {
            root: root.to_path_buf(),
            excluded,
            git_controls: active_git_control_paths(root, docs_freshness),
            docs_freshness,
            config_exact: BTreeSet::new(),
            external_exact: BTreeSet::new(),
            external_prefixes: BTreeSet::new(),
            source_policy: RefCell::new(walk::SourcePathPolicy::new(root)),
            documentation_policy: RefCell::new(docs::corpus::DocumentationPathPolicy::new(
                root,
                documentation,
            )?),
            documentation_options: documentation.clone(),
        })
    }

    fn reload_path_policies(&mut self) -> Result<()> {
        *self.source_policy.get_mut() = walk::SourcePathPolicy::new(&self.root);
        self.documentation_policy.get_mut().reload_ignore()
    }

    fn reload_indexing_policy(
        &mut self,
        documentation: &docs::corpus::CorpusOptions,
        docs_freshness: bool,
    ) -> Result<()> {
        if self.documentation_options == *documentation {
            self.reload_path_policies()?;
        } else {
            *self.source_policy.get_mut() = walk::SourcePathPolicy::new(&self.root);
            *self.documentation_policy.get_mut() =
                docs::corpus::DocumentationPathPolicy::new(&self.root, documentation)?;
            self.documentation_options = documentation.clone();
        }
        self.git_controls = active_git_control_paths(&self.root, docs_freshness);
        self.docs_freshness = docs_freshness;
        Ok(())
    }

    fn set_external(
        &mut self,
        config_exact: BTreeSet<PathBuf>,
        exact: BTreeSet<PathBuf>,
        prefixes: BTreeSet<PathBuf>,
    ) {
        self.config_exact = config_exact;
        self.external_exact = exact;
        self.external_prefixes = prefixes;
    }

    fn classify(&self, paths: &[PathBuf]) -> Option<DirtySignal> {
        if paths.is_empty() {
            return Some(DirtySignal::full("unknown-event"));
        }
        let mut signal = None;
        for path in paths {
            let path = self.absolute(path);
            if self.excluded.contains(&path) {
                continue;
            }
            if self.config_exact.contains(&path) {
                merge_signal(
                    &mut signal,
                    DirtySignal::inventory(format!("config:{}", display_path(&self.root, &path))),
                );
                continue;
            }
            if self.git_controls.contains(&path) {
                merge_signal(
                    &mut signal,
                    DirtySignal::full(format!("git:{}", display_path(&self.root, &path))),
                );
                continue;
            }
            if self.external_exact.contains(&path)
                || self
                    .external_prefixes
                    .iter()
                    .any(|prefix| path.starts_with(prefix))
            {
                merge_signal(
                    &mut signal,
                    DirtySignal::full(format!("external:{}", path.display())),
                );
                continue;
            }
            // Selected external roots were handled above. For repository
            // paths, use the inventory walker's directory policy before
            // boundary detection so package.json under node_modules/dist does
            // not promote ordinary dependency noise to a full refresh.
            if walk::is_in_skipped_directory(&self.root, &path) {
                continue;
            }
            if is_active_refresh_boundary(&path, self.docs_freshness) {
                merge_signal(
                    &mut signal,
                    DirtySignal::full(format!("boundary:{}", display_path(&self.root, &path))),
                );
                continue;
            }
            let is_directory = path.is_dir();
            let is_file = path.is_file();
            if self
                .documentation_policy
                .borrow_mut()
                .is_admitted(&path, is_directory)
            {
                let relative = display_path(&self.root, &path);
                merge_signal(
                    &mut signal,
                    DirtySignal::documentation(format!("documentation:{relative}")),
                );
                continue;
            }
            let source_ignored = self
                .source_policy
                .borrow_mut()
                .is_ignored(&path, is_directory);
            if source_ignored {
                // A missing path may have been a directory. Re-query it with
                // directory semantics before suppressing the event: a
                // directory-only whitelist can reopen a hidden source tree,
                // while querying the vanished path as a file cannot observe
                // that whitelist.
                if !is_file && !self.source_policy.borrow_mut().is_ignored(&path, true) {
                    merge_signal(
                        &mut signal,
                        DirtySignal::inventory("inventory:directory-event"),
                    );
                    continue;
                }
                if (is_directory || !is_file)
                    && self
                        .documentation_policy
                        .borrow_mut()
                        .may_contain_document(&path)
                {
                    let relative = display_path(&self.root, &path);
                    merge_signal(
                        &mut signal,
                        DirtySignal::documentation(format!("documentation-directory:{relative}")),
                    );
                }
                continue;
            }
            if path.file_name().is_some_and(|name| name == "Cargo.toml") {
                merge_signal(
                    &mut signal,
                    DirtySignal::rust_membership(format!(
                        "rust-membership:{}",
                        display_path(&self.root, &path)
                    )),
                );
                continue;
            }
            if is_directory {
                merge_signal(
                    &mut signal,
                    DirtySignal::inventory("inventory:directory-event"),
                );
                continue;
            }
            if let Some(format) = formats::repository_code_for_path(&path) {
                if !self
                    .source_policy
                    .borrow_mut()
                    .directory_admitted(&path, format)
                {
                    continue;
                }
                let relative = display_path(&self.root, &path);
                merge_signal(
                    &mut signal,
                    DirtySignal::source(
                        format!("source:{relative}"),
                        relative,
                        format.checker_watch_affinity(),
                    ),
                );
                continue;
            }
            // Existing regular files outside both admitted corpora are
            // ordinary repository noise (Finder metadata and similar).
            // Missing paths and directories remain conservative because a
            // backend may be reporting a delete, rename, or rescan without
            // enough type information to classify it safely.
            if !is_file {
                merge_signal(
                    &mut signal,
                    DirtySignal::inventory("inventory:unknown-event"),
                );
            }
        }
        signal
    }

    fn absolute(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
    }
}

fn merge_signal(target: &mut Option<DirtySignal>, signal: DirtySignal) {
    match target {
        Some(target) => target.merge(signal),
        None => *target = Some(signal),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetKind {
    Exact,
    Prefix,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TargetSource {
    Git,
    Config,
    Dependency,
    Checker,
}

#[derive(Clone, Debug)]
struct WatchTarget {
    watch_path: PathBuf,
    path: PathBuf,
    mode: RecursiveMode,
    kind: TargetKind,
    source: TargetSource,
}

#[derive(Default)]
struct WatchRegistry {
    active: BTreeMap<PathBuf, RecursiveMode>,
    failures: BTreeMap<PathBuf, u8>,
}

impl WatchRegistry {
    fn reconcile<W: Watcher>(&mut self, watcher: &mut W, root: &Path, targets: &[WatchTarget]) {
        let mut desired = BTreeMap::new();
        for target in targets
            .iter()
            .filter(|target| !target.watch_path.starts_with(root))
        {
            desired
                .entry(target.watch_path.clone())
                .and_modify(|mode| {
                    if target.mode == RecursiveMode::Recursive {
                        *mode = RecursiveMode::Recursive;
                    }
                })
                .or_insert(target.mode);
        }
        self.failures.retain(|path, _| desired.contains_key(path));
        let removed = self
            .active
            .keys()
            .filter(|path| !desired.contains_key(*path))
            .cloned()
            .collect::<Vec<_>>();
        for path in removed {
            if let Err(error) = watcher.unwatch(&path) {
                eprintln!(
                    "watch coverage path={} status=unwatch-failed error={error}",
                    path.display()
                );
            }
            self.active.remove(&path);
            self.failures.remove(&path);
        }
        for (path, mode) in desired {
            if self.active.get(&path) == Some(&mode) {
                continue;
            }
            if self.active.contains_key(&path) {
                let _ = watcher.unwatch(&path);
                self.active.remove(&path);
            }
            match watcher.watch(&path, mode) {
                Ok(()) => {
                    self.active.insert(path.clone(), mode);
                    self.failures.remove(&path);
                }
                Err(error) => {
                    let failures = self.failures.entry(path.clone()).or_default();
                    *failures = failures.saturating_add(1);
                    eprintln!(
                        "watch coverage path={} status=degraded attempt={} recovery=target-reconciliation error={error}",
                        path.display(),
                        failures
                    );
                }
            }
        }
    }
}

struct RefreshResult {
    snapshot: String,
    outcome: indexer::IndexOutcome,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RejectionReportDecision {
    Silent,
    Details,
    Cleared { previous: usize },
}

#[derive(Default)]
struct RejectionReportLatch {
    // This is diagnostic state only. The refresh summary remains authoritative
    // and continues to report the rejection count for every generation.
    previous: Vec<indexer::IndexRejection>,
}

impl RejectionReportLatch {
    fn observe(&mut self, rejections: &[indexer::IndexRejection]) -> RejectionReportDecision {
        let mut current = rejections.to_vec();
        current.sort_by(|left, right| {
            left.path
                .cmp(&right.path)
                .then_with(|| left.stage.cmp(right.stage))
                .then_with(|| left.error.cmp(&right.error))
        });
        if current == self.previous {
            return RejectionReportDecision::Silent;
        }

        let previous = self.previous.len();
        let decision = if current.is_empty() {
            RejectionReportDecision::Cleared { previous }
        } else {
            RejectionReportDecision::Details
        };
        self.previous = current;
        decision
    }
}

fn prepare_refresh(
    root: &Path,
    options: &WatchOptions<'_>,
    current: &DocsIndexingPolicy,
    classifier: &mut EventClassifier,
    requested_scope: RefreshScope,
) -> Result<(DocsIndexingPolicy, RefreshScope)> {
    // Reload before opening the phase database. Invalid or temporarily partial
    // configuration therefore enters the ordinary refresh retry loop without
    // publishing a snapshot under stale indexing policy.
    let next = DocsIndexingPolicy::load(root, options)?;
    let changed = next != *current;
    classifier.reload_indexing_policy(&next.corpus_options(), next.freshness)?;
    Ok((
        next,
        if changed {
            RefreshScope::Full
        } else {
            requested_scope
        },
    ))
}

pub fn watch(root: &Path, options: &WatchOptions<'_>) -> Result<()> {
    validate_options(options)?;
    let root = root.canonicalize()?;
    let database = absolute_database_path(&root, options.database);
    let binary_fingerprint = crate::runtime_identity::current_binary_fingerprint();
    let checker_policy_fingerprint = checker::watch_policy_fingerprint(&watch_enrich_options(
        &database,
        options.checker_sidecar,
        options.checker_node,
        options.enrich_timeout,
        false,
        Vec::new(),
    ));
    let watch_policy_fingerprint = effective_watch_policy_fingerprint(options);
    let provider = if options.embed_on_change {
        Some(Arc::new(
            options
                .provider
                .context("watch embedding requires embedding.provider in .jscout.toml")?
                .clone(),
        ))
    } else {
        None
    };

    let (sender, receiver) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })?;
    // Subscribe before startup refresh so changes during a long first pass are
    // queued and force a later generation.
    watcher.watch(&root, RecursiveMode::Recursive)?;

    let started = Instant::now();
    let mut coordinator = Coordinator::new(
        options.debounce,
        options.embed_on_change,
        options.enrich_on_change,
    );
    let mut rejection_report_latch = RejectionReportLatch::default();
    let mut docs_policy = DocsIndexingPolicy::from_options(options);
    let documentation = docs_policy.corpus_options();
    let mut classifier = EventClassifier::new_with_docs_freshness(
        &root,
        &database,
        &documentation,
        docs_policy.freshness,
    )?;
    let mut registry = WatchRegistry::default();
    let mut targets = collect_watch_targets(&root, &database, docs_policy.freshness)
        .unwrap_or_else(|_| active_git_watch_targets(&root, docs_policy.freshness));
    extend_configured_watch_targets(&mut targets, &root, options);
    normalize_targets(&mut targets);
    update_classifier_targets(&mut classifier, &targets);
    registry.reconcile(&mut watcher, &root, &targets);
    // Reconciliation is anchored after a complete generation, not process
    // start or timer fire. Long generations therefore cannot create a
    // back-to-back refresh loop.
    let mut next_reconcile = None;
    // Checker carry-forward intentionally ignores some ambient type drift.
    // Bound that window independently of the frequent structural reconcile.
    let mut next_checker_flush = options
        .enrich_on_change
        .then_some(CHECKER_DRIFT_FLUSH_INTERVAL);
    let mut checker_flush_pending = false;

    eprintln!(
        "{}",
        watch_startup_log(
            &root,
            &database,
            options,
            &binary_fingerprint,
            &checker_policy_fingerprint,
            &watch_policy_fingerprint,
        )
    );
    if options.reconcile_interval.is_zero() {
        eprintln!(
            "warning: periodic reconciliation is disabled; missed notifications and degraded external coverage may remain stale until another event"
        );
    }

    loop {
        let now = started.elapsed();
        if next_checker_flush.is_some_and(|deadline| now >= deadline) && coordinator.is_clean() {
            coordinator.mark_checker_drift_flush(now);
            next_checker_flush = None;
            checker_flush_pending = true;
        }
        if next_reconcile.is_some_and(|deadline| now >= deadline) && coordinator.is_clean() {
            coordinator.mark_reconciliation(now);
            next_reconcile = None;
        }
        drain_events(&receiver, &classifier, &mut coordinator, started.elapsed());
        // Any ordinary generation supersedes the previous clean-generation
        // reconciliation deadline. A new deadline is anchored only when this
        // generation completes; retaining an overdue deadline would poll at
        // the one-millisecond floor throughout retry wait.
        clear_reconciliation_deadline_if_dirty(&coordinator, &mut next_reconcile);

        if let Some(work) = coordinator.next_work(started.elapsed()) {
            let phase_started = Instant::now();
            if work.phase == Phase::Refresh {
                // A lexical config symlink may have been repointed by the
                // event that scheduled this generation. Subscribe its current
                // resolved target before loading policy or indexing, while
                // retaining the prior target until successful reconciliation.
                extend_config_watch_targets(&mut targets, options);
                normalize_targets(&mut targets);
                update_classifier_targets(&mut classifier, &targets);
                registry.reconcile(&mut watcher, &root, &targets);
            }
            let mut refresh_preflight = (work.phase == Phase::Refresh).then(|| {
                prepare_refresh(
                    &root,
                    options,
                    &docs_policy,
                    &mut classifier,
                    work.refresh_scope,
                )
            });
            let displayed_refresh_scope = refresh_preflight
                .as_ref()
                .and_then(|result| result.as_ref().ok())
                .map_or(work.refresh_scope, |(_, scope)| *scope);
            eprintln!(
                "watch generation={} phase={} refresh_scope={} status=started reasons={}",
                work.generation,
                work.phase,
                displayed_refresh_scope,
                coordinator
                    .dirty_reasons
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            );
            match work.phase {
                Phase::Refresh => {
                    let (refresh_scope, candidate_policy, refresh_result) = match refresh_preflight
                        .take()
                        .expect("refresh preflight exists for the refresh phase")
                    {
                        Ok((next_policy, refresh_scope)) => {
                            if !docs_policy.freshness && next_policy.freshness {
                                // Subscribe the provenance controls before the
                                // first enabled refresh starts. In particular,
                                // linked-worktree/common-dir controls can live
                                // outside the recursive repository root. A Git
                                // change during this refresh must queue a
                                // successor generation instead of landing in
                                // the registration gap after publication.
                                targets.extend(active_git_watch_targets(&root, true));
                                normalize_targets(&mut targets);
                                update_classifier_targets(&mut classifier, &targets);
                                registry.reconcile(&mut watcher, &root, &targets);
                            }
                            if next_policy != docs_policy {
                                eprintln!(
                                    "watch config_reload=docs-indexing status=loaded freshness={} include={} exclude={}",
                                    next_policy.freshness,
                                    next_policy.include.len(),
                                    next_policy.exclude.len(),
                                );
                            }
                            let index_options = next_policy.index_options(options);
                            (
                                refresh_scope,
                                Some(next_policy),
                                run_refresh(
                                    &root,
                                    &database,
                                    &index_options,
                                    refresh_scope,
                                    work.rebind_checker,
                                ),
                            )
                        }
                        Err(error) => (work.refresh_scope, None, Err(error)),
                    };
                    match refresh_result {
                        Ok(result) => {
                            docs_policy = candidate_policy
                                .expect("successful refresh has a loaded documentation policy");
                            match rejection_report_latch.observe(&result.outcome.rejections) {
                                RejectionReportDecision::Silent => {}
                                RejectionReportDecision::Details => {
                                    indexer::report_rejections(&result.outcome);
                                }
                                RejectionReportDecision::Cleared { previous } => {
                                    eprintln!(
                                        "watch index input rejections cleared (previous={previous})"
                                    );
                                }
                            }
                            eprintln!(
                                "watch generation={} phase=refresh refresh_scope={} status=succeeded snapshot={} indexed={} unchanged={} removed={} rejected={} extracted_chunks={} extracted_refs={} rust_parse_error_files={} rust_parse_errors={} projection={} elapsed_ms={}",
                                work.generation,
                                refresh_scope,
                                result.snapshot,
                                result.outcome.indexed,
                                result.outcome.unchanged,
                                result.outcome.removed,
                                result.outcome.rejected,
                                result.outcome.chunks,
                                result.outcome.refs,
                                result.outcome.rust_files_with_parse_errors,
                                result.outcome.rust_parse_error_count,
                                if result.outcome.projection_rebuilt {
                                    "rebuilt"
                                } else {
                                    "reused"
                                },
                                phase_started.elapsed().as_millis()
                            );
                            let previous_targets = targets.clone();
                            targets =
                                collect_watch_targets(&root, &database, docs_policy.freshness)
                                    .unwrap_or_else(|error| {
                                        eprintln!(
                                            "watch coverage status=read-failed error={error:#}"
                                        );
                                        active_git_watch_targets(&root, docs_policy.freshness)
                                    });
                            if options.enrich_on_change {
                                targets.extend(
                                    previous_targets
                                        .into_iter()
                                        .filter(|target| target.source == TargetSource::Checker),
                                );
                                normalize_targets(&mut targets);
                            }
                            extend_configured_watch_targets(&mut targets, &root, options);
                            normalize_targets(&mut targets);
                            update_classifier_targets(&mut classifier, &targets);
                            registry.reconcile(&mut watcher, &root, &targets);
                            drain_events(
                                &receiver,
                                &classifier,
                                &mut coordinator,
                                started.elapsed(),
                            );
                            report_finish(
                                work,
                                coordinator.finish_refresh(work),
                                started.elapsed(),
                                options.reconcile_interval,
                                &mut next_reconcile,
                            );
                        }
                        Err(error) => {
                            eprintln!(
                                "watch generation={} phase=refresh status=failed elapsed_ms={} error={error:#}",
                                work.generation,
                                phase_started.elapsed().as_millis()
                            );
                            report_finish(
                                work,
                                coordinator.finish_error(started.elapsed(), work),
                                started.elapsed(),
                                options.reconcile_interval,
                                &mut next_reconcile,
                            );
                        }
                    }
                }
                Phase::Embed => {
                    let provider = Arc::clone(provider.as_ref().expect("provider validated"));
                    let result = {
                        let mut monitor = PhaseMonitor {
                            receiver: &receiver,
                            classifier: &classifier,
                            coordinator: &mut coordinator,
                            work,
                            started,
                        };
                        run_embedding_interruptible(
                            &root,
                            &database,
                            provider,
                            options.embed_product_only,
                            &mut monitor,
                        )
                    };
                    match result {
                        Ok(report) => {
                            eprintln!(
                                "watch generation={} phase=embed status={} missing={} embedded={} cached_reused={} occurrences_synced={} elapsed_ms={}",
                                work.generation,
                                if report.canceled {
                                    "canceled"
                                } else {
                                    "succeeded"
                                },
                                report.missing,
                                report.embedded,
                                report.cached_reused,
                                report.occurrences_synced,
                                phase_started.elapsed().as_millis()
                            );
                            debug_assert!(!report.canceled || coordinator.is_superseded(work));
                            let state = coordinator.finish_optional(work);
                            report_finish(
                                work,
                                state,
                                started.elapsed(),
                                options.reconcile_interval,
                                &mut next_reconcile,
                            );
                        }
                        Err(error) => {
                            eprintln!(
                                "watch generation={} phase=embed status=failed elapsed_ms={} error={error:#}",
                                work.generation,
                                phase_started.elapsed().as_millis()
                            );
                            report_finish(
                                work,
                                coordinator.finish_error(started.elapsed(), work),
                                started.elapsed(),
                                options.reconcile_interval,
                                &mut next_reconcile,
                            );
                        }
                    }
                }
                Phase::Enrich => {
                    let result = {
                        let mut monitor = PhaseMonitor {
                            receiver: &receiver,
                            classifier: &classifier,
                            coordinator: &mut coordinator,
                            work,
                            started,
                        };
                        run_enrichment_interruptible(&root, &database, options, &mut monitor)
                    };
                    match result {
                        Ok(report) => {
                            eprintln!(
                                "watch generation={} phase=enrich status=succeeded snapshot={} facts={} occurrences={} occurrences_queried={} projects={} exact_batch_reused={} projects_resumed={} occurrences_resumed={} projects_reset={} occurrences_reset={} projects_carried={} projects_carried_from_staging={} projects_carried_from_active={} projects_partially_carried={} occurrences_carried={} project_occurrences_carried={} checker_version={} checker_source={} files_without_configured_project={} occurrences_skipped_inferred_project={} elapsed_ms={}",
                                work.generation,
                                report.snapshot,
                                report.facts_published,
                                report.occurrences_queried,
                                report.occurrences_queried,
                                report.projects,
                                report.exact_batch_reused,
                                report.projects_resumed,
                                report.occurrences_resumed,
                                report.projects_reset,
                                report.occurrences_reset,
                                report.projects_carried,
                                report.projects_carried_from_staging,
                                report.projects_carried_from_active,
                                report.projects_partially_carried,
                                report.occurrences_carried,
                                report.project_occurrences_carried,
                                report.checker_version,
                                report.checker_source,
                                report.files_without_configured_project,
                                report.occurrences_skipped_inferred_project,
                                phase_started.elapsed().as_millis()
                            );
                            targets =
                                collect_watch_targets(&root, &database, docs_policy.freshness)
                                    .unwrap_or_else(|error| {
                                        eprintln!(
                                            "watch coverage status=read-failed error={error:#}"
                                        );
                                        targets.clone()
                                    });
                            extend_configured_watch_targets(&mut targets, &root, options);
                            normalize_targets(&mut targets);
                            update_classifier_targets(&mut classifier, &targets);
                            registry.reconcile(&mut watcher, &root, &targets);
                            report_finish(
                                work,
                                coordinator.finish_enrichment_success(work),
                                started.elapsed(),
                                options.reconcile_interval,
                                &mut next_reconcile,
                            );
                        }
                        Err(error) => {
                            let interrupted = checker::process::interrupt_pending();
                            let superseded = coordinator.is_superseded(work);
                            let terminal_partial = checker::is_terminal_partial_failure(&error);
                            eprintln!(
                                "watch generation={} phase=enrich status={} elapsed_ms={} error={error:#}",
                                work.generation,
                                if interrupted {
                                    "interrupted"
                                } else if superseded {
                                    "canceled"
                                } else if terminal_partial {
                                    "partial"
                                } else {
                                    "failed"
                                },
                                phase_started.elapsed().as_millis()
                            );
                            if interrupted {
                                eprintln!("watch status=stopped reason=interrupt");
                                return Ok(());
                            }
                            let state = if terminal_partial {
                                coordinator.finish_optional_partial(work)
                            } else {
                                coordinator.finish_error(started.elapsed(), work)
                            };
                            report_finish(
                                work,
                                state,
                                started.elapsed(),
                                options.reconcile_interval,
                                &mut next_reconcile,
                            );
                        }
                    }
                }
                Phase::SemanticEmbed => {
                    let provider = Arc::clone(provider.as_ref().expect("provider validated"));
                    let result = {
                        let mut monitor = PhaseMonitor {
                            receiver: &receiver,
                            classifier: &classifier,
                            coordinator: &mut coordinator,
                            work,
                            started,
                        };
                        run_semantic_embedding_interruptible(
                            &root,
                            &database,
                            provider,
                            &mut monitor,
                        )
                    };
                    match result {
                        Ok(report) => {
                            eprintln!(
                                "watch generation={} phase=semantic-embed status={} missing={} embedded={} cached_reused={} occurrences_synced={} elapsed_ms={}",
                                work.generation,
                                if report.canceled {
                                    "canceled"
                                } else {
                                    "succeeded"
                                },
                                report.missing,
                                report.embedded,
                                report.cached_reused,
                                report.occurrences_synced,
                                phase_started.elapsed().as_millis()
                            );
                            debug_assert!(!report.canceled || coordinator.is_superseded(work));
                            report_finish(
                                work,
                                coordinator.finish_optional(work),
                                started.elapsed(),
                                options.reconcile_interval,
                                &mut next_reconcile,
                            );
                        }
                        Err(error) => {
                            eprintln!(
                                "watch generation={} phase=semantic-embed status=failed elapsed_ms={} error={error:#}",
                                work.generation,
                                phase_started.elapsed().as_millis()
                            );
                            report_finish(
                                work,
                                coordinator.finish_error(started.elapsed(), work),
                                started.elapsed(),
                                options.reconcile_interval,
                                &mut next_reconcile,
                            );
                        }
                    }
                }
            }
            if checker_flush_pending && coordinator.is_clean() {
                checker_flush_pending = false;
                next_checker_flush = Some(
                    started
                        .elapsed()
                        .saturating_add(CHECKER_DRIFT_FLUSH_INTERVAL),
                );
            }
            continue;
        }

        let now = started.elapsed();
        let phase_deadline = coordinator.next_deadline();
        let checker_deadline = coordinator
            .is_clean()
            .then_some(next_checker_flush)
            .flatten();
        let deadline = [phase_deadline, next_reconcile, checker_deadline]
            .into_iter()
            .flatten()
            .min();
        let received = match deadline {
            Some(deadline) => {
                receiver.recv_timeout(deadline.saturating_sub(now).max(Duration::from_millis(1)))
            }
            None => match receiver.recv() {
                Ok(event) => Ok(event),
                Err(_) => return Ok(()),
            },
        };
        match received {
            Ok(event) => ingest_event(event, &classifier, &mut coordinator, started.elapsed()),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

fn watch_startup_log(
    root: &Path,
    database: &Path,
    options: &WatchOptions<'_>,
    binary_fingerprint: &str,
    checker_policy_fingerprint: &str,
    watch_policy_fingerprint: &str,
) -> String {
    format!(
        "watch root={} database={} jscout_version={} binary_fingerprint={} config_fingerprint={} config_loaded={} config_reload=docs-indexing-only checker_policy_fingerprint={} watch_policy_fingerprint={} debounce_ms={} reconcile_seconds={} embed={} product={} enrich={} docs_freshness={}",
        root.display(),
        database.display(),
        env!("CARGO_PKG_VERSION"),
        binary_fingerprint,
        options.config_fingerprint,
        options.config_loaded,
        checker_policy_fingerprint,
        watch_policy_fingerprint,
        options.debounce.as_millis(),
        options.reconcile_interval.as_secs(),
        options.embed_on_change,
        options.embed_product_only,
        options.enrich_on_change,
        options.docs_freshness,
    )
}

/// Hash the effective, non-secret watch invocation after config defaults and
/// CLI overrides have been resolved. The baseline config fingerprint remains
/// separately visible; this identity closes the gap where two invocations
/// with different dependency selectors, timeout, or sidecar override would
/// otherwise produce identical startup identities.
fn effective_watch_policy_fingerprint(options: &WatchOptions<'_>) -> String {
    fn field(hasher: &mut blake3::Hasher, name: &str, value: &str) {
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&(value.len() as u64).to_le_bytes());
        hasher.update(value.as_bytes());
    }
    fn list(hasher: &mut blake3::Hasher, name: &str, values: &[String]) {
        let mut values = values.to_vec();
        values.sort();
        values.dedup();
        field(hasher, name, &values.join("\0"));
    }

    let mut hasher = blake3::Hasher::new();
    field(&mut hasher, "domain", "jscout-watch-effective-policy-v1");
    field(&mut hasher, "config", options.config_fingerprint);
    field(&mut hasher, "embed", &options.embed_on_change.to_string());
    field(
        &mut hasher,
        "product",
        &options.embed_product_only.to_string(),
    );
    if let Some(provider) = options.provider {
        field(&mut hasher, "embed_provider", &provider.name);
        field(&mut hasher, "embed_model", &provider.model);
    }
    list(&mut hasher, "dependencies", options.dependencies);
    list(&mut hasher, "docs_include", options.docs_include);
    list(&mut hasher, "docs_exclude", options.docs_exclude);
    field(
        &mut hasher,
        "docs_freshness",
        &options.docs_freshness.to_string(),
    );
    field(&mut hasher, "enrich", &options.enrich_on_change.to_string());
    field(
        &mut hasher,
        "enrich_timeout_ms",
        &options.enrich_timeout.as_millis().to_string(),
    );
    field(
        &mut hasher,
        "checker_sidecar",
        &options
            .checker_sidecar
            .map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
    );
    field(&mut hasher, "checker_node", options.checker_node);
    field(&mut hasher, "timing", &options.timing.to_string());
    field(&mut hasher, "debug", &options.debug.to_string());
    field(
        &mut hasher,
        "debounce_ms",
        &options.debounce.as_millis().to_string(),
    );
    field(
        &mut hasher,
        "reconcile_ms",
        &options.reconcile_interval.as_millis().to_string(),
    );
    hasher.finalize().to_hex().to_string()
}

fn validate_options(options: &WatchOptions<'_>) -> Result<()> {
    if options.config_explicit && options.config_path.is_none() {
        bail!("explicit watch configuration path is missing");
    }
    if options.embed_product_only && !options.embed_on_change {
        bail!("--product requires --embed");
    }
    if options.enrich_on_change && options.enrich_timeout.is_zero() {
        bail!("--enrich-timeout must be greater than zero seconds");
    }
    if options.debounce.is_zero() {
        bail!("--debounce-ms must be greater than zero");
    }
    if !options.reconcile_interval.is_zero() && options.reconcile_interval <= options.debounce {
        bail!("--reconcile-seconds must exceed --debounce-ms or be zero");
    }
    Ok(())
}

fn run_refresh(
    root: &Path,
    database: &Path,
    options: &indexer::IndexOptions,
    scope: RefreshScope,
    rebind_checker: bool,
) -> Result<RefreshResult> {
    let conn = open_phase_database(root, database)?;
    let outcome = match scope {
        RefreshScope::Incremental => {
            if rebind_checker {
                indexer::incremental_refresh_repo_rebinding_checker(root, &conn, options)?
            } else {
                indexer::incremental_refresh_repo_with_options(root, &conn, options)?
            }
        }
        RefreshScope::Full if rebind_checker => {
            indexer::watch_full_refresh_repo_rebinding_checker(root, &conn, options)?
        }
        RefreshScope::Full => indexer::watch_full_refresh_repo_with_options(root, &conn, options)?,
    };
    let snapshot = structural::current_snapshot(&conn)?;
    Ok(RefreshResult { snapshot, outcome })
}

fn run_embedding_interruptible(
    root: &Path,
    database: &Path,
    provider: Arc<embed::Provider>,
    product_only: bool,
    monitor: &mut PhaseMonitor<'_>,
) -> Result<embed::EmbeddingPassReport> {
    let root = root.to_path_buf();
    let database = database.to_path_buf();
    let canceled = Arc::new(AtomicBool::new(false));
    let worker_canceled = Arc::clone(&canceled);
    let worker = thread::spawn(move || -> Result<embed::EmbeddingPassReport> {
        let conn = open_phase_database(&root, &database)?;
        embed::embed_missing_interruptible(&conn, &provider, 64, product_only, || {
            worker_canceled.load(Ordering::SeqCst)
        })
    });
    while !worker.is_finished() {
        monitor.poll();
        if monitor.is_superseded() {
            canceled.store(true, Ordering::SeqCst);
        }
        thread::sleep(OPTIONAL_PHASE_POLL);
    }
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("embedding worker panicked"))?
}

fn run_semantic_embedding_interruptible(
    root: &Path,
    database: &Path,
    provider: Arc<embed::Provider>,
    monitor: &mut PhaseMonitor<'_>,
) -> Result<embed::EmbeddingPassReport> {
    let root = root.to_path_buf();
    let database = database.to_path_buf();
    let canceled = Arc::new(AtomicBool::new(false));
    let worker_canceled = Arc::clone(&canceled);
    let worker = thread::spawn(move || -> Result<embed::EmbeddingPassReport> {
        let conn = open_phase_database(&root, &database)?;
        embed::embed_semantic_missing_interruptible(&conn, &provider, 64, || {
            worker_canceled.load(Ordering::SeqCst)
        })
    });
    while !worker.is_finished() {
        monitor.poll();
        if monitor.is_superseded() {
            canceled.store(true, Ordering::SeqCst);
        }
        thread::sleep(OPTIONAL_PHASE_POLL);
    }
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("semantic embedding worker panicked"))?
}

fn run_enrichment_interruptible(
    root: &Path,
    database: &Path,
    options: &WatchOptions<'_>,
    monitor: &mut PhaseMonitor<'_>,
) -> Result<checker::EnrichReport> {
    let root = root.to_path_buf();
    let database = database.to_path_buf();
    let sidecar = options.checker_sidecar.map(Path::to_path_buf);
    let node = options.checker_node.to_string();
    let timeout = options.enrich_timeout;
    let force_full = monitor.work.force_full_enrichment;
    let dirty_files = monitor
        .coordinator
        .checker_dirty_source_paths
        .iter()
        .cloned()
        .collect();
    let worker = thread::spawn(move || {
        let enrich_options = watch_enrich_options(
            &database,
            sidecar.as_deref(),
            &node,
            timeout,
            force_full,
            dirty_files,
        );
        checker::enrich(&root, &enrich_options)
    });
    while !worker.is_finished() {
        monitor.poll();
        if monitor.is_superseded() {
            let _ = checker::process::cancel_active_operation();
        }
        thread::sleep(OPTIONAL_PHASE_POLL);
    }
    worker
        .join()
        .map_err(|_| anyhow::anyhow!("checker enrichment worker panicked"))?
}

fn watch_enrich_options<'a>(
    database: &'a Path,
    sidecar: Option<&'a Path>,
    node: &'a str,
    timeout: Duration,
    force_full: bool,
    dirty_files: Vec<String>,
) -> checker::EnrichOptions<'a> {
    checker::EnrichOptions {
        database: Some(database),
        sidecar,
        node,
        timeout,
        files: Vec::new(),
        packages: Vec::new(),
        members: Vec::new(),
        roles: Vec::new(),
        max_occurrences: None,
        include_all: false,
        dry_run: false,
        carry_forward: !force_full,
        force_full,
        dirty_files,
    }
}

struct PhaseMonitor<'a> {
    receiver: &'a mpsc::Receiver<notify::Result<notify::Event>>,
    classifier: &'a EventClassifier,
    coordinator: &'a mut Coordinator,
    work: Work,
    started: Instant,
}

impl PhaseMonitor<'_> {
    fn poll(&mut self) {
        match self.receiver.recv_timeout(OPTIONAL_PHASE_POLL) {
            Ok(event) => ingest_event(
                event,
                self.classifier,
                self.coordinator,
                self.started.elapsed(),
            ),
            Err(mpsc::RecvTimeoutError::Timeout | mpsc::RecvTimeoutError::Disconnected) => {}
        }
        drain_events(
            self.receiver,
            self.classifier,
            self.coordinator,
            self.started.elapsed(),
        );
    }

    fn is_superseded(&self) -> bool {
        self.coordinator.is_superseded(self.work)
    }
}

fn ingest_event(
    event: notify::Result<notify::Event>,
    classifier: &EventClassifier,
    coordinator: &mut Coordinator,
    now: Duration,
) {
    let signal = match event {
        Ok(event) => classifier.classify(&event.paths),
        Err(error) => Some(DirtySignal::full(format!("watch-error:{error}"))),
    };
    if let Some(signal) = signal {
        coordinator.mark_dirty(now, signal);
    }
}

fn drain_events(
    receiver: &mpsc::Receiver<notify::Result<notify::Event>>,
    classifier: &EventClassifier,
    coordinator: &mut Coordinator,
    now: Duration,
) {
    while let Ok(event) = receiver.try_recv() {
        ingest_event(event, classifier, coordinator, now);
    }
}

fn report_finish(
    work: Work,
    state: FinishState,
    now: Duration,
    reconcile_interval: Duration,
    next_reconcile: &mut Option<Duration>,
) {
    match state {
        FinishState::Continue => {}
        FinishState::Complete => {
            eprintln!("watch generation={} status=clean", work.generation);
            *next_reconcile =
                (!reconcile_interval.is_zero()).then(|| now.saturating_add(reconcile_interval));
        }
        FinishState::Partial => {
            eprintln!("watch generation={} status=partial", work.generation);
            *next_reconcile =
                (!reconcile_interval.is_zero()).then(|| now.saturating_add(reconcile_interval));
        }
        FinishState::Retry { after } => eprintln!(
            "watch generation={} phase={} status=retry-wait retry_ms={}",
            work.generation,
            work.phase,
            after.as_millis()
        ),
        FinishState::Superseded => eprintln!(
            "watch generation={} phase={} status=superseded",
            work.generation, work.phase
        ),
    }
}

fn clear_reconciliation_deadline_if_dirty(
    coordinator: &Coordinator,
    next_reconcile: &mut Option<Duration>,
) {
    if !coordinator.is_clean() {
        *next_reconcile = None;
    }
}

fn open_phase_database(root: &Path, database: &Path) -> Result<Connection> {
    let conn = if database == store::db_path(root) {
        store::open(root)?
    } else {
        store::open_path(database)?
    };
    conn.busy_timeout(DEFAULT_BUSY_TIMEOUT)?;
    Ok(conn)
}

fn absolute_database_path(root: &Path, selected: Option<&Path>) -> PathBuf {
    let path = match selected {
        Some(path) if path.is_absolute() => path.to_path_buf(),
        Some(path) => root.join(path),
        None => store::db_path(root),
    };
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map(|parent| parent.join(name))
            .unwrap_or(path),
        _ => path,
    }
}

/// Apply runtime policy to the complete boundary vocabulary. Git attribute
/// conversion affects documentation provenance only, so an opted-out watcher
/// ignores `.gitattributes` while retaining every structural boundary.
fn is_active_refresh_boundary(path: &Path, docs_freshness: bool) -> bool {
    is_refresh_boundary(path)
        && (docs_freshness
            || !path
                .file_name()
                .is_some_and(|name| name == ".gitattributes"))
}

/// Paths whose changes can alter source discovery, package ownership, module
/// resolution, dependency selection, checker ownership, or Git conversion.
fn is_refresh_boundary(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name,
        "package.json"
            | "pnpm-workspace.yaml"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lock"
            | "bun.lockb"
            | "tsconfig.json"
            | "jsconfig.json"
            | ".gitattributes"
            | ".gitignore"
            | ".ignore"
            | ".gitmodules"
    ) || (name.starts_with("tsconfig.") && name.ends_with(".json"))
        || (name.starts_with("jsconfig.") && name.ends_with(".json"))
        || name.ends_with(".d.ts")
        || name.ends_with(".d.mts")
        || name.ends_with(".d.cts")
}

fn git_output(root: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(output.stdout)
}

fn strip_git_record_terminator(mut output: Vec<u8>) -> Option<Vec<u8>> {
    if output.last() == Some(&b'\n') {
        output.pop();
        #[cfg(windows)]
        if output.last() == Some(&b'\r') {
            output.pop();
        }
    }
    (!output.is_empty()).then_some(output)
}

fn parse_git_ascii_scalar(output: Vec<u8>) -> Option<String> {
    let output = strip_git_record_terminator(output)?;
    if !output.is_ascii() || output.iter().any(|byte| matches!(byte, b'\r' | b'\n')) {
        return None;
    }
    String::from_utf8(output).ok()
}

fn git_ascii_scalar(root: &Path, arguments: &[&str]) -> Option<String> {
    parse_git_ascii_scalar(git_output(root, arguments)?)
}

#[cfg(unix)]
fn path_from_git_bytes(output: Vec<u8>) -> PathBuf {
    PathBuf::from(OsString::from_vec(output))
}

#[cfg(not(unix))]
fn path_from_git_bytes(output: Vec<u8>) -> Option<PathBuf> {
    String::from_utf8(output).ok().map(PathBuf::from)
}

fn git_path(root: &Path, arguments: &[&str]) -> Option<PathBuf> {
    let output = strip_git_record_terminator(git_output(root, arguments)?)?;
    #[cfg(unix)]
    let path = path_from_git_bytes(output);
    #[cfg(not(unix))]
    let path = path_from_git_bytes(output)?;
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    if let Ok(canonical) = path.canonicalize() {
        return Some(canonical);
    }
    Some(match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map(|parent| parent.join(name))
            .unwrap_or(path),
        _ => path,
    })
}

fn filesystem_git_directories(root: &Path) -> Option<(PathBuf, PathBuf)> {
    let dot_git = root
        .ancestors()
        .map(|ancestor| ancestor.join(".git"))
        .find(|candidate| candidate.is_dir() || candidate.is_file());
    let git_dir = dot_git.and_then(|dot_git| {
        if dot_git.is_dir() {
            return Some(dot_git);
        }
        fs::read_to_string(&dot_git).ok().and_then(|contents| {
            contents
                .trim()
                .strip_prefix("gitdir:")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .map(|path| {
                    if path.is_absolute() {
                        path
                    } else {
                        dot_git
                            .parent()
                            .map_or_else(|| root.join(&path), |parent| parent.join(&path))
                    }
                })
        })
    })?;
    let git_dir = git_dir.canonicalize().unwrap_or(git_dir);
    let common_dir = fs::read_to_string(git_dir.join("commondir"))
        .ok()
        .map(|contents| contents.trim().to_owned())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                git_dir.join(path)
            }
        })
        .map_or_else(
            || git_dir.clone(),
            |path| path.canonicalize().unwrap_or(path),
        );
    Some((git_dir, common_dir))
}

fn git_directories(root: &Path) -> Option<(PathBuf, PathBuf)> {
    match (
        git_path(root, &["rev-parse", "--absolute-git-dir"]),
        git_path(root, &["rev-parse", "--git-common-dir"]),
    ) {
        (Some(git_dir), Some(common_dir)) => Some((git_dir, common_dir)),
        _ => filesystem_git_directories(root),
    }
}

fn git_ancestor_attribute_paths(root: &Path) -> BTreeSet<PathBuf> {
    let Some(worktree_root) = git_path(root, &["rev-parse", "--show-toplevel"]) else {
        return BTreeSet::new();
    };
    let indexed_root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !indexed_root.starts_with(&worktree_root) {
        return BTreeSet::new();
    }
    let mut paths = BTreeSet::new();
    let mut directory = Some(indexed_root.as_path());
    while let Some(current) = directory {
        paths.insert(current.join(".gitattributes"));
        if current == worktree_root {
            break;
        }
        directory = current.parent();
    }
    paths
}

/// Git controls retained from the structural watcher contract. These remain
/// active when documentation provenance is disabled; source/package behavior
/// must not lose its existing checkout and submodule coverage.
fn source_git_control_paths(root: &Path) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::from([root.join(".gitmodules")]);
    // A linked worktree or absorbed submodule discovers its private Git
    // directory through the nearest file-form `.git`. If that indirection is
    // repaired or repointed, every resolved HEAD/control target can move, so
    // retain the gitfile itself as a structural control in either freshness
    // mode.
    if let Some(gitfile) = root
        .ancestors()
        .map(|ancestor| ancestor.join(".git"))
        .find(|candidate| candidate.is_file() || candidate.is_dir())
        .filter(|candidate| candidate.is_file())
    {
        paths.insert(gitfile);
    }
    // Submodule declarations apply from the repository worktree root even
    // when only a nested package is indexed. That file lies outside the
    // recursive nested-root subscription and therefore needs an exact target.
    if let Some(worktree_root) = git_path(root, &["rev-parse", "--show-toplevel"]) {
        paths.insert(worktree_root.join(".gitmodules"));
    }
    if let Some((git_dir, _)) = git_directories(root) {
        paths.insert(git_dir.join("HEAD"));
    }
    paths
}

/// Complete Git control plane used when documentation provenance is enabled.
fn git_control_paths(root: &Path) -> BTreeSet<PathBuf> {
    let mut paths = source_git_control_paths(root);
    // Git itself discovers a worktree by walking upward. Watch must use the
    // same ownership boundary when the indexed root is a repository subtree;
    // otherwise a commit can change blame state without touching bytes below
    // the watched root. Prefer Git's raw path resolution so non-UTF8 linked
    // worktree pointers remain usable; the filesystem parser is retained for
    // synthetic fixtures and older Git behavior.
    if let Some((git_dir, common_dir)) = git_directories(root) {
        paths.extend(git_ancestor_attribute_paths(root));
        let head = git_dir.join("HEAD");
        paths.extend([
            head.clone(),
            git_dir.join("logs/HEAD"),
            git_dir.join("shallow"),
            common_dir.join("packed-refs"),
            common_dir.join("shallow"),
        ]);
        // `--git-path` resolves the worktree-specific index and deliberately
        // honors GIT_INDEX_FILE, just like the provenance commands whose
        // result depends on index membership. Git may return a relative path,
        // which is relative to the indexed root selected by `-C`.
        paths.insert(
            git_path(root, &["rev-parse", "--git-path", "index"])
                .unwrap_or_else(|| git_dir.join("index")),
        );
        paths.insert(
            git_path(root, &["rev-parse", "--git-path", "info/attributes"])
                .unwrap_or_else(|| common_dir.join("info/attributes")),
        );
        paths.insert(
            git_path(root, &["rev-parse", "--git-path", "config"])
                .unwrap_or_else(|| common_dir.join("config")),
        );
        paths.insert(
            git_path(root, &["rev-parse", "--git-path", "config.worktree"])
                .unwrap_or_else(|| git_dir.join("config.worktree")),
        );
        // Reftable updates manifests atomically. Linked worktrees can update
        // either their private stack or the shared stack, so both manifests
        // are controls. Match the reported format exactly: older Git versions
        // may reject this query, in which case the file/packed-refs controls
        // above remain the conservative fallback for repositories they can
        // understand.
        if git_ascii_scalar(root, &["rev-parse", "--show-ref-format"]).as_deref()
            == Some("reftable")
        {
            paths.insert(git_dir.join("reftable/tables.list"));
            paths.insert(common_dir.join("reftable/tables.list"));
        }
        if let Some(reference) = fs::read_to_string(&head)
            .ok()
            .and_then(|contents| {
                contents
                    .trim()
                    .strip_prefix("ref:")
                    .map(str::trim)
                    .map(str::to_owned)
            })
            .filter(|reference| {
                let path = Path::new(reference);
                !path.is_absolute()
                    && path
                        .components()
                        .all(|component| matches!(component, std::path::Component::Normal(_)))
            })
        {
            paths.insert(common_dir.join(&reference));
            paths.insert(common_dir.join("logs").join(reference));
        }
    }
    paths
}

fn active_git_control_paths(root: &Path, docs_freshness: bool) -> BTreeSet<PathBuf> {
    if docs_freshness {
        git_control_paths(root)
    } else {
        source_git_control_paths(root)
    }
}

#[cfg(test)]
fn git_watch_targets(root: &Path) -> Vec<WatchTarget> {
    active_git_watch_targets(root, true)
}

fn active_git_watch_targets(root: &Path, docs_freshness: bool) -> Vec<WatchTarget> {
    active_git_control_paths(root, docs_freshness)
        .into_iter()
        .map(|path| exact_watch_target(path, TargetSource::Git))
        .collect()
}

fn extend_configured_watch_targets(
    targets: &mut Vec<WatchTarget>,
    root: &Path,
    options: &WatchOptions<'_>,
) {
    extend_config_watch_targets(targets, options);
    targets.extend(selector_watch_targets(root, options.dependencies));
}

fn extend_config_watch_targets(targets: &mut Vec<WatchTarget>, options: &WatchOptions<'_>) {
    if let Some(path) = options.config_path {
        for path in config_watch_paths(path) {
            targets.push(exact_watch_target(path, TargetSource::Config));
        }
    }
}

fn config_watch_paths(path: &Path) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::from([path.to_path_buf()]);
    if let Ok(resolved) = path.canonicalize() {
        paths.insert(resolved);
    }
    paths
}

fn selector_watch_targets(root: &Path, dependencies: &[String]) -> Vec<WatchTarget> {
    dependencies
        .iter()
        .map(|name| {
            exact_watch_target(
                root.join("node_modules").join(name),
                TargetSource::Dependency,
            )
        })
        .collect()
}

fn collect_watch_targets(
    root: &Path,
    database: &Path,
    docs_freshness: bool,
) -> Result<Vec<WatchTarget>> {
    let conn = store::open_path_read_only(database)?;
    let mut targets = active_git_watch_targets(root, docs_freshness);
    let mut packages = conn.prepare(
        "SELECT canonical_root, locator FROM package_instances WHERE origin='dependency'",
    )?;
    let rows = packages.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (canonical_root, locator) = row?;
        let canonical_root = PathBuf::from(canonical_root);
        targets.push(WatchTarget {
            watch_path: canonical_root.clone(),
            path: canonical_root,
            mode: RecursiveMode::Recursive,
            kind: TargetKind::Prefix,
            source: TargetSource::Dependency,
        });
        targets.push(exact_watch_target(
            root.join(locator),
            TargetSource::Dependency,
        ));
    }
    let mut checker_inputs = conn.prepare(
        "SELECT input.input_kind, input.input_path
         FROM checker_project_inputs input
         JOIN checker_enrichment_batches batch ON batch.id=input.batch_id
         WHERE batch.active=1",
    )?;
    let rows = checker_inputs.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        let (kind, input) = row?;
        let path = if kind == "absolute" {
            PathBuf::from(input)
        } else {
            root.join(input)
        };
        if !path.starts_with(root) {
            targets.push(exact_watch_target(path, TargetSource::Checker));
        }
    }
    normalize_targets(&mut targets);
    Ok(targets)
}

fn normalize_targets(targets: &mut Vec<WatchTarget>) {
    targets.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.watch_path.cmp(&right.watch_path))
            .then_with(|| left.source.cmp(&right.source))
    });
    targets.dedup_by(|left, right| {
        left.path == right.path
            && left.watch_path == right.watch_path
            && left.mode == right.mode
            && left.source == right.source
    });
}

fn exact_watch_target(path: PathBuf, source: TargetSource) -> WatchTarget {
    let watch_path = path
        .parent()
        .map_or_else(|| path.clone(), Path::to_path_buf);
    WatchTarget {
        watch_path,
        path,
        mode: RecursiveMode::NonRecursive,
        kind: TargetKind::Exact,
        source,
    }
}

fn update_classifier_targets(classifier: &mut EventClassifier, targets: &[WatchTarget]) {
    let mut config_exact = BTreeSet::new();
    let mut exact = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    for target in targets {
        if target.source == TargetSource::Config {
            config_exact.insert(target.path.clone());
            continue;
        }
        match target.kind {
            TargetKind::Exact => {
                exact.insert(target.path.clone());
            }
            TargetKind::Prefix => {
                prefixes.insert(target.path.clone());
            }
        }
    }
    classifier.set_external(config_exact, exact, prefixes);
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests;
