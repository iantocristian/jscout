use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use notify::{RecursiveMode, Watcher};
use rusqlite::Connection;

use crate::{checker, embed, indexer, store, structural, walk};

const DEFAULT_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_RETRY_INITIAL: Duration = Duration::from_millis(500);
const DEFAULT_RETRY_MAX: Duration = Duration::from_secs(30);
const OPTIONAL_PHASE_POLL: Duration = Duration::from_millis(100);
const MAX_INCREMENTAL_SOURCE_PATHS: usize = 256;

pub struct WatchOptions<'a> {
    pub database: Option<&'a Path>,
    pub embed_on_change: bool,
    pub dependencies: &'a [String],
    pub enrich_on_change: bool,
    pub enrich_timeout: Duration,
    pub checker_sidecar: Option<&'a Path>,
    pub debounce: Duration,
    pub reconcile_interval: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Phase {
    Refresh,
    Embed,
    Enrich,
}

impl fmt::Display for Phase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Refresh => "refresh",
            Self::Embed => "embed",
            Self::Enrich => "enrich",
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
}

impl DirtySignal {
    fn full(reason: impl Into<String>) -> Self {
        Self {
            scope: RefreshScope::Full,
            reasons: [reason.into()].into(),
            source_paths: BTreeSet::new(),
        }
    }

    fn source(reason: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            scope: RefreshScope::Incremental,
            reasons: [reason.into()].into(),
            source_paths: [path.into()].into(),
        }
    }

    fn merge(&mut self, other: Self) {
        self.scope = self.scope.max(other.scope);
        self.reasons.extend(other.reasons);
        self.source_paths.extend(other.source_paths);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Work {
    generation: u64,
    phase: Phase,
    refresh_scope: RefreshScope,
}

#[derive(Clone, Copy, Debug)]
struct Retry {
    work: Work,
    due: Duration,
}

#[derive(Debug, PartialEq, Eq)]
enum FinishState {
    Continue,
    Complete { degraded: bool },
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
    cycle_snapshot: Option<String>,
    cycle_degraded: bool,
    dirty_reasons: BTreeSet<String>,
    dirty_source_paths: BTreeSet<String>,
    refresh_scope: RefreshScope,
    debounce: Duration,
    retry_initial: Duration,
    retry_max: Duration,
    embed: bool,
    enrich: bool,
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
            cycle_snapshot: None,
            cycle_degraded: false,
            dirty_reasons: BTreeSet::new(),
            dirty_source_paths: BTreeSet::new(),
            refresh_scope: RefreshScope::Full,
            debounce,
            retry_initial: DEFAULT_RETRY_INITIAL,
            retry_max: DEFAULT_RETRY_MAX,
            embed,
            enrich,
        };
        coordinator.mark_dirty(Duration::ZERO, DirtySignal::full("startup"));
        coordinator.refresh_immediate = true;
        coordinator
    }

    fn mark_dirty(&mut self, now: Duration, signal: DirtySignal) {
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
            self.dirty_reasons.clear();
            self.dirty_source_paths.clear();
            self.refresh_scope = RefreshScope::Incremental;
        }
        self.last_dirty_at = now;
        self.refresh_scope = self.refresh_scope.max(signal.scope);
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
        self.cycle_snapshot = None;
        self.cycle_degraded = false;
    }

    fn mark_reconciliation(&mut self, now: Duration) {
        debug_assert!(self.is_clean());
        self.mark_dirty(now, DirtySignal::full("periodic-reconciliation"));
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

    fn finish_refresh(&mut self, work: Work, snapshot: String, degraded: bool) -> FinishState {
        self.clear_active(work);
        if work.generation != self.desired_generation {
            return FinishState::Superseded;
        }
        // The refresh operation succeeded and published a coherent snapshot.
        // Individual files that could not be read or parsed are subject-local
        // omissions: report them, mark the generation degraded, and continue.
        // Only an Err from the refresh operation enters finish_error/retry.
        self.retry_attempts.remove(&Phase::Refresh);
        self.cycle_degraded = degraded;
        self.cycle_snapshot = Some(snapshot);
        self.advance(work)
    }

    fn finish_optional(&mut self, work: Work) -> FinishState {
        self.clear_active(work);
        if work.generation != self.desired_generation {
            return FinishState::Superseded;
        }
        self.retry_attempts.remove(&work.phase);
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
            Phase::Refresh if self.enrich => Some(Phase::Enrich),
            Phase::Embed if self.enrich => Some(Phase::Enrich),
            _ => None,
        };
        if let Some(phase) = next {
            self.ready = Some(Work {
                generation: work.generation,
                phase,
                refresh_scope: work.refresh_scope,
            });
            FinishState::Continue
        } else {
            self.completed_generation = work.generation;
            self.dirty_reasons.clear();
            self.dirty_source_paths.clear();
            self.cycle_snapshot = None;
            let degraded = self.cycle_degraded;
            self.cycle_degraded = false;
            FinishState::Complete { degraded }
        }
    }

    fn is_superseded(&self, work: Work) -> bool {
        self.desired_generation != work.generation
    }

    fn is_clean(&self) -> bool {
        self.desired_generation == self.completed_generation
            && self.active.is_none()
            && self.ready.is_none()
            && self.retry.is_none()
    }
}

#[derive(Default)]
struct EventClassifier {
    root: PathBuf,
    excluded: BTreeSet<PathBuf>,
    git_controls: BTreeSet<PathBuf>,
    external_exact: BTreeSet<PathBuf>,
    external_prefixes: BTreeSet<PathBuf>,
}

impl EventClassifier {
    fn new(root: &Path, database: &Path) -> Self {
        let mut excluded = BTreeSet::new();
        excluded.insert(database.to_path_buf());
        for suffix in ["-wal", "-shm", "-journal"] {
            excluded.insert(PathBuf::from(format!("{}{suffix}", database.display())));
        }
        Self {
            root: root.to_path_buf(),
            excluded,
            git_controls: git_control_paths(root),
            external_exact: BTreeSet::new(),
            external_prefixes: BTreeSet::new(),
        }
    }

    fn set_external(&mut self, exact: BTreeSet<PathBuf>, prefixes: BTreeSet<PathBuf>) {
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
            if is_noise(&path) {
                continue;
            }
            if is_refresh_boundary(&path) {
                merge_signal(
                    &mut signal,
                    DirtySignal::full(format!("boundary:{}", display_path(&self.root, &path))),
                );
                continue;
            }
            if path.is_dir() {
                merge_signal(&mut signal, DirtySignal::full("unknown-directory-event"));
                continue;
            }
            if walk::is_indexable(&path) {
                let relative = display_path(&self.root, &path);
                merge_signal(
                    &mut signal,
                    DirtySignal::source(format!("source:{relative}"), relative),
                );
                continue;
            }
            // Existing regular files with irrelevant extensions are ordinary
            // repository noise (README edits, Finder metadata, and similar).
            // Missing paths and directories remain conservative because a
            // backend may be reporting a delete, rename, or rescan without
            // enough type information to classify it safely.
            if !path.is_file() {
                merge_signal(&mut signal, DirtySignal::full("unknown-event"));
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetSource {
    Git,
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
                        "watch coverage path={} status={} attempt={} error={error}",
                        path.display(),
                        if *failures >= 3 {
                            "degraded"
                        } else {
                            "retrying"
                        },
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

pub fn watch(root: &Path, options: &WatchOptions<'_>) -> Result<()> {
    validate_options(options)?;
    let root = root.canonicalize()?;
    let database = absolute_database_path(&root, options.database);
    let provider = if options.embed_on_change {
        Some(Arc::new(embed::Provider::from_env()?.context(
            "--embed requires JSCOUT_EMBED_PROVIDER=local, voyage, or openai",
        )?))
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
    let mut classifier = EventClassifier::new(&root, &database);
    let mut registry = WatchRegistry::default();
    let mut targets =
        collect_watch_targets(&root, &database).unwrap_or_else(|_| git_watch_targets(&root));
    targets.extend(selector_watch_targets(&root, options.dependencies));
    normalize_targets(&mut targets);
    update_classifier_targets(&mut classifier, &targets);
    registry.reconcile(&mut watcher, &root, &targets);
    // Reconciliation is anchored after a complete generation, not process
    // start or timer fire. Long generations therefore cannot create a
    // back-to-back refresh loop.
    let mut next_reconcile = None;

    eprintln!(
        "watch root={} database={} debounce_ms={} reconcile_seconds={} embed={} enrich={}",
        root.display(),
        database.display(),
        options.debounce.as_millis(),
        options.reconcile_interval.as_secs(),
        options.embed_on_change,
        options.enrich_on_change
    );
    if options.reconcile_interval.is_zero() {
        eprintln!(
            "warning: periodic reconciliation is disabled; missed notifications and degraded external coverage may remain stale until another event"
        );
    }

    loop {
        let now = started.elapsed();
        if next_reconcile.is_some_and(|deadline| now >= deadline) && coordinator.is_clean() {
            coordinator.mark_reconciliation(now);
            next_reconcile = None;
        }
        drain_events(&receiver, &classifier, &mut coordinator, started.elapsed());

        if let Some(work) = coordinator.next_work(started.elapsed()) {
            let phase_started = Instant::now();
            eprintln!(
                "watch generation={} phase={} refresh_scope={} status=started reasons={}",
                work.generation,
                work.phase,
                work.refresh_scope,
                coordinator
                    .dirty_reasons
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            );
            match work.phase {
                Phase::Refresh => {
                    match run_refresh(&root, &database, options.dependencies, work.refresh_scope) {
                        Ok(result) => {
                            indexer::report_failures(&result.outcome);
                            eprintln!(
                                "watch generation={} phase=refresh refresh_scope={} status={} snapshot={} indexed={} unchanged={} removed={} failed={} extracted_chunks={} extracted_refs={} projection={} elapsed_ms={}",
                                work.generation,
                                work.refresh_scope,
                                if result.outcome.failed == 0 {
                                    "succeeded"
                                } else {
                                    "partial"
                                },
                                result.snapshot,
                                result.outcome.indexed,
                                result.outcome.unchanged,
                                result.outcome.removed,
                                result.outcome.failed,
                                result.outcome.chunks,
                                result.outcome.refs,
                                if result.outcome.projection_rebuilt {
                                    "rebuilt"
                                } else {
                                    "reused"
                                },
                                phase_started.elapsed().as_millis()
                            );
                            let previous_targets = targets.clone();
                            targets =
                                collect_watch_targets(&root, &database).unwrap_or_else(|error| {
                                    eprintln!("watch coverage status=read-failed error={error:#}");
                                    git_watch_targets(&root)
                                });
                            if options.enrich_on_change {
                                targets.extend(
                                    previous_targets
                                        .into_iter()
                                        .filter(|target| target.source == TargetSource::Checker),
                                );
                                normalize_targets(&mut targets);
                            }
                            targets.extend(selector_watch_targets(&root, options.dependencies));
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
                                coordinator.finish_refresh(
                                    work,
                                    result.snapshot,
                                    result.outcome.failed > 0,
                                ),
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
                        run_embedding_interruptible(&root, &database, provider, &mut monitor)
                    };
                    match result {
                        Ok((done, total, canceled)) => {
                            eprintln!(
                                "watch generation={} phase=embed status={} embedded={done}/{total} elapsed_ms={}",
                                work.generation,
                                if canceled { "canceled" } else { "succeeded" },
                                phase_started.elapsed().as_millis()
                            );
                            debug_assert!(!canceled || coordinator.is_superseded(work));
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
                                "watch generation={} phase=enrich status=succeeded snapshot={} facts={} occurrences={} projects={} elapsed_ms={}",
                                work.generation,
                                report.snapshot,
                                report.facts_published,
                                report.occurrences_queried,
                                report.projects,
                                phase_started.elapsed().as_millis()
                            );
                            targets =
                                collect_watch_targets(&root, &database).unwrap_or_else(|error| {
                                    eprintln!("watch coverage status=read-failed error={error:#}");
                                    targets.clone()
                                });
                            targets.extend(selector_watch_targets(&root, options.dependencies));
                            normalize_targets(&mut targets);
                            update_classifier_targets(&mut classifier, &targets);
                            registry.reconcile(&mut watcher, &root, &targets);
                            report_finish(
                                work,
                                coordinator.finish_optional(work),
                                started.elapsed(),
                                options.reconcile_interval,
                                &mut next_reconcile,
                            );
                        }
                        Err(error) => {
                            let interrupted = checker::process::interrupt_pending();
                            let superseded = coordinator.is_superseded(work);
                            eprintln!(
                                "watch generation={} phase=enrich status={} elapsed_ms={} error={error:#}",
                                work.generation,
                                if interrupted {
                                    "interrupted"
                                } else if superseded {
                                    "canceled"
                                } else {
                                    "failed"
                                },
                                phase_started.elapsed().as_millis()
                            );
                            if interrupted {
                                eprintln!("watch status=stopped reason=interrupt");
                                return Ok(());
                            }
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
            continue;
        }

        let now = started.elapsed();
        let phase_deadline = coordinator.next_deadline();
        let deadline = match (phase_deadline, next_reconcile) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        };
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

fn validate_options(options: &WatchOptions<'_>) -> Result<()> {
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
    dependencies: &[String],
    scope: RefreshScope,
) -> Result<RefreshResult> {
    let conn = open_phase_database(root, database)?;
    let options = indexer::IndexOptions {
        dependencies: dependencies.to_vec(),
        ..Default::default()
    };
    let outcome = match scope {
        RefreshScope::Incremental => {
            indexer::incremental_refresh_repo_with_options(root, &conn, &options)?
        }
        RefreshScope::Full => indexer::refresh_repo_with_options(root, &conn, &options)?,
    };
    let snapshot = structural::current_snapshot(&conn)?;
    Ok(RefreshResult { snapshot, outcome })
}

fn run_embedding_interruptible(
    root: &Path,
    database: &Path,
    provider: Arc<embed::Provider>,
    monitor: &mut PhaseMonitor<'_>,
) -> Result<(usize, usize, bool)> {
    let root = root.to_path_buf();
    let database = database.to_path_buf();
    let canceled = Arc::new(AtomicBool::new(false));
    let worker_canceled = Arc::clone(&canceled);
    let worker = thread::spawn(move || -> Result<(usize, usize, bool)> {
        let conn = open_phase_database(&root, &database)?;
        embed::embed_missing_interruptible(&conn, &provider, 64, || {
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

fn run_enrichment_interruptible(
    root: &Path,
    database: &Path,
    options: &WatchOptions<'_>,
    monitor: &mut PhaseMonitor<'_>,
) -> Result<checker::EnrichReport> {
    let root = root.to_path_buf();
    let database = database.to_path_buf();
    let sidecar = options.checker_sidecar.map(Path::to_path_buf);
    let timeout = options.enrich_timeout;
    let worker = thread::spawn(move || {
        checker::enrich(
            &root,
            &checker::EnrichOptions {
                database: Some(&database),
                sidecar: sidecar.as_deref(),
                timeout,
                files: Vec::new(),
                packages: Vec::new(),
                members: Vec::new(),
                roles: Vec::new(),
                max_occurrences: None,
                include_all: false,
                dry_run: false,
            },
        )
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
        FinishState::Complete { degraded } => {
            eprintln!(
                "watch generation={} status={}",
                work.generation,
                if degraded { "degraded" } else { "clean" }
            );
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

/// Paths whose changes can alter source discovery, package ownership, module
/// resolution, dependency selection, or checker project ownership.
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
            | ".gitignore"
            | ".ignore"
            | ".gitmodules"
    ) || (name.starts_with("tsconfig.") && name.ends_with(".json"))
        || (name.starts_with("jsconfig.") && name.ends_with(".json"))
        || name.ends_with(".d.ts")
        || name.ends_with(".d.mts")
        || name.ends_with(".d.cts")
}

fn is_noise(path: &Path) -> bool {
    path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("node_modules")
                | Some(".git")
                | Some("dist")
                | Some("build")
                | Some(".next")
                | Some("coverage")
                | Some("out")
        )
    })
}

fn git_control_paths(root: &Path) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::from([root.join(".gitmodules")]);
    let dot_git = root.join(".git");
    let git_dir = if dot_git.is_dir() {
        Some(dot_git)
    } else {
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
                        root.join(path)
                    }
                })
        })
    };
    if let Some(git_dir) = git_dir {
        let git_dir = git_dir.canonicalize().unwrap_or(git_dir);
        paths.insert(git_dir.join("HEAD"));
    }
    paths
}

fn git_watch_targets(root: &Path) -> Vec<WatchTarget> {
    git_control_paths(root)
        .into_iter()
        .map(|path| exact_watch_target(path, TargetSource::Git))
        .collect()
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

fn collect_watch_targets(root: &Path, database: &Path) -> Result<Vec<WatchTarget>> {
    let conn = store::open_path_read_only(database)?;
    let mut targets = git_watch_targets(root);
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
    });
    targets.dedup_by(|left, right| {
        left.path == right.path && left.watch_path == right.watch_path && left.mode == right.mode
    });
}

fn exact_watch_target(path: PathBuf, source: TargetSource) -> WatchTarget {
    let watch_path = path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| path.clone());
    WatchTarget {
        watch_path,
        path,
        mode: RecursiveMode::NonRecursive,
        kind: TargetKind::Exact,
        source,
    }
}

fn update_classifier_targets(classifier: &mut EventClassifier, targets: &[WatchTarget]) {
    let mut exact = BTreeSet::new();
    let mut prefixes = BTreeSet::new();
    for target in targets {
        match target.kind {
            TargetKind::Exact => {
                exact.insert(target.path.clone());
            }
            TargetKind::Prefix => {
                prefixes.insert(target.path.clone());
            }
        }
    }
    classifier.set_external(exact, prefixes);
}

fn display_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    use anyhow::Result;

    use super::{
        Coordinator, DirtySignal, EventClassifier, FinishState, MAX_INCREMENTAL_SOURCE_PATHS,
        Phase, RefreshScope, WatchOptions, is_noise, is_refresh_boundary, run_refresh,
        validate_options,
    };

    fn seconds(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    fn source_signal(path: &str) -> DirtySignal {
        DirtySignal::source(format!("source:{path}"), path)
    }

    #[test]
    fn startup_refresh_is_immediate_and_optional_phases_are_ordered() {
        let mut coordinator = Coordinator::new(seconds(2), true, true);
        let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
        assert_eq!(refresh.phase, Phase::Refresh);
        assert_eq!(refresh.refresh_scope, RefreshScope::Full);
        assert_eq!(
            coordinator.finish_refresh(refresh, "s1".into(), false),
            FinishState::Continue
        );
        let embed = coordinator.next_work(Duration::ZERO).expect("embed");
        assert_eq!(embed.phase, Phase::Embed);
        assert_eq!(coordinator.finish_optional(embed), FinishState::Continue);
        let enrich = coordinator.next_work(Duration::ZERO).expect("enrich");
        assert_eq!(enrich.phase, Phase::Enrich);
        assert_eq!(
            coordinator.finish_optional(enrich),
            FinishState::Complete { degraded: false }
        );
    }

    #[test]
    fn an_event_during_refresh_supersedes_optional_work_and_debounces_again() {
        let mut coordinator = Coordinator::new(seconds(2), true, true);
        let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
        coordinator.mark_dirty(seconds(1), source_signal("a.ts"));
        assert_eq!(
            coordinator.finish_refresh(refresh, "s1".into(), false),
            FinishState::Superseded
        );
        assert!(coordinator.next_work(seconds(2)).is_none());
        let next = coordinator.next_work(seconds(3)).expect("next refresh");
        assert_eq!(next.generation, 2);
        assert_eq!(next.phase, Phase::Refresh);
        assert_eq!(next.refresh_scope, RefreshScope::Incremental);
    }

    #[test]
    fn failed_work_retries_without_a_new_event_with_bounded_backoff() {
        let mut coordinator = Coordinator::new(Duration::from_millis(100), false, false);
        let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
        assert_eq!(
            coordinator.finish_error(Duration::ZERO, refresh),
            FinishState::Retry {
                after: Duration::from_millis(500)
            }
        );
        // The parked retry gates the fresh-work path even though debounce has
        // already elapsed.
        assert!(coordinator.next_work(Duration::from_millis(100)).is_none());
        assert!(coordinator.next_work(Duration::from_millis(499)).is_none());
        let retry = coordinator
            .next_work(Duration::from_millis(500))
            .expect("retry");
        assert_eq!(retry, refresh);
        assert_eq!(
            coordinator.finish_error(Duration::from_millis(500), retry),
            FinishState::Retry {
                after: Duration::from_secs(1)
            }
        );
        assert!(
            coordinator
                .next_work(Duration::from_millis(1_499))
                .is_none()
        );
        assert_eq!(
            coordinator
                .next_work(Duration::from_millis(1_500))
                .expect("second retry"),
            refresh
        );
    }

    #[test]
    fn partial_refresh_advances_immediately_as_degraded() {
        let mut coordinator = Coordinator::new(seconds(2), true, false);
        let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
        assert_eq!(
            coordinator.finish_refresh(refresh, "partial".into(), true),
            FinishState::Continue
        );
        let embed = coordinator.next_work(Duration::ZERO).expect("embed");
        assert_eq!(embed.phase, Phase::Embed);
        assert_eq!(
            coordinator.finish_optional(embed),
            FinishState::Complete { degraded: true }
        );
        assert!(coordinator.retry.is_none());
    }

    #[test]
    fn reconciliation_is_immediate_only_after_the_previous_generation_completes() {
        let mut coordinator = Coordinator::new(seconds(2), false, false);
        let startup = coordinator.next_work(Duration::ZERO).expect("startup");
        assert_eq!(
            coordinator.finish_refresh(startup, "s1".into(), false),
            FinishState::Complete { degraded: false }
        );
        assert!(coordinator.is_clean());

        coordinator.mark_reconciliation(seconds(10));
        let refresh = coordinator
            .next_work(seconds(10))
            .expect("immediate reconciliation");
        assert_eq!(refresh.generation, 2);
        assert_eq!(refresh.refresh_scope, RefreshScope::Full);
    }

    #[test]
    fn events_coalesce_into_one_successor_generation_and_drop_old_reasons() {
        let mut coordinator = Coordinator::new(seconds(2), false, false);
        let startup = coordinator.next_work(Duration::ZERO).expect("startup");
        coordinator.mark_dirty(seconds(1), source_signal("a.ts"));
        coordinator.mark_dirty(seconds(2), source_signal("b.ts"));
        assert_eq!(coordinator.desired_generation, 2);
        assert_eq!(
            coordinator.dirty_reasons,
            ["source:a.ts".to_string(), "source:b.ts".to_string()].into()
        );
        assert_eq!(coordinator.refresh_scope, RefreshScope::Incremental);
        assert_eq!(
            coordinator.finish_refresh(startup, "old".into(), false),
            FinishState::Superseded
        );
    }

    #[test]
    fn a_full_refresh_signal_is_sticky_within_the_generation() {
        let mut coordinator = Coordinator::new(seconds(2), false, false);
        let startup = coordinator.next_work(Duration::ZERO).expect("startup");
        assert_eq!(
            coordinator.finish_refresh(startup, "s1".into(), false),
            FinishState::Complete { degraded: false }
        );

        coordinator.mark_dirty(seconds(1), source_signal("a.ts"));
        coordinator.mark_dirty(seconds(2), DirtySignal::full("boundary:package.json"));
        coordinator.mark_dirty(seconds(3), source_signal("b.ts"));

        let work = coordinator.next_work(seconds(5)).expect("refresh");
        assert_eq!(work.refresh_scope, RefreshScope::Full);
        assert_eq!(
            coordinator.dirty_source_paths,
            ["a.ts".to_string(), "b.ts".to_string()].into()
        );
        assert!(coordinator.dirty_reasons.contains("source:a.ts"));
        assert!(coordinator.dirty_reasons.contains("source:b.ts"));
        assert!(coordinator.dirty_reasons.contains("boundary:package.json"));
    }

    #[test]
    fn a_large_source_batch_promotes_to_full_refresh() {
        let mut coordinator = Coordinator::new(seconds(2), false, false);
        let startup = coordinator.next_work(Duration::ZERO).expect("startup");
        assert_eq!(
            coordinator.finish_refresh(startup, "s1".into(), false),
            FinishState::Complete { degraded: false }
        );

        for index in 0..=MAX_INCREMENTAL_SOURCE_PATHS {
            let path = format!("src/file-{index}.ts");
            coordinator.mark_dirty(seconds(1), source_signal(&path));
        }

        let work = coordinator.next_work(seconds(3)).expect("refresh");
        assert_eq!(work.refresh_scope, RefreshScope::Full);
        assert!(coordinator.dirty_reasons.contains("mass-source-change"));
        assert_eq!(
            coordinator.dirty_source_paths.len(),
            MAX_INCREMENTAL_SOURCE_PATHS
        );
    }

    #[test]
    fn event_classifier_excludes_only_the_exact_database_family() {
        let root = PathBuf::from("/repo");
        let database = root.join(".jscout.db");
        let classifier = EventClassifier::new(&root, &database);
        assert!(classifier.classify(&[database]).is_none());
        assert!(
            classifier
                .classify(&[root.join(".jscout.db-wal")])
                .is_none()
        );
        assert!(
            classifier
                .classify(&[root.join(".jscout-notes.ts")])
                .is_some_and(|signal| signal.scope == RefreshScope::Incremental)
        );
    }

    #[test]
    fn selected_external_prefix_overrides_node_modules_noise() {
        let root = PathBuf::from("/repo");
        let dependency = root.join("node_modules/pkg");
        let mut classifier = EventClassifier::new(&root, &root.join(".jscout.db"));
        classifier.set_external(Default::default(), [dependency.clone()].into());
        assert!(
            classifier
                .classify(&[dependency.join("index.js")])
                .is_some_and(|signal| signal.scope == RefreshScope::Full)
        );
        assert!(
            classifier
                .classify(&[root.join("node_modules/other/index.js")])
                .is_none()
        );
    }

    #[test]
    fn a_refresh_boundary_dominates_source_paths_in_one_event() {
        let root = PathBuf::from("/repo");
        let classifier = EventClassifier::new(&root, &root.join(".jscout.db"));

        let signal = classifier
            .classify(&[root.join("src/main.ts"), root.join("package.json")])
            .expect("relevant event");

        assert_eq!(signal.scope, RefreshScope::Full);
        assert!(signal.reasons.contains("source:src/main.ts"));
        assert!(signal.reasons.contains("boundary:package.json"));
    }

    #[test]
    fn lockfiles_and_configs_are_full_refresh_boundaries() {
        assert!(is_refresh_boundary(Path::new("pnpm-lock.yaml")));
        assert!(is_refresh_boundary(Path::new("pnpm-workspace.yaml")));
        assert!(is_refresh_boundary(Path::new("package-lock.json")));
        assert!(is_refresh_boundary(Path::new("yarn.lock")));
        assert!(is_refresh_boundary(Path::new("tsconfig.server.json")));
        assert!(is_refresh_boundary(Path::new("types/ambient.d.ts")));
        assert!(is_refresh_boundary(Path::new(".gitignore")));
        assert!(is_refresh_boundary(Path::new(".ignore")));
        assert!(is_noise(Path::new("node_modules/dep/index.js")));
        assert!(!is_noise(Path::new("pnpm-lock.yaml")));

        let root = PathBuf::from("/repo");
        let classifier = EventClassifier::new(&root, &root.join(".jscout.db"));
        for boundary in [".gitignore", ".ignore", "pnpm-workspace.yaml"] {
            assert!(
                classifier
                    .classify(&[root.join(boundary)])
                    .is_some_and(|signal| signal.scope == RefreshScope::Full),
                "{boundary} must force a full refresh"
            );
        }
    }

    #[test]
    fn irrelevant_regular_files_are_ignored_but_uncertain_shapes_rebuild() -> Result<()> {
        let root = tempfile::tempdir()?;
        fs::write(root.path().join("README.md"), "documentation\n")?;
        fs::write(root.path().join(".DS_Store"), "metadata\n")?;
        fs::create_dir(root.path().join("renamed-directory"))?;
        fs::create_dir(root.path().join(".git"))?;
        fs::write(root.path().join(".git/index"), "git metadata\n")?;
        let classifier = EventClassifier::new(root.path(), &root.path().join("watch.db"));

        assert!(
            classifier
                .classify(&[root.path().join("README.md")])
                .is_none()
        );
        assert!(
            classifier
                .classify(&[root.path().join(".DS_Store")])
                .is_none()
        );
        assert!(
            classifier
                .classify(&[root.path().join(".git/index")])
                .is_none()
        );
        assert_eq!(
            classifier.classify(&[root.path().join("renamed-directory")]),
            Some(DirtySignal::full("unknown-directory-event"))
        );
        assert_eq!(
            classifier.classify(&[root.path().join("deleted-unknown-file")]),
            Some(DirtySignal::full("unknown-event"))
        );
        Ok(())
    }

    #[test]
    fn reconciliation_interval_must_exceed_debounce() {
        let options = WatchOptions {
            database: None,
            embed_on_change: false,
            dependencies: &[],
            enrich_on_change: false,
            enrich_timeout: seconds(300),
            checker_sidecar: None,
            debounce: seconds(2),
            reconcile_interval: seconds(2),
        };
        let error = validate_options(&options).expect_err("invalid interval");
        assert!(error.to_string().contains("must exceed"));
    }

    #[test]
    fn refresh_phase_replaces_the_complete_file_set() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("a.ts"), "export const a = 1;\n")?;
        let database = directory.path().join("watch.db");
        let first = run_refresh(directory.path(), &database, &[], RefreshScope::Full)?;
        assert_eq!(first.outcome.indexed, 1);
        fs::remove_file(directory.path().join("a.ts"))?;
        fs::write(directory.path().join("b.ts"), "export const b = 2;\n")?;
        let second = run_refresh(directory.path(), &database, &[], RefreshScope::Incremental)?;
        assert_eq!(second.outcome.indexed, 1);
        assert_eq!(second.outcome.removed, 1);
        let conn = crate::store::open_path_read_only(&database)?;
        let paths = conn
            .prepare("SELECT path FROM files ORDER BY path")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(paths, vec!["b.ts"]);
        Ok(())
    }

    #[test]
    fn incremental_refresh_reuses_unchanged_source_rows() -> Result<()> {
        let directory = tempfile::tempdir()?;
        fs::write(directory.path().join("a.ts"), "export const a = 1;\n")?;
        fs::write(directory.path().join("b.ts"), "export const b = 2;\n")?;
        let database = directory.path().join("watch.db");
        run_refresh(directory.path(), &database, &[], RefreshScope::Full)?;

        fs::write(directory.path().join("a.ts"), "export const a = 3;\n")?;
        let refreshed = run_refresh(directory.path(), &database, &[], RefreshScope::Incremental)?;

        assert_eq!(
            (refreshed.outcome.indexed, refreshed.outcome.unchanged),
            (1, 1)
        );
        assert_eq!(refreshed.outcome.removed, 0);
        assert!(refreshed.outcome.projection_rebuilt);
        Ok(())
    }
}
