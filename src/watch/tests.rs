use std::collections::BTreeSet;
#[cfg(unix)]
use std::ffi::OsString;
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStringExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::Result;

use crate::indexer;

use super::{
    Coordinator, DirtySignal, DocsIndexingPolicy, EventClassifier, FinishState,
    MAX_INCREMENTAL_SOURCE_PATHS, Phase, RefreshScope, RejectionReportDecision,
    RejectionReportLatch, TargetKind, TargetSource, WatchOptions, active_git_control_paths,
    active_git_watch_targets, clear_reconciliation_deadline_if_dirty, config_watch_paths,
    effective_watch_policy_fingerprint, extend_config_watch_targets,
    extend_configured_watch_targets, git_control_paths, git_path, git_watch_targets,
    is_refresh_boundary, normalize_targets, parse_git_ascii_scalar, prepare_refresh, run_refresh,
    strip_git_record_terminator, update_classifier_targets, validate_options, watch_enrich_options,
    watch_startup_log,
};

fn rejection(path: &str, stage: &'static str, error: &str) -> crate::indexer::IndexRejection {
    crate::indexer::IndexRejection {
        path: path.to_string(),
        stage,
        error: error.to_string(),
    }
}

fn seconds(value: u64) -> Duration {
    Duration::from_secs(value)
}

fn source_signal(path: &str) -> DirtySignal {
    DirtySignal::source(format!("source:{path}"), path)
}

fn git(root: &Path, arguments: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "git {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}

#[test]
fn git_record_parsing_removes_only_the_output_terminator() {
    assert_eq!(
        strip_git_record_terminator(b"index-with-newline\n\n".to_vec()),
        Some(b"index-with-newline\n".to_vec())
    );
    assert_eq!(
        strip_git_record_terminator(b"index-with-tab\t\n".to_vec()),
        Some(b"index-with-tab\t".to_vec())
    );
    #[cfg(not(windows))]
    assert_eq!(
        strip_git_record_terminator(b"index-with-carriage-return\r\n".to_vec()),
        Some(b"index-with-carriage-return\r".to_vec())
    );
    assert_eq!(strip_git_record_terminator(b"\n".to_vec()), None);
    assert_eq!(
        parse_git_ascii_scalar(b"reftable\n".to_vec()).as_deref(),
        Some("reftable")
    );
    assert_eq!(parse_git_ascii_scalar(b"\n".to_vec()), None);
    assert_eq!(parse_git_ascii_scalar(b"reftable\nfiles\n".to_vec()), None);
}

#[test]
fn rejection_details_are_reported_once_per_distinct_set() {
    let first = rejection("video-a.ts", "read", "invalid UTF-8");
    let second = rejection("video-b.ts", "extract", "unsupported syntax");
    let mut latch = RejectionReportLatch::default();

    assert_eq!(
        latch.observe(&[first.clone(), second.clone()]),
        RejectionReportDecision::Details
    );
    assert_eq!(
        latch.observe(&[first.clone(), second.clone()]),
        RejectionReportDecision::Silent
    );
    assert_eq!(
        latch.observe(&[second.clone(), first.clone()]),
        RejectionReportDecision::Silent
    );

    let changed_error = rejection("video-a.ts", "read", "permission denied");
    assert_eq!(
        latch.observe(&[changed_error, second]),
        RejectionReportDecision::Details
    );
}

#[test]
fn rejection_recovery_and_reappearance_are_each_reported_once() {
    let rejected = rejection("video.ts", "read", "invalid UTF-8");
    let mut latch = RejectionReportLatch::default();

    assert_eq!(latch.observe(&[]), RejectionReportDecision::Silent);
    assert_eq!(
        latch.observe(std::slice::from_ref(&rejected)),
        RejectionReportDecision::Details
    );
    assert_eq!(
        latch.observe(&[]),
        RejectionReportDecision::Cleared { previous: 1 }
    );
    assert_eq!(latch.observe(&[]), RejectionReportDecision::Silent);
    assert_eq!(latch.observe(&[rejected]), RejectionReportDecision::Details);
}

#[test]
fn startup_refresh_is_immediate_and_optional_phases_are_ordered() {
    let mut coordinator = Coordinator::new(seconds(2), true, true);
    let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
    assert_eq!(refresh.phase, Phase::Refresh);
    assert_eq!(refresh.refresh_scope, RefreshScope::Full);
    assert_eq!(coordinator.finish_refresh(refresh), FinishState::Continue);
    let embed = coordinator.next_work(Duration::ZERO).expect("embed");
    assert_eq!(embed.phase, Phase::Embed);
    assert_eq!(coordinator.finish_optional(embed), FinishState::Continue);
    let enrich = coordinator.next_work(Duration::ZERO).expect("enrich");
    assert_eq!(enrich.phase, Phase::Enrich);
    assert_eq!(coordinator.finish_optional(enrich), FinishState::Continue);
    let semantic = coordinator
        .next_work(Duration::ZERO)
        .expect("semantic embed");
    assert_eq!(semantic.phase, Phase::SemanticEmbed);
    assert_eq!(coordinator.finish_optional(semantic), FinishState::Complete);
}

#[test]
fn terminal_enrichment_partial_survives_the_semantic_embedding_tail() {
    let mut coordinator = Coordinator::new(seconds(2), true, true);
    let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
    assert_eq!(coordinator.finish_refresh(refresh), FinishState::Continue);
    let embed = coordinator.next_work(Duration::ZERO).expect("embed");
    assert_eq!(coordinator.finish_optional(embed), FinishState::Continue);
    let enrich = coordinator.next_work(Duration::ZERO).expect("enrich");
    assert_eq!(
        coordinator.finish_optional_partial(enrich),
        FinishState::Continue
    );
    let semantic = coordinator
        .next_work(Duration::ZERO)
        .expect("semantic embed");
    assert_eq!(coordinator.finish_optional(semantic), FinishState::Partial);
}

#[test]
fn semantic_embedding_immediately_follows_code_embedding_without_enrichment() {
    let mut coordinator = Coordinator::new(seconds(2), true, false);
    let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
    assert_eq!(coordinator.finish_refresh(refresh), FinishState::Continue);
    let embed = coordinator.next_work(Duration::ZERO).expect("embed");
    assert_eq!(embed.phase, Phase::Embed);
    assert_eq!(coordinator.finish_optional(embed), FinishState::Continue);
    let semantic = coordinator
        .next_work(Duration::ZERO)
        .expect("semantic embed");
    assert_eq!(semantic.phase, Phase::SemanticEmbed);
    assert_eq!(coordinator.finish_optional(semantic), FinishState::Complete);
}

#[test]
fn an_event_during_refresh_supersedes_optional_work_and_debounces_again() {
    let mut coordinator = Coordinator::new(seconds(2), true, true);
    let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
    coordinator.mark_dirty(seconds(1), source_signal("a.ts"));
    assert_eq!(coordinator.finish_refresh(refresh), FinishState::Superseded);
    assert!(coordinator.next_work(seconds(2)).is_none());
    let next = coordinator.next_work(seconds(3)).expect("next refresh");
    assert_eq!(next.generation, 2);
    assert_eq!(next.phase, Phase::Refresh);
    assert_eq!(next.refresh_scope, RefreshScope::Incremental);
}

#[test]
fn failed_work_retries_without_a_new_event_with_capped_delay() {
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
fn source_event_cannot_downgrade_a_failed_full_refresh_retry() {
    let mut coordinator = Coordinator::new(seconds(2), false, false);
    let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
    assert_eq!(refresh.refresh_scope, RefreshScope::Full);
    assert!(matches!(
        coordinator.finish_error(Duration::ZERO, refresh),
        FinishState::Retry { .. }
    ));

    coordinator.mark_dirty(Duration::from_millis(100), source_signal("changed.ts"));
    let successor = coordinator
        .next_work(Duration::from_millis(2_100))
        .expect("successor refresh");

    assert_eq!(successor.generation, 2);
    assert_eq!(successor.refresh_scope, RefreshScope::Full);
    assert!(coordinator.dirty_reasons.contains("startup"));
    assert!(coordinator.dirty_reasons.contains("source:changed.ts"));
}

#[test]
fn optional_phase_retry_does_not_force_the_next_refresh_full() {
    let mut coordinator = Coordinator::new(seconds(2), true, false);
    let refresh = coordinator.next_work(Duration::ZERO).expect("refresh");
    assert_eq!(coordinator.finish_refresh(refresh), FinishState::Continue);
    let embed = coordinator.next_work(Duration::ZERO).expect("embed");
    assert!(matches!(
        coordinator.finish_error(Duration::ZERO, embed),
        FinishState::Retry { .. }
    ));

    coordinator.mark_dirty(Duration::from_millis(100), source_signal("changed.ts"));
    let successor = coordinator
        .next_work(Duration::from_millis(2_100))
        .expect("successor refresh");

    assert_eq!(successor.refresh_scope, RefreshScope::Incremental);
}

#[test]
fn dirty_generation_discards_the_old_reconciliation_deadline() {
    let mut coordinator = Coordinator::new(seconds(2), false, false);
    let startup = coordinator.next_work(Duration::ZERO).expect("startup");
    assert_eq!(coordinator.finish_refresh(startup), FinishState::Complete);
    let mut next_reconcile = Some(seconds(600));

    coordinator.mark_dirty(seconds(599), source_signal("changed.ts"));
    clear_reconciliation_deadline_if_dirty(&coordinator, &mut next_reconcile);

    assert_eq!(next_reconcile, None);
}

#[test]
fn reconciliation_is_immediate_only_after_the_previous_generation_completes() {
    let mut coordinator = Coordinator::new(seconds(2), false, false);
    let startup = coordinator.next_work(Duration::ZERO).expect("startup");
    assert_eq!(coordinator.finish_refresh(startup), FinishState::Complete);
    assert!(coordinator.is_clean());

    coordinator.mark_reconciliation(seconds(10));
    let refresh = coordinator
        .next_work(seconds(10))
        .expect("immediate reconciliation");
    assert_eq!(refresh.generation, 2);
    assert_eq!(refresh.refresh_scope, RefreshScope::Incremental);
    assert!(!refresh.force_full_enrichment);
}

#[test]
fn checker_drift_flush_is_independent_and_forces_only_enrichment() {
    let mut coordinator = Coordinator::new(seconds(2), false, true);
    let startup = coordinator.next_work(Duration::ZERO).expect("startup");
    assert_eq!(coordinator.finish_refresh(startup), FinishState::Continue);
    let startup_enrich = coordinator.next_work(Duration::ZERO).expect("enrich");
    assert_eq!(
        coordinator.finish_optional(startup_enrich),
        FinishState::Complete
    );

    coordinator.mark_checker_drift_flush(seconds(86_400));
    let refresh = coordinator
        .next_work(seconds(86_400))
        .expect("drift refresh");
    assert_eq!(refresh.refresh_scope, RefreshScope::Incremental);
    assert!(refresh.force_full_enrichment);
    assert_eq!(coordinator.finish_refresh(refresh), FinishState::Continue);
    let enrich = coordinator.next_work(seconds(86_400)).expect("full enrich");
    assert_eq!(enrich.phase, Phase::Enrich);
    assert!(enrich.force_full_enrichment);
    assert_eq!(coordinator.finish_optional(enrich), FinishState::Complete);
    assert!(!coordinator.force_full_enrichment);
}

#[test]
fn source_event_superseding_a_drift_flush_keeps_the_full_enrichment_requirement() {
    let mut coordinator = Coordinator::new(seconds(2), false, true);
    let startup = coordinator.next_work(Duration::ZERO).expect("startup");
    assert_eq!(coordinator.finish_refresh(startup), FinishState::Continue);
    let enrich = coordinator.next_work(Duration::ZERO).expect("enrich");
    assert_eq!(coordinator.finish_optional(enrich), FinishState::Complete);

    coordinator.mark_checker_drift_flush(seconds(86_400));
    let drift_refresh = coordinator.next_work(seconds(86_400)).expect("drift");
    coordinator.mark_dirty(seconds(86_401), source_signal("changed.ts"));
    assert_eq!(
        coordinator.finish_refresh(drift_refresh),
        FinishState::Superseded
    );
    let successor = coordinator.next_work(seconds(86_403)).expect("successor");
    assert!(successor.force_full_enrichment);
    assert!(coordinator.dirty_reasons.contains("checker-drift-flush"));
    assert!(coordinator.dirty_reasons.contains("source:changed.ts"));
}

#[test]
fn checker_dirty_paths_survive_supersession_until_successor_publication() {
    let mut coordinator = Coordinator::new(seconds(2), false, true);
    let startup = coordinator.next_work(Duration::ZERO).expect("startup");
    assert_eq!(coordinator.finish_refresh(startup), FinishState::Continue);
    let startup_enrich = coordinator.next_work(Duration::ZERO).expect("enrich");
    assert_eq!(
        coordinator.finish_enrichment_success(startup_enrich),
        FinishState::Complete
    );

    coordinator.mark_dirty(seconds(1), source_signal("src/a.ts"));
    let refresh_a = coordinator.next_work(seconds(3)).expect("refresh A");
    assert_eq!(coordinator.finish_refresh(refresh_a), FinishState::Continue);
    let enrich_a = coordinator.next_work(seconds(3)).expect("enrich A");
    assert_eq!(
        coordinator.checker_dirty_source_paths,
        BTreeSet::from(["src/a.ts".to_string()])
    );

    coordinator.mark_dirty(seconds(4), source_signal("src/b.ts"));
    assert_eq!(
        coordinator.finish_enrichment_success(enrich_a),
        FinishState::Superseded
    );
    assert_eq!(
        coordinator.checker_dirty_source_paths,
        BTreeSet::from(["src/a.ts".to_string(), "src/b.ts".to_string()])
    );

    let refresh_b = coordinator.next_work(seconds(6)).expect("refresh B");
    assert_eq!(coordinator.finish_refresh(refresh_b), FinishState::Continue);
    let enrich_b = coordinator.next_work(seconds(6)).expect("enrich B");
    assert_eq!(
        coordinator.finish_enrichment_success(enrich_b),
        FinishState::Complete
    );
    assert!(coordinator.checker_dirty_source_paths.is_empty());
}

#[test]
fn checker_dirty_paths_survive_partial_and_retryable_enrichment() {
    let mut coordinator = Coordinator::new(seconds(2), false, true);
    let startup = coordinator.next_work(Duration::ZERO).expect("startup");
    assert_eq!(coordinator.finish_refresh(startup), FinishState::Continue);
    let startup_enrich = coordinator.next_work(Duration::ZERO).expect("enrich");
    assert_eq!(
        coordinator.finish_enrichment_success(startup_enrich),
        FinishState::Complete
    );

    coordinator.mark_dirty(seconds(1), source_signal("src/partial.ts"));
    let refresh = coordinator.next_work(seconds(3)).expect("refresh");
    assert_eq!(coordinator.finish_refresh(refresh), FinishState::Continue);
    let enrich = coordinator.next_work(seconds(3)).expect("enrich");
    assert_eq!(
        coordinator.finish_optional_partial(enrich),
        FinishState::Partial
    );
    assert_eq!(
        coordinator.checker_dirty_source_paths,
        BTreeSet::from(["src/partial.ts".to_string()])
    );

    coordinator.mark_dirty(seconds(4), source_signal("src/retry.ts"));
    let refresh = coordinator.next_work(seconds(6)).expect("refresh");
    assert_eq!(coordinator.finish_refresh(refresh), FinishState::Continue);
    let enrich = coordinator.next_work(seconds(6)).expect("enrich");
    assert!(matches!(
        coordinator.finish_error(seconds(6), enrich),
        FinishState::Retry { .. }
    ));
    assert_eq!(
        coordinator.checker_dirty_source_paths,
        BTreeSet::from(["src/partial.ts".to_string(), "src/retry.ts".to_string()])
    );
}

#[test]
fn documentation_and_disabled_enrichment_never_grow_checker_dirty_paths() {
    let mut enriched = Coordinator::new(seconds(2), false, true);
    let startup = enriched.next_work(Duration::ZERO).expect("startup");
    assert_eq!(enriched.finish_refresh(startup), FinishState::Continue);
    let startup_enrich = enriched.next_work(Duration::ZERO).expect("enrich");
    assert_eq!(
        enriched.finish_enrichment_success(startup_enrich),
        FinishState::Complete
    );
    enriched.mark_dirty(
        seconds(1),
        DirtySignal::documentation("documentation:README.md"),
    );
    assert!(enriched.checker_dirty_source_paths.is_empty());

    let mut structural_only = Coordinator::new(seconds(2), false, false);
    structural_only.mark_dirty(seconds(1), source_signal("src/main.ts"));
    assert!(structural_only.checker_dirty_source_paths.is_empty());
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
    assert_eq!(coordinator.finish_refresh(startup), FinishState::Superseded);
}

#[test]
fn a_full_refresh_signal_is_sticky_within_the_generation() {
    let mut coordinator = Coordinator::new(seconds(2), false, false);
    let startup = coordinator.next_work(Duration::ZERO).expect("startup");
    assert_eq!(coordinator.finish_refresh(startup), FinishState::Complete);

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
    assert_eq!(coordinator.finish_refresh(startup), FinishState::Complete);

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
    let classifier = EventClassifier::new(&root, &database, &Default::default()).unwrap();
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
fn git_controls_cover_symbolic_head_history_and_shallow_state() -> Result<()> {
    let repository = tempfile::tempdir()?;
    fs::create_dir_all(repository.path().join(".git/logs/refs/heads"))?;
    fs::create_dir_all(repository.path().join(".git/refs/heads"))?;
    fs::write(
        repository.path().join(".git/HEAD"),
        "ref: refs/heads/main\n",
    )?;
    let canonical_root = repository.path().canonicalize()?;
    let root = canonical_root.as_path();

    let controls = git_control_paths(root);
    for relative in [
        ".git/HEAD",
        ".git/config",
        ".git/config.worktree",
        ".git/index",
        ".git/info/attributes",
        ".git/logs/HEAD",
        ".git/refs/heads/main",
        ".git/logs/refs/heads/main",
        ".git/packed-refs",
        ".git/shallow",
    ] {
        assert!(
            controls.contains(&root.join(relative)),
            "missing Git provenance control {relative}"
        );
    }

    let classifier = EventClassifier::new(root, &root.join("watch.db"), &Default::default())?;
    for relative in [
        ".git/index",
        ".git/config",
        ".git/config.worktree",
        ".git/info/attributes",
        ".git/refs/heads/main",
        ".git/logs/HEAD",
        ".git/shallow",
    ] {
        let signal = classifier
            .classify(&[root.join(relative)])
            .expect("Git provenance metadata must schedule a refresh");
        assert_eq!(signal.scope, RefreshScope::Full);
    }

    let nested = root.join("packages/app");
    fs::create_dir_all(&nested)?;
    let nested_controls = git_control_paths(&nested);
    assert!(nested_controls.contains(&root.join(".git/HEAD")));
    assert!(nested_controls.contains(&root.join(".git/logs/HEAD")));
    let nested_classifier =
        EventClassifier::new(&nested, &nested.join("watch.db"), &Default::default())?;
    let signal = nested_classifier
        .classify(&[root.join(".git/refs/heads/main")])
        .expect("parent Git metadata must schedule a nested-root refresh");
    assert_eq!(signal.scope, RefreshScope::Full);
    Ok(())
}

#[test]
fn disabled_docs_freshness_gates_only_provenance_git_controls() -> Result<()> {
    let repository = tempfile::tempdir()?;
    fs::create_dir_all(repository.path().join(".git/logs"))?;
    fs::write(repository.path().join(".git/HEAD"), "detached\n")?;
    fs::write(repository.path().join(".gitattributes"), "*.md text\n")?;
    fs::write(repository.path().join(".gitignore"), "dist/\n")?;
    fs::write(repository.path().join(".gitmodules"), "")?;
    let root = repository.path().canonicalize()?;

    let disabled = active_git_control_paths(&root, false);
    let enabled = active_git_control_paths(&root, true);

    assert!(disabled.contains(&root.join(".gitmodules")));
    assert!(disabled.contains(&root.join(".git/HEAD")));
    assert!(!disabled.contains(&root.join(".git/index")));
    assert!(!disabled.contains(&root.join(".git/logs/HEAD")));
    assert!(enabled.is_superset(&disabled));
    assert!(enabled.contains(&root.join(".git/index")));
    assert!(enabled.contains(&root.join(".git/logs/HEAD")));

    let disabled_classifier = EventClassifier::new_with_docs_freshness(
        &root,
        &root.join("disabled.db"),
        &Default::default(),
        false,
    )?;
    assert!(
        disabled_classifier
            .classify(&[root.join(".gitattributes")])
            .is_none()
    );
    for boundary in [".gitignore", ".gitmodules"] {
        assert!(
            disabled_classifier
                .classify(&[root.join(boundary)])
                .is_some_and(|signal| signal.scope == RefreshScope::Full),
            "disabled provenance must retain structural boundary {boundary}"
        );
    }
    let enabled_classifier = EventClassifier::new_with_docs_freshness(
        &root,
        &root.join("enabled.db"),
        &Default::default(),
        true,
    )?;
    assert!(
        enabled_classifier
            .classify(&[root.join(".gitattributes")])
            .is_some_and(|signal| signal.scope == RefreshScope::Full)
    );

    let nested = root.join("packages/app");
    fs::create_dir_all(&nested)?;
    let disabled_nested = EventClassifier::new_with_docs_freshness(
        &nested,
        &nested.join("disabled.db"),
        &Default::default(),
        false,
    )?;
    assert!(
        disabled_nested
            .classify(&[root.join(".gitattributes")])
            .is_none()
    );
    let enabled_nested = EventClassifier::new_with_docs_freshness(
        &nested,
        &nested.join("enabled.db"),
        &Default::default(),
        true,
    )?;
    assert!(
        enabled_nested
            .classify(&[root.join(".gitattributes")])
            .is_some_and(|signal| signal.scope == RefreshScope::Full)
    );
    Ok(())
}

#[test]
fn git_controls_cover_the_repository_index() -> Result<()> {
    let repository = tempfile::tempdir()?;
    git(repository.path(), &["init"])?;
    fs::write(repository.path().join("README.md"), "tracked\n")?;
    git(repository.path(), &["add", "README.md"])?;
    let root = repository.path().canonicalize()?;
    let index = git_path(&root, &["rev-parse", "--git-path", "index"])
        .expect("Git must resolve its index path");

    assert!(git_control_paths(&root).contains(&index));
    let classifier = EventClassifier::new(&root, &root.join("watch.db"), &Default::default())?;
    let signal = classifier
        .classify(std::slice::from_ref(&index))
        .expect("index membership change must schedule a refresh");
    assert_eq!(signal.scope, RefreshScope::Full);
    assert!(signal.reasons.contains(&format!(
        "git:{}",
        index.strip_prefix(&root).unwrap_or(&index).display()
    )));
    Ok(())
}

#[test]
fn nested_root_watches_ancestor_attributes_and_repository_conversion_config() -> Result<()> {
    let repository = tempfile::tempdir()?;
    git(repository.path(), &["init"])?;
    let worktree_root = repository.path().canonicalize()?;
    let nested = worktree_root.join("packages/app");
    fs::create_dir_all(&nested)?;
    let root_attributes = worktree_root.join(".gitattributes");
    let package_attributes = worktree_root.join("packages/.gitattributes");
    let nested_attributes = nested.join(".gitattributes");
    let root_gitmodules = worktree_root.join(".gitmodules");
    let config = git_path(&nested, &["rev-parse", "--git-path", "config"])
        .expect("Git must resolve repository config");
    let worktree_config = git_path(&nested, &["rev-parse", "--git-path", "config.worktree"])
        .expect("Git must resolve worktree config");
    let controls = git_control_paths(&nested);

    for path in [
        &root_attributes,
        &package_attributes,
        &nested_attributes,
        &config,
        &worktree_config,
        &root_gitmodules,
    ] {
        assert!(
            controls.contains(path),
            "missing control {}",
            path.display()
        );
    }

    let structural_controls = active_git_control_paths(&nested, false);
    assert!(structural_controls.contains(&root_gitmodules));
    let structural_classifier = EventClassifier::new_with_docs_freshness(
        &nested,
        &nested.join("structural-watch.db"),
        &Default::default(),
        false,
    )?;
    assert!(
        structural_classifier
            .classify(std::slice::from_ref(&root_gitmodules))
            .is_some_and(|signal| signal.scope == RefreshScope::Full)
    );
    let structural_target = active_git_watch_targets(&nested, false)
        .into_iter()
        .find(|target| target.path == root_gitmodules)
        .expect("worktree-root .gitmodules needs an external exact target");
    assert_eq!(structural_target.kind, TargetKind::Exact);
    assert_eq!(structural_target.source, TargetSource::Git);
    assert_eq!(structural_target.watch_path, worktree_root);
    let classifier = EventClassifier::new(&nested, &nested.join("watch.db"), &Default::default())?;
    for path in [
        &root_attributes,
        &package_attributes,
        &config,
        &worktree_config,
    ] {
        assert!(
            classifier
                .classify(std::slice::from_ref(path))
                .is_some_and(|signal| signal.scope == RefreshScope::Full),
            "control must trigger a full refresh: {}",
            path.display()
        );
    }
    let targets = git_watch_targets(&nested);
    let target = targets
        .iter()
        .find(|target| target.path == root_attributes)
        .expect("worktree-root attributes need an external exact target");
    assert_eq!(target.kind, TargetKind::Exact);
    assert_eq!(target.source, TargetSource::Git);
    assert_eq!(target.watch_path, worktree_root);
    Ok(())
}

#[test]
fn git_controls_cover_the_linked_worktree_index() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let repository = fixture.path().join("repository");
    fs::create_dir(&repository)?;
    git(&repository, &["init"])?;
    git(&repository, &["config", "user.name", "jscout tests"])?;
    git(
        &repository,
        &["config", "user.email", "jscout-tests@example.invalid"],
    )?;
    fs::write(repository.join("README.md"), "tracked\n")?;
    git(&repository, &["add", "README.md"])?;
    git(&repository, &["commit", "-m", "initial"])?;

    let linked = fixture.path().join("linked");
    git(
        &repository,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().expect("temporary path is UTF-8"),
        ],
    )?;
    let linked = linked.canonicalize()?;
    let index = git_path(&linked, &["rev-parse", "--git-path", "index"])
        .expect("Git must resolve the linked worktree index path");
    let gitfile = linked.join(".git");
    let nested = linked.join("packages/app");
    fs::create_dir_all(&nested)?;

    assert!(index.is_file());
    assert!(gitfile.is_file());
    assert!(!index.starts_with(&linked));
    assert!(git_control_paths(&linked).contains(&index));
    let classifier = EventClassifier::new(&linked, &linked.join("watch.db"), &Default::default())?;
    assert!(
        classifier
            .classify(&[index])
            .is_some_and(|signal| signal.scope == RefreshScope::Full)
    );
    for (indexed_root, label) in [(&linked, "worktree root"), (&nested, "nested root")] {
        for freshness in [false, true] {
            assert!(
                active_git_control_paths(indexed_root, freshness).contains(&gitfile),
                "{label} must retain its gitfile when freshness={freshness}"
            );
            let target = active_git_watch_targets(indexed_root, freshness)
                .into_iter()
                .find(|target| target.path == gitfile)
                .expect("linked-worktree gitfile needs an exact watch target");
            assert_eq!(target.kind, TargetKind::Exact);
            assert_eq!(target.source, TargetSource::Git);
            assert_eq!(target.watch_path, linked);

            let classifier = EventClassifier::new_with_docs_freshness(
                indexed_root,
                &indexed_root.join("gitfile-watch.db"),
                &Default::default(),
                freshness,
            )?;
            assert!(
                classifier
                    .classify(std::slice::from_ref(&gitfile))
                    .is_some_and(|signal| signal.scope == RefreshScope::Full),
                "{label} gitfile must force a full refresh when freshness={freshness}"
            );
        }
    }

    let nested_repository = linked.join("nested-repository");
    fs::create_dir(&nested_repository)?;
    git(&nested_repository, &["init"])?;
    let nested_repository_controls = active_git_control_paths(&nested_repository, false);
    assert!(nested_repository_controls.contains(&nested_repository.join(".git/HEAD")));
    assert!(
        !nested_repository_controls.contains(&gitfile),
        "a nearer .git directory must stop discovery of the outer worktree gitfile"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn git_controls_resolve_a_non_utf8_linked_worktree_pointer() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let repository = fixture
        .path()
        .join(OsString::from_vec(b"repository-\xff".to_vec()));
    if let Err(error) = fs::create_dir(&repository) {
        if matches!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::InvalidInput
        ) || error.raw_os_error() == Some(libc::EILSEQ)
        {
            // Some Unix filesystems (notably the default macOS filesystem)
            // reject non-UTF8 names before Git can exercise this path.
            return Ok(());
        }
        return Err(error.into());
    }
    git(&repository, &["init"])?;
    git(&repository, &["config", "user.name", "jscout tests"])?;
    git(
        &repository,
        &["config", "user.email", "jscout-tests@example.invalid"],
    )?;
    git(&repository, &["commit", "--allow-empty", "-m", "initial"])?;

    let linked = fixture
        .path()
        .join(OsString::from_vec(b"linked-\xfe".to_vec()));
    let add = Command::new("git")
        .arg("-C")
        .arg(&repository)
        .args(["worktree", "add", "--detach"])
        .arg(&linked)
        .output()?;
    if !add.status.success()
        && String::from_utf8_lossy(&add.stderr).contains("Operation not permitted")
    {
        return Ok(());
    }
    anyhow::ensure!(add.status.success(), "git worktree add failed");
    let linked = linked.canonicalize()?;
    assert!(fs::read_to_string(linked.join(".git")).is_err());

    let git_dir = git_path(&linked, &["rev-parse", "--absolute-git-dir"])
        .expect("Git must return the raw worktree Git directory");
    let common_dir = git_path(&linked, &["rev-parse", "--git-common-dir"])
        .expect("Git must return the raw common Git directory");
    let index = git_path(&linked, &["rev-parse", "--git-path", "index"])
        .expect("Git must return the raw index path");
    let controls = git_control_paths(&linked);

    assert!(controls.contains(&git_dir.join("HEAD")));
    assert!(controls.contains(&common_dir.join("packed-refs")));
    assert!(controls.contains(&index));
    let classifier = EventClassifier::new(&linked, &linked.join("watch.db"), &Default::default())?;
    assert!(
        classifier
            .classify(&[index])
            .is_some_and(|signal| signal.scope == RefreshScope::Full)
    );
    Ok(())
}

#[test]
fn git_controls_cover_both_linked_worktree_reftable_manifests() -> Result<()> {
    let fixture = tempfile::tempdir()?;
    let repository = fixture.path().join("repository");
    fs::create_dir(&repository)?;
    let init = Command::new("git")
        .arg("-C")
        .arg(&repository)
        .args(["init", "--ref-format=reftable"])
        .output()?;
    if !init.status.success() {
        // Reftable is optional in older Git builds.
        return Ok(());
    }
    git(&repository, &["config", "user.name", "jscout tests"])?;
    git(
        &repository,
        &["config", "user.email", "jscout-tests@example.invalid"],
    )?;
    git(&repository, &["commit", "--allow-empty", "-m", "initial"])?;

    let linked = fixture.path().join("linked");
    git(
        &repository,
        &[
            "worktree",
            "add",
            "--detach",
            linked.to_str().expect("temporary path is UTF-8"),
        ],
    )?;
    let linked = linked.canonicalize()?;
    let git_dir = git_path(&linked, &["rev-parse", "--absolute-git-dir"])
        .expect("Git must resolve the worktree Git directory");
    let common_dir = git_path(
        &linked,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .expect("Git must resolve the common Git directory");
    let worktree_manifest = git_dir.join("reftable/tables.list");
    let common_manifest = common_dir.join("reftable/tables.list");
    let controls = git_control_paths(&linked);

    assert_ne!(worktree_manifest, common_manifest);
    assert!(controls.contains(&worktree_manifest));
    assert!(controls.contains(&common_manifest));
    let classifier = EventClassifier::new(&linked, &linked.join("watch.db"), &Default::default())?;
    for manifest in [worktree_manifest, common_manifest] {
        assert!(
            classifier
                .classify(&[manifest])
                .is_some_and(|signal| signal.scope == RefreshScope::Full)
        );
    }
    Ok(())
}

#[test]
fn selected_external_prefix_overrides_node_modules_noise() {
    let root = PathBuf::from("/repo");
    let dependency = root.join("node_modules/pkg");
    let mut classifier =
        EventClassifier::new(&root, &root.join(".jscout.db"), &Default::default()).unwrap();
    classifier.set_external(
        Default::default(),
        Default::default(),
        [dependency.clone()].into(),
    );
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
    let classifier =
        EventClassifier::new(&root, &root.join(".jscout.db"), &Default::default()).unwrap();

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
    assert!(is_refresh_boundary(Path::new(".gitattributes")));
    assert!(is_refresh_boundary(Path::new(".ignore")));

    let root = PathBuf::from("/repo");
    let classifier =
        EventClassifier::new(&root, &root.join(".jscout.db"), &Default::default()).unwrap();
    for boundary in [
        ".gitignore",
        ".gitattributes",
        ".ignore",
        "pnpm-workspace.yaml",
    ] {
        assert!(
            classifier
                .classify(&[root.join(boundary)])
                .is_some_and(|signal| signal.scope == RefreshScope::Full),
            "{boundary} must force a full refresh"
        );
    }
}

#[test]
fn event_filter_uses_walker_ignore_policy_without_excluding_authored_build_dirs() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let root = repo.path();
    fs::create_dir_all(root.join(".git"))?;
    fs::write(root.join(".gitignore"), "/build/\n")?;
    fs::create_dir_all(root.join("build"))?;
    fs::create_dir_all(root.join("src/build"))?;
    fs::create_dir_all(root.join("node_modules/dep"))?;
    fs::write(
        root.join("build/generated.ts"),
        "export const generated = 1;\n",
    )?;
    fs::write(
        root.join("src/build/plugin.ts"),
        "export const plugin = 1;\n",
    )?;
    fs::write(
        root.join("node_modules/dep/index.js"),
        "module.exports = 1;\n",
    )?;
    let classifier =
        EventClassifier::new(root, &root.join("watch.db"), &Default::default()).unwrap();

    assert!(
        classifier
            .classify(&[root.join("build/generated.ts")])
            .is_none()
    );
    assert_eq!(
        classifier.classify(&[root.join("src/build/plugin.ts")]),
        Some(DirtySignal::source(
            "source:src/build/plugin.ts",
            "src/build/plugin.ts"
        ))
    );
    assert!(
        classifier
            .classify(&[root.join("node_modules/dep/index.js")])
            .is_none()
    );
    Ok(())
}

#[test]
fn event_filter_reloads_ignore_rules_after_a_refresh() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let root = repo.path();
    fs::create_dir_all(root.join(".git"))?;
    fs::create_dir_all(root.join("generated"))?;
    fs::write(
        root.join("generated/output.ts"),
        "export const output = 1;\n",
    )?;
    let mut classifier =
        EventClassifier::new(root, &root.join("watch.db"), &Default::default()).unwrap();

    assert!(
        classifier
            .classify(&[root.join("generated/output.ts")])
            .is_some()
    );
    fs::write(root.join(".gitignore"), "/generated/\n")?;
    classifier.reload_path_policies()?;
    assert!(
        classifier
            .classify(&[root.join("generated/output.ts")])
            .is_none()
    );
    Ok(())
}

#[test]
fn event_filter_applies_the_configured_documentation_policy() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let root = repo.path();
    fs::create_dir(root.join(".git"))?;
    fs::create_dir_all(root.join("docs"))?;
    fs::create_dir_all(root.join("private"))?;
    fs::create_dir_all(root.join(".github"))?;
    fs::create_dir_all(root.join(".github/.private"))?;
    fs::create_dir_all(root.join(".claude"))?;
    fs::create_dir_all(root.join(".agents"))?;
    fs::create_dir_all(root.join(".hidden"))?;
    fs::create_dir_all(root.join(".source-visible"))?;
    fs::create_dir_all(root.join("packages/app/.github"))?;
    fs::write(
        root.join(".gitignore"),
        "/docs/\n/.claude/\n!.source-visible/\n",
    )?;
    fs::write(root.join("docs/ignored.md"), "ignored\n")?;
    fs::write(root.join("private/excluded.md"), "excluded\n")?;
    fs::write(root.join(".github/guide.mdx"), "visible\n")?;
    fs::write(root.join(".hidden/secret.md"), "hidden\n")?;
    let documentation = crate::docs::corpus::CorpusOptions {
        exclude: vec!["private/**".to_string()],
        ..Default::default()
    };
    let mut classifier = EventClassifier::new(root, &root.join("watch.db"), &documentation)?;

    assert!(
        classifier
            .classify(&[root.join("docs/ignored.md")])
            .is_none()
    );
    assert!(
        classifier
            .classify(&[root.join("private/excluded.md")])
            .is_none()
    );
    assert!(
        classifier
            .classify(&[root.join(".hidden/secret.md")])
            .is_none()
    );
    assert_eq!(
        classifier.classify(&[root.join(".github/guide.mdx")]),
        Some(DirtySignal::documentation(
            "documentation:.github/guide.mdx"
        ))
    );
    assert_eq!(
        classifier.classify(&[root.join(".github")]),
        Some(DirtySignal::documentation(
            "documentation-directory:.github"
        ))
    );
    assert_eq!(
        classifier.classify(&[root.join(".agents")]),
        Some(DirtySignal::documentation(
            "documentation-directory:.agents"
        ))
    );
    assert!(classifier.classify(&[root.join(".claude")]).is_none());
    assert!(
        classifier
            .classify(&[root.join(".github/.private")])
            .is_none()
    );
    assert!(
        classifier
            .classify(&[root.join("packages/app/.github")])
            .is_none()
    );
    assert_eq!(
        classifier.classify(&[root.join(".source-visible")]),
        Some(DirtySignal::inventory("inventory:directory-event"))
    );
    fs::remove_dir_all(root.join(".github"))?;
    fs::remove_dir_all(root.join(".agents"))?;
    fs::remove_dir_all(root.join(".claude"))?;
    fs::remove_dir_all(root.join(".hidden"))?;
    fs::remove_dir_all(root.join(".source-visible"))?;
    assert_eq!(
        classifier.classify(&[root.join(".github")]),
        Some(DirtySignal::documentation(
            "documentation-directory:.github"
        ))
    );
    assert_eq!(
        classifier.classify(&[root.join(".agents")]),
        Some(DirtySignal::documentation(
            "documentation-directory:.agents"
        ))
    );
    assert!(classifier.classify(&[root.join(".claude")]).is_none());
    assert!(classifier.classify(&[root.join(".hidden")]).is_none());
    assert_eq!(
        classifier.classify(&[root.join(".source-visible")]),
        Some(DirtySignal::inventory("inventory:directory-event"))
    );

    let disabled = crate::docs::corpus::CorpusOptions {
        include: Vec::new(),
        ..Default::default()
    };
    let disabled_classifier = EventClassifier::new(root, &root.join("disabled.db"), &disabled)?;
    assert!(
        disabled_classifier
            .classify(&[root.join(".github")])
            .is_none()
    );

    fs::write(root.join(".gitignore"), "")?;
    classifier.reload_path_policies()?;
    assert_eq!(
        classifier.classify(&[root.join(".claude")]),
        Some(DirtySignal::documentation(
            "documentation-directory:.claude"
        ))
    );
    assert_eq!(
        classifier.classify(&[root.join("docs/deleted.md")]),
        Some(DirtySignal::documentation("documentation:docs/deleted.md"))
    );
    Ok(())
}

#[test]
fn irrelevant_regular_files_are_ignored_but_inventory_shapes_refresh_incrementally() -> Result<()> {
    let root = tempfile::tempdir()?;
    fs::write(root.path().join("README.md"), "documentation\n")?;
    fs::write(root.path().join(".DS_Store"), "metadata\n")?;
    fs::create_dir(root.path().join("renamed-directory"))?;
    fs::create_dir(root.path().join(".git"))?;
    fs::write(root.path().join(".git/index"), "git metadata\n")?;
    let classifier = EventClassifier::new(
        root.path(),
        &root.path().join("watch.db"),
        &Default::default(),
    )?;

    assert_eq!(
        classifier.classify(&[root.path().join("README.md")]),
        Some(DirtySignal::documentation("documentation:README.md"))
    );
    assert!(
        classifier
            .classify(&[root.path().join(".DS_Store")])
            .is_none()
    );
    assert!(
        classifier
            .classify(&[root.path().join(".git/index").canonicalize()?])
            .is_some_and(|signal| signal.scope == RefreshScope::Full)
    );
    assert_eq!(
        classifier.classify(&[root.path().join("renamed-directory")]),
        Some(DirtySignal::inventory("inventory:directory-event"))
    );
    assert_eq!(
        classifier.classify(&[root.path().join("deleted-unknown-file")]),
        Some(DirtySignal::inventory("inventory:unknown-event"))
    );

    fs::create_dir_all(root.path().join("docs/nested"))?;
    fs::write(root.path().join("docs/nested/guide.md"), "guide\n")?;
    fs::remove_dir_all(root.path().join("docs"))?;
    assert_eq!(
        classifier.classify(&[root.path().join("docs")]),
        Some(DirtySignal::inventory("inventory:unknown-event"))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_config_watches_lexical_and_external_resolved_paths() -> Result<()> {
    let repository = tempfile::tempdir()?;
    let external = tempfile::tempdir()?;
    let target = external.path().join("repository-config.toml");
    fs::write(&target, "version = 1\n")?;
    let target = target.canonicalize()?;

    let default_link = repository.path().join(crate::config::FILE_NAME);
    std::os::unix::fs::symlink(&target, &default_link)?;
    let default_paths = config_watch_paths(&default_link);
    assert!(default_paths.contains(&default_link));
    assert!(default_paths.contains(&target));

    let explicit_link = external.path().join("explicit-config.toml");
    std::os::unix::fs::symlink(&target, &explicit_link)?;
    let explicit_paths = config_watch_paths(&explicit_link);
    assert!(explicit_paths.contains(&explicit_link));
    assert!(explicit_paths.contains(&target));

    let root = repository.path().canonicalize()?;
    let docs_include = crate::docs::default_include_globs();
    let options = WatchOptions {
        database: None,
        embed_on_change: false,
        provider: None,
        embed_product_only: false,
        dependencies: &[],
        docs_include: &docs_include,
        docs_exclude: &[],
        docs_freshness: false,
        enrich_on_change: false,
        enrich_timeout: seconds(300),
        checker_sidecar: None,
        checker_node: "node",
        timing: false,
        debug: false,
        debounce: seconds(2),
        reconcile_interval: seconds(600),
        config_fingerprint: "symlink-config",
        config_loaded: true,
        config_path: Some(&default_link),
        config_explicit: false,
    };
    let mut classifier = EventClassifier::new_with_docs_freshness(
        &root,
        &root.join("watch.db"),
        &Default::default(),
        false,
    )?;
    let mut targets = Vec::new();
    extend_configured_watch_targets(&mut targets, &root, &options);
    normalize_targets(&mut targets);
    update_classifier_targets(&mut classifier, &targets);

    for config_event in [&default_link, &target] {
        assert!(
            classifier
                .classify(std::slice::from_ref(config_event))
                .is_some_and(|signal| signal.scope == RefreshScope::Incremental),
            "config event was not recognized at {}",
            config_event.display()
        );
    }
    assert!(targets.iter().any(|watch_target| {
        watch_target.source == TargetSource::Config
            && watch_target.path == target
            && !watch_target.watch_path.starts_with(&root)
    }));

    let repointed_target = external.path().join("repointed-config.toml");
    fs::write(&repointed_target, "version = 1\n")?;
    let repointed_target = repointed_target.canonicalize()?;
    fs::remove_file(&default_link)?;
    std::os::unix::fs::symlink(&repointed_target, &default_link)?;
    extend_config_watch_targets(&mut targets, &options);
    normalize_targets(&mut targets);
    update_classifier_targets(&mut classifier, &targets);

    assert!(
        targets
            .iter()
            .any(|watch_target| watch_target.path == repointed_target)
    );
    assert!(
        targets
            .iter()
            .any(|watch_target| watch_target.path == target),
        "the prior target remains covered until successful refresh reconciliation"
    );
    assert!(
        classifier
            .classify(std::slice::from_ref(&repointed_target))
            .is_some_and(|signal| signal.scope == RefreshScope::Incremental)
    );
    Ok(())
}

#[test]
fn config_reload_toggles_docs_provenance_and_forces_a_full_refresh() -> Result<()> {
    let repository = tempfile::tempdir()?;
    fs::create_dir_all(repository.path().join(".git/logs"))?;
    fs::write(repository.path().join(".git/HEAD"), "detached\n")?;
    fs::write(repository.path().join(".gitattributes"), "*.md text\n")?;
    let root = repository.path().canonicalize()?;
    let config_path = root.join(crate::config::FILE_NAME);
    fs::write(
        &config_path,
        "version = 1\n\n[docs.search]\nfreshness = false\n",
    )?;
    let docs_include = crate::docs::default_include_globs();
    let options = WatchOptions {
        database: None,
        embed_on_change: false,
        provider: None,
        embed_product_only: false,
        dependencies: &[],
        docs_include: &docs_include,
        docs_exclude: &[],
        docs_freshness: false,
        enrich_on_change: false,
        enrich_timeout: seconds(300),
        checker_sidecar: None,
        checker_node: "node",
        timing: false,
        debug: false,
        debounce: seconds(2),
        reconcile_interval: seconds(600),
        config_fingerprint: "startup-config",
        config_loaded: true,
        config_path: Some(&config_path),
        config_explicit: false,
    };
    let mut policy = DocsIndexingPolicy::from_options(&options);
    let mut classifier = EventClassifier::new_with_docs_freshness(
        &root,
        &root.join("watch.db"),
        &policy.corpus_options(),
        policy.freshness,
    )?;
    let mut targets = Vec::new();
    extend_configured_watch_targets(&mut targets, &root, &options);
    update_classifier_targets(&mut classifier, &targets);
    assert!(
        classifier
            .classify(std::slice::from_ref(&config_path))
            .is_some_and(|signal| signal.scope == RefreshScope::Incremental)
    );

    fs::write(
        &config_path,
        "version = 1\n\n[docs.search]\nfreshness = false\n\n[watch]\ndebounce_ms = 3000\n",
    )?;
    let (unrelated_edit, scope) = prepare_refresh(
        &root,
        &options,
        &policy,
        &mut classifier,
        RefreshScope::Incremental,
    )?;
    assert_eq!(scope, RefreshScope::Incremental);
    assert_eq!(unrelated_edit, policy);

    // Restart-only settings are outside the hot-reload semantic boundary.
    // Their invalid values must not poison documentation refresh retries.
    fs::write(
        &config_path,
        "version = 1\n\n[docs.search]\nfreshness = false\nmax_rank_movement = 0\nlimit = 0\n\n[watch]\ndebounce_ms = 0\n",
    )?;
    let (invalid_restart_only, scope) = prepare_refresh(
        &root,
        &options,
        &policy,
        &mut classifier,
        RefreshScope::Incremental,
    )?;
    assert_eq!(scope, RefreshScope::Incremental);
    assert_eq!(invalid_restart_only, policy);

    fs::write(
        &config_path,
        "version = 1\n\n[docs.search]\nfreshness = true\n",
    )?;
    let (enabled, scope) = prepare_refresh(
        &root,
        &options,
        &policy,
        &mut classifier,
        RefreshScope::Incremental,
    )?;
    assert_eq!(scope, RefreshScope::Full);
    assert!(enabled.freshness);
    assert!(enabled.index_options(&options).docs_freshness);
    assert!(
        classifier
            .classify(&[root.join(".git/logs/HEAD")])
            .is_some_and(|signal| signal.scope == RefreshScope::Full)
    );
    assert!(
        classifier
            .classify(&[root.join(".gitattributes")])
            .is_some_and(|signal| signal.scope == RefreshScope::Full)
    );

    // A failed publication does not advance the in-memory published policy;
    // retrying the same incremental work must retain the Full promotion.
    let (retry_enabled, retry_scope) = prepare_refresh(
        &root,
        &options,
        &policy,
        &mut classifier,
        RefreshScope::Incremental,
    )?;
    assert_eq!(retry_scope, RefreshScope::Full);
    assert_eq!(retry_enabled, enabled);

    policy = retry_enabled;
    fs::write(
        &config_path,
        "version = 1\n\n[docs.search]\nfreshness = false\n",
    )?;
    let (disabled, scope) = prepare_refresh(
        &root,
        &options,
        &policy,
        &mut classifier,
        RefreshScope::Incremental,
    )?;
    assert_eq!(scope, RefreshScope::Full);
    assert!(!disabled.freshness);
    assert!(
        classifier
            .classify(&[root.join(".git/logs/HEAD")])
            .is_none()
    );
    assert!(
        classifier
            .classify(&[root.join(".gitattributes")])
            .is_none()
    );
    assert!(
        classifier
            .classify(&[root.join(".git/HEAD")])
            .is_some_and(|signal| signal.scope == RefreshScope::Full)
    );

    fs::write(
        &config_path,
        "version = 1\n\n[docs]\ninclude = [\"handbook/**/*.md\"]\nexclude = [\"handbook/private/**\"]\n\n[docs.search]\nfreshness = false\n",
    )?;
    let (reselected, scope) = prepare_refresh(
        &root,
        &options,
        &disabled,
        &mut classifier,
        RefreshScope::Incremental,
    )?;
    assert_eq!(scope, RefreshScope::Full);
    assert_eq!(reselected.include, ["handbook/**/*.md"]);
    assert_eq!(reselected.exclude, ["handbook/private/**"]);

    fs::write(
        &config_path,
        "version = 1\n\n[docs]\nenabled = false\n\n[docs.search]\nfreshness = true\n",
    )?;
    let (docs_disabled, scope) = prepare_refresh(
        &root,
        &options,
        &reselected,
        &mut classifier,
        RefreshScope::Incremental,
    )?;
    assert_eq!(scope, RefreshScope::Full);
    assert!(docs_disabled.include.is_empty());
    assert!(docs_disabled.exclude.is_empty());
    assert!(!docs_disabled.freshness);

    fs::write(
        &config_path,
        "version = 1\n\n[docs]\nenabled = false\n\n[docs.search]\nfreshness = false\n",
    )?;
    let (disabled_noop, scope) = prepare_refresh(
        &root,
        &options,
        &docs_disabled,
        &mut classifier,
        RefreshScope::Incremental,
    )?;
    assert_eq!(scope, RefreshScope::Incremental);
    assert_eq!(disabled_noop, docs_disabled);

    fs::write(
        &config_path,
        "version = 1\n\n[docs]\ninclude = [\"!private/**\"]\n",
    )?;
    let policy_error = prepare_refresh(
        &root,
        &options,
        &docs_disabled,
        &mut classifier,
        RefreshScope::Incremental,
    )
    .expect_err("invalid hot-reloaded documentation policy must retry");
    assert!(
        policy_error
            .to_string()
            .contains("validate documentation include/exclude patterns")
    );

    fs::write(&config_path, "version =")?;
    let error = prepare_refresh(
        &root,
        &options,
        &docs_disabled,
        &mut classifier,
        RefreshScope::Incremental,
    )
    .expect_err("invalid configuration must fail refresh preflight");
    assert!(error.to_string().contains("parse configuration"));
    assert!(
        classifier
            .classify(&[root.join(".git/logs/HEAD")])
            .is_none()
    );
    Ok(())
}

#[test]
fn reconciliation_interval_must_exceed_debounce() {
    let options = WatchOptions {
        database: None,
        embed_on_change: false,
        provider: None,
        embed_product_only: false,
        dependencies: &[],
        docs_include: &[],
        docs_exclude: &[],
        docs_freshness: false,
        enrich_on_change: false,
        enrich_timeout: seconds(300),
        checker_sidecar: None,
        checker_node: "node",
        timing: false,
        debug: false,
        debounce: seconds(2),
        reconcile_interval: seconds(2),
        config_fingerprint: "config-test",
        config_loaded: true,
        config_path: None,
        config_explicit: false,
    };
    let error = validate_options(&options).expect_err("invalid interval");
    assert!(error.to_string().contains("must exceed"));
}

#[test]
fn product_embedding_requires_embedding_phase() {
    let options = WatchOptions {
        database: None,
        embed_on_change: false,
        provider: None,
        embed_product_only: true,
        dependencies: &[],
        docs_include: &[],
        docs_exclude: &[],
        docs_freshness: false,
        enrich_on_change: false,
        enrich_timeout: seconds(300),
        checker_sidecar: None,
        checker_node: "node",
        timing: false,
        debug: false,
        debounce: seconds(2),
        reconcile_interval: seconds(600),
        config_fingerprint: "config-test",
        config_loaded: false,
        config_path: None,
        config_explicit: false,
    };
    let error = validate_options(&options).expect_err("product needs embedding");
    assert_eq!(error.to_string(), "--product requires --embed");
}

#[test]
fn startup_log_records_runtime_identities_and_effective_watch_flags() {
    let options = WatchOptions {
        database: None,
        embed_on_change: true,
        provider: None,
        embed_product_only: true,
        dependencies: &[],
        docs_include: &[],
        docs_exclude: &[],
        docs_freshness: false,
        enrich_on_change: true,
        enrich_timeout: seconds(300),
        checker_sidecar: None,
        checker_node: "node",
        timing: false,
        debug: false,
        debounce: seconds(2),
        reconcile_interval: seconds(600),
        config_fingerprint: "runtime-config",
        config_loaded: true,
        config_path: None,
        config_explicit: false,
    };
    let line = watch_startup_log(
        Path::new("/repo"),
        Path::new("/repo/.jscout.db"),
        &options,
        "binary-id",
        "checker-policy-id",
        "watch-policy-id",
    );
    for expected in [
        "jscout_version=",
        "binary_fingerprint=binary-id",
        "config_fingerprint=runtime-config",
        "config_loaded=true",
        "config_reload=docs-indexing-only",
        "checker_policy_fingerprint=checker-policy-id",
        "watch_policy_fingerprint=watch-policy-id",
        "debounce_ms=2000",
        "reconcile_seconds=600",
        "embed=true",
        "product=true",
        "enrich=true",
        "docs_freshness=false",
    ] {
        assert!(line.contains(expected), "missing {expected:?} from {line}");
    }
}

#[test]
fn effective_watch_policy_identity_tracks_cli_resolved_overrides() {
    let dependencies = Vec::<String>::new();
    let mut options = WatchOptions {
        database: None,
        embed_on_change: false,
        provider: None,
        embed_product_only: false,
        dependencies: &dependencies,
        docs_include: &[],
        docs_exclude: &[],
        docs_freshness: false,
        enrich_on_change: true,
        enrich_timeout: seconds(300),
        checker_sidecar: None,
        checker_node: "node",
        timing: false,
        debug: false,
        debounce: seconds(2),
        reconcile_interval: seconds(600),
        config_fingerprint: "same-baseline",
        config_loaded: true,
        config_path: None,
        config_explicit: false,
    };
    let baseline = effective_watch_policy_fingerprint(&options);
    assert_eq!(baseline.len(), 64);

    let selected_dependencies = vec!["@scope/runtime".to_string()];
    options.dependencies = &selected_dependencies;
    let dependencies_changed = effective_watch_policy_fingerprint(&options);
    assert_ne!(baseline, dependencies_changed);

    options.enrich_timeout = seconds(301);
    let timeout_changed = effective_watch_policy_fingerprint(&options);
    assert_ne!(dependencies_changed, timeout_changed);

    options.checker_sidecar = Some(Path::new("checker/custom.mjs"));
    let sidecar_changed = effective_watch_policy_fingerprint(&options);
    assert_ne!(timeout_changed, sidecar_changed);

    options.docs_freshness = true;
    assert_ne!(
        sidecar_changed,
        effective_watch_policy_fingerprint(&options)
    );
}

#[test]
fn checker_watch_options_factory_owns_selection_and_generation_overrides() {
    let database = Path::new("/repo/.jscout.db");
    let normal = watch_enrich_options(
        database,
        Some(Path::new("checker/main.mjs")),
        "node",
        seconds(300),
        false,
        vec!["src/a.ts".into()],
    );
    let flush = watch_enrich_options(
        database,
        Some(Path::new("checker/main.mjs")),
        "node",
        seconds(300),
        true,
        vec!["src/b.ts".into()],
    );

    assert!(normal.files.is_empty());
    assert!(normal.packages.is_empty());
    assert!(normal.members.is_empty());
    assert!(normal.roles.is_empty());
    assert_eq!(normal.max_occurrences, None);
    assert!(!normal.include_all);
    assert!(!normal.dry_run);
    assert!(normal.carry_forward);
    assert!(!normal.force_full);
    assert_eq!(normal.dirty_files, ["src/a.ts"]);

    assert!(!flush.carry_forward);
    assert!(flush.force_full);
    assert_eq!(flush.dirty_files, ["src/b.ts"]);
    assert_eq!(
        crate::checker::watch_policy_fingerprint(&normal),
        crate::checker::watch_policy_fingerprint(&flush),
        "per-generation carry-free work must not change the startup policy identity"
    );
}

#[test]
fn refresh_phase_replaces_the_complete_file_set() -> Result<()> {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("a.ts"), "export const a = 1;\n")?;
    let database = directory.path().join("watch.db");
    let first = run_refresh(
        directory.path(),
        &database,
        &indexer::IndexOptions::default(),
        RefreshScope::Full,
    )?;
    assert_eq!(first.outcome.indexed, 1);
    fs::remove_file(directory.path().join("a.ts"))?;
    fs::write(directory.path().join("b.ts"), "export const b = 2;\n")?;
    let second = run_refresh(
        directory.path(),
        &database,
        &indexer::IndexOptions::default(),
        RefreshScope::Incremental,
    )?;
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
fn incremental_inventory_refresh_removes_a_deleted_documentation_subtree() -> Result<()> {
    let directory = tempfile::tempdir()?;
    fs::create_dir_all(directory.path().join("removed/nested"))?;
    fs::write(
        directory.path().join("removed/nested/guide.md"),
        "# Guide\n\nCurrent documentation.\n",
    )?;
    fs::write(
        directory.path().join("removed/main.ts"),
        "export const removed = true;\n",
    )?;
    fs::write(
        directory.path().join("stable.ts"),
        "export const stable = true;\n",
    )?;
    let database = directory.path().join("watch.db");
    run_refresh(
        directory.path(),
        &database,
        &indexer::IndexOptions::default(),
        RefreshScope::Full,
    )?;

    fs::remove_dir_all(directory.path().join("removed"))?;
    let refreshed = run_refresh(
        directory.path(),
        &database,
        &indexer::IndexOptions::default(),
        RefreshScope::Incremental,
    )?;
    assert_eq!(refreshed.outcome.unchanged, 1);
    assert_eq!(refreshed.outcome.removed, 2);
    assert!(!refreshed.outcome.extraction_reset);

    let conn = crate::store::open_path_read_only(&database)?;
    let remaining = conn
        .prepare("SELECT path, corpus FROM files ORDER BY path")?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(remaining, vec![("stable.ts".into(), "code".into())]);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM docs_fts", [], |row| {
            row.get::<_, i64>(0)
        })?,
        0
    );
    Ok(())
}

#[test]
fn hidden_documentation_directory_event_removes_its_subtree_incrementally() -> Result<()> {
    let directory = tempfile::tempdir()?;
    fs::create_dir_all(directory.path().join(".github/guides"))?;
    fs::write(
        directory.path().join(".github/guides/guide.md"),
        "# Guide\n\nCurrent documentation.\n",
    )?;
    fs::write(
        directory.path().join("stable.ts"),
        "export const stable = true;\n",
    )?;
    let database = directory.path().join("watch.db");
    run_refresh(
        directory.path(),
        &database,
        &indexer::IndexOptions::default(),
        RefreshScope::Full,
    )?;
    let classifier = EventClassifier::new(
        directory.path(),
        &database,
        &crate::docs::corpus::CorpusOptions::default(),
    )?;

    fs::remove_dir_all(directory.path().join(".github"))?;
    let signal = classifier
        .classify(&[directory.path().join(".github")])
        .expect("deleted hidden documentation directory must schedule refresh");
    assert_eq!(signal.scope, RefreshScope::Incremental);
    assert!(signal.source_paths.is_empty());
    let refreshed = run_refresh(
        directory.path(),
        &database,
        &indexer::IndexOptions::default(),
        signal.scope,
    )?;
    assert_eq!(refreshed.outcome.unchanged, 1);
    assert_eq!(refreshed.outcome.removed, 1);
    assert!(!refreshed.outcome.extraction_reset);

    let conn = crate::store::open_path_read_only(&database)?;
    assert_eq!(
        conn.query_row("SELECT count(*) FROM docs_fts", [], |row| {
            row.get::<_, i64>(0)
        })?,
        0
    );
    Ok(())
}

#[test]
fn refresh_with_an_unreadable_source_succeeds_with_a_rejection() -> Result<()> {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("video.ts"), [0xff, 0xfe])?;
    let database = directory.path().join("watch.db");

    let result = run_refresh(
        directory.path(),
        &database,
        &indexer::IndexOptions::default(),
        RefreshScope::Full,
    )?;

    assert_eq!(result.outcome.rejected, 1);
    assert_eq!(result.outcome.rejections[0].path, "video.ts");
    assert!(!result.snapshot.is_empty());
    Ok(())
}

#[test]
fn incremental_refresh_reuses_unchanged_source_rows() -> Result<()> {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("a.ts"), "export const a = 1;\n")?;
    fs::write(directory.path().join("b.ts"), "export const b = 2;\n")?;
    let database = directory.path().join("watch.db");
    run_refresh(
        directory.path(),
        &database,
        &indexer::IndexOptions::default(),
        RefreshScope::Full,
    )?;

    fs::write(directory.path().join("a.ts"), "export const a = 3;\n")?;
    let refreshed = run_refresh(
        directory.path(),
        &database,
        &indexer::IndexOptions::default(),
        RefreshScope::Incremental,
    )?;

    assert_eq!(
        (refreshed.outcome.indexed, refreshed.outcome.unchanged),
        (1, 1)
    );
    assert_eq!(refreshed.outcome.removed, 0);
    assert!(refreshed.outcome.projection_rebuilt);
    Ok(())
}
