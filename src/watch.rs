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
const STABLE_FAILURE_THRESHOLD: u8 = 3;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Work {
    generation: u64,
    phase: Phase,
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
    stable_failure: Option<(String, u8)>,
    cycle_snapshot: Option<String>,
    cycle_degraded: bool,
    dirty_reasons: BTreeSet<String>,
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
            stable_failure: None,
            cycle_snapshot: None,
            cycle_degraded: false,
            dirty_reasons: BTreeSet::new(),
            debounce,
            retry_initial: DEFAULT_RETRY_INITIAL,
            retry_max: DEFAULT_RETRY_MAX,
            embed,
            enrich,
        };
        coordinator.mark_dirty(Duration::ZERO, "startup");
        coordinator.refresh_immediate = true;
        coordinator
    }

    fn mark_dirty(&mut self, now: Duration, reason: impl Into<String>) {
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
        }
        self.last_dirty_at = now;
        self.dirty_reasons.insert(reason.into());
        self.refresh_immediate = false;
        self.ready = None;
        self.retry = None;
        self.retry_attempts.clear();
        self.cycle_snapshot = None;
        self.cycle_degraded = false;
    }

    fn mark_reconciliation(&mut self, now: Duration) {
        debug_assert!(self.is_clean());
        self.mark_dirty(now, "periodic-reconciliation");
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

    fn finish_refresh(
        &mut self,
        now: Duration,
        work: Work,
        snapshot: String,
        failure_fingerprint: Option<String>,
    ) -> FinishState {
        self.clear_active(work);
        if work.generation != self.desired_generation {
            return FinishState::Superseded;
        }
        if let Some(fingerprint) = failure_fingerprint {
            let repeats = match &self.stable_failure {
                Some((previous, repeats)) if previous == &fingerprint => repeats.saturating_add(1),
                _ => 1,
            };
            self.stable_failure = Some((fingerprint, repeats));
            if repeats < STABLE_FAILURE_THRESHOLD {
                return self.schedule_retry(now, work);
            }
            self.retry_attempts.remove(&Phase::Refresh);
            self.cycle_degraded = true;
        } else {
            self.retry_attempts.remove(&Phase::Refresh);
            self.stable_failure = None;
            self.cycle_degraded = false;
        }
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
            });
            FinishState::Continue
        } else {
            self.completed_generation = work.generation;
            self.dirty_reasons.clear();
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

    fn classify(&self, paths: &[PathBuf]) -> Option<String> {
        if paths.is_empty() {
            return Some("unknown-event".into());
        }
        let mut saw_uncertain = false;
        for path in paths {
            let path = self.absolute(path);
            if self.excluded.contains(&path) {
                continue;
            }
            if self.git_controls.contains(&path) {
                return Some(format!("git:{}", display_path(&self.root, &path)));
            }
            if self.external_exact.contains(&path)
                || self
                    .external_prefixes
                    .iter()
                    .any(|prefix| path.starts_with(prefix))
            {
                return Some(format!("external:{}", path.display()));
            }
            if is_noise(&path) {
                continue;
            }
            if is_relevant(&path) {
                return Some(format!("source:{}", display_path(&self.root, &path)));
            }
            // Existing regular files with irrelevant extensions are ordinary
            // repository noise (README edits, Finder metadata, and similar).
            // Missing paths and directories remain conservative because a
            // backend may be reporting a delete, rename, or rescan without
            // enough type information to classify it safely.
            if !path.is_file() {
                saw_uncertain = true;
            }
        }
        saw_uncertain.then(|| "unknown-event".into())
    }

    fn absolute(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.root.join(path)
        }
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
    failure_fingerprint: Option<String>,
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
                "watch generation={} phase={} status=started reasons={}",
                work.generation,
                work.phase,
                coordinator
                    .dirty_reasons
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(",")
            );
            match work.phase {
                Phase::Refresh => match run_refresh(&root, &database, options.dependencies) {
                    Ok(result) => {
                        indexer::report_failures(&result.outcome);
                        eprintln!(
                            "watch generation={} phase=refresh status={} snapshot={} indexed={} failed={} chunks={} refs={} elapsed_ms={}",
                            work.generation,
                            if result.outcome.failed == 0 {
                                "succeeded"
                            } else {
                                "partial"
                            },
                            result.snapshot,
                            result.outcome.indexed,
                            result.outcome.failed,
                            result.outcome.chunks,
                            result.outcome.refs,
                            phase_started.elapsed().as_millis()
                        );
                        let previous_targets = targets.clone();
                        targets = collect_watch_targets(&root, &database).unwrap_or_else(|error| {
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
                        drain_events(&receiver, &classifier, &mut coordinator, started.elapsed());
                        report_finish(
                            work,
                            coordinator.finish_refresh(
                                started.elapsed(),
                                work,
                                result.snapshot,
                                result.failure_fingerprint,
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
                },
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
                            eprintln!(
                                "watch generation={} phase=enrich status=failed elapsed_ms={} error={error:#}",
                                work.generation,
                                phase_started.elapsed().as_millis()
                            );
                            if checker::process::interrupt_pending() {
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

fn run_refresh(root: &Path, database: &Path, dependencies: &[String]) -> Result<RefreshResult> {
    let conn = open_phase_database(root, database)?;
    let outcome = indexer::refresh_repo_with_options(
        root,
        &conn,
        &indexer::IndexOptions {
            dependencies: dependencies.to_vec(),
            ..Default::default()
        },
    )?;
    let snapshot = structural::current_snapshot(&conn)?;
    let failure_fingerprint = failure_fingerprint(&outcome);
    Ok(RefreshResult {
        snapshot,
        outcome,
        failure_fingerprint,
    })
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
    let reason = match event {
        Ok(event) => classifier.classify(&event.paths),
        Err(error) => Some(format!("watch-error:{error}")),
    };
    if let Some(reason) = reason {
        coordinator.mark_dirty(now, reason);
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

fn failure_fingerprint(outcome: &indexer::IndexOutcome) -> Option<String> {
    if outcome.failures.is_empty() {
        return None;
    }
    let mut failures = outcome
        .failures
        .iter()
        .map(|failure| format!("{}\0{}\0{}", failure.path, failure.stage, failure.error))
        .collect::<Vec<_>>();
    failures.sort();
    Some(
        blake3::hash(failures.join("\n").as_bytes())
            .to_hex()
            .to_string(),
    )
}

/// Paths that change extraction, module resolution, or checker ownership.
fn is_relevant(path: &Path) -> bool {
    if walk::is_indexable(path) {
        return true;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name,
        "package.json"
            | "package-lock.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lock"
            | "bun.lockb"
            | "tsconfig.json"
            | "jsconfig.json"
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
        Coordinator, EventClassifier, FinishState, Phase, WatchOptions, is_noise, is_relevant,
        run_refresh, validate_options,
    };

    fn seconds(value: u64) -> Duration {
        Duration::from_secs(value)
    }

    #[test]
    fn startup_refresh_is_immediate_and_optional_phases_are_ordered() {
        let mut coordinator = Coordinator::new(seconds(2), true, true);
        let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
        assert_eq!(refresh.phase, Phase::Refresh);
        assert_eq!(
            coordinator.finish_refresh(Duration::ZERO, refresh, "s1".into(), None),
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
        coordinator.mark_dirty(seconds(1), "source:a.ts");
        assert_eq!(
            coordinator.finish_refresh(seconds(1), refresh, "s1".into(), None),
            FinishState::Superseded
        );
        assert!(coordinator.next_work(seconds(2)).is_none());
        let next = coordinator.next_work(seconds(3)).expect("next refresh");
        assert_eq!(next.generation, 2);
        assert_eq!(next.phase, Phase::Refresh);
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
    fn three_identical_partial_refreshes_publish_a_degraded_generation() {
        let mut coordinator = Coordinator::new(seconds(2), false, false);
        let mut now = Duration::ZERO;
        for attempt in 0..3 {
            let work = coordinator.next_work(now).expect("refresh attempt");
            let state = coordinator.finish_refresh(
                now,
                work,
                format!("s{attempt}"),
                Some("same-failure".into()),
            );
            if attempt < 2 {
                let FinishState::Retry { after } = state else {
                    panic!("expected retry")
                };
                assert_eq!(
                    after,
                    if attempt == 0 {
                        Duration::from_millis(500)
                    } else {
                        Duration::from_secs(1)
                    }
                );
                now = now.saturating_add(after);
            } else {
                assert_eq!(state, FinishState::Complete { degraded: true });
            }
        }
    }

    #[test]
    fn a_known_degraded_failure_does_not_pay_three_retries_each_generation() {
        let mut coordinator = Coordinator::new(Duration::from_millis(100), false, false);
        let mut now = Duration::ZERO;
        for _ in 0..3 {
            let work = coordinator.next_work(now).expect("refresh attempt");
            match coordinator.finish_refresh(
                now,
                work,
                "partial".into(),
                Some("permanent-failure".into()),
            ) {
                FinishState::Retry { after } => now = now.saturating_add(after),
                FinishState::Complete { degraded: true } => {}
                other => panic!("unexpected state: {other:?}"),
            }
        }

        coordinator.mark_dirty(now, "source:still-unreadable.ts");
        now = now.saturating_add(Duration::from_millis(100));
        let work = coordinator.next_work(now).expect("next generation");
        assert_eq!(
            coordinator.finish_refresh(
                now,
                work,
                "partial".into(),
                Some("permanent-failure".into()),
            ),
            FinishState::Complete { degraded: true }
        );
    }

    #[test]
    fn reconciliation_is_immediate_only_after_the_previous_generation_completes() {
        let mut coordinator = Coordinator::new(seconds(2), false, false);
        let startup = coordinator.next_work(Duration::ZERO).expect("startup");
        assert_eq!(
            coordinator.finish_refresh(Duration::ZERO, startup, "s1".into(), None),
            FinishState::Complete { degraded: false }
        );
        assert!(coordinator.is_clean());

        coordinator.mark_reconciliation(seconds(10));
        let refresh = coordinator
            .next_work(seconds(10))
            .expect("immediate reconciliation");
        assert_eq!(refresh.generation, 2);
    }

    #[test]
    fn events_coalesce_into_one_successor_generation_and_drop_old_reasons() {
        let mut coordinator = Coordinator::new(seconds(2), false, false);
        let startup = coordinator.next_work(Duration::ZERO).expect("startup");
        coordinator.mark_dirty(seconds(1), "source:a.ts");
        coordinator.mark_dirty(seconds(2), "source:b.ts");
        assert_eq!(coordinator.desired_generation, 2);
        assert_eq!(
            coordinator.dirty_reasons,
            ["source:a.ts".to_string(), "source:b.ts".to_string()].into()
        );
        assert_eq!(
            coordinator.finish_refresh(seconds(2), startup, "old".into(), None),
            FinishState::Superseded
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
                .is_some()
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
                .is_some()
        );
        assert!(
            classifier
                .classify(&[root.join("node_modules/other/index.js")])
                .is_none()
        );
    }

    #[test]
    fn lockfiles_configs_and_declarations_trigger_reconciliation() {
        assert!(is_relevant(Path::new("pnpm-lock.yaml")));
        assert!(is_relevant(Path::new("package-lock.json")));
        assert!(is_relevant(Path::new("yarn.lock")));
        assert!(is_relevant(Path::new("tsconfig.server.json")));
        assert!(is_relevant(Path::new("types/ambient.d.ts")));
        assert!(is_noise(Path::new("node_modules/dep/index.js")));
        assert!(!is_noise(Path::new("pnpm-lock.yaml")));
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
            Some("unknown-event".into())
        );
        assert_eq!(
            classifier.classify(&[root.path().join("deleted-unknown-file")]),
            Some("unknown-event".into())
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
        let first = run_refresh(directory.path(), &database, &[])?;
        assert_eq!(first.outcome.indexed, 1);
        fs::remove_file(directory.path().join("a.ts"))?;
        fs::write(directory.path().join("b.ts"), "export const b = 2;\n")?;
        let second = run_refresh(directory.path(), &database, &[])?;
        assert_eq!(second.outcome.indexed, 1);
        let conn = crate::store::open_path_read_only(&database)?;
        let paths = conn
            .prepare("SELECT path FROM files ORDER BY path")?
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        assert_eq!(paths, vec!["b.ts"]);
        Ok(())
    }
}
