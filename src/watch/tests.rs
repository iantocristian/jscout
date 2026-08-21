use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;

use super::{
    Coordinator, DirtySignal, EventClassifier, FinishState, MAX_INCREMENTAL_SOURCE_PATHS, Phase,
    RefreshScope, WatchOptions, clear_reconciliation_deadline_if_dirty, is_refresh_boundary,
    run_refresh, validate_options,
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
    assert_eq!(refresh.refresh_scope, RefreshScope::Full);
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
    let classifier = EventClassifier::new(root, &root.join("watch.db"));

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
    let mut classifier = EventClassifier::new(root, &root.join("watch.db"));

    assert!(
        classifier
            .classify(&[root.join("generated/output.ts")])
            .is_some()
    );
    fs::write(root.join(".gitignore"), "/generated/\n")?;
    classifier.reload_source_policy();
    assert!(
        classifier
            .classify(&[root.join("generated/output.ts")])
            .is_none()
    );
    Ok(())
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
        provider: None,
        embed_product_only: false,
        dependencies: &[],
        enrich_on_change: false,
        enrich_timeout: seconds(300),
        checker_sidecar: None,
        checker_node: "node",
        timing: false,
        debug: false,
        debounce: seconds(2),
        reconcile_interval: seconds(2),
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
        enrich_on_change: false,
        enrich_timeout: seconds(300),
        checker_sidecar: None,
        checker_node: "node",
        timing: false,
        debug: false,
        debounce: seconds(2),
        reconcile_interval: seconds(600),
    };
    let error = validate_options(&options).expect_err("product needs embedding");
    assert_eq!(error.to_string(), "--product requires --embed");
}

#[test]
fn refresh_phase_replaces_the_complete_file_set() -> Result<()> {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("a.ts"), "export const a = 1;\n")?;
    let database = directory.path().join("watch.db");
    let first = run_refresh(
        directory.path(),
        &database,
        &[],
        RefreshScope::Full,
        false,
        false,
    )?;
    assert_eq!(first.outcome.indexed, 1);
    fs::remove_file(directory.path().join("a.ts"))?;
    fs::write(directory.path().join("b.ts"), "export const b = 2;\n")?;
    let second = run_refresh(
        directory.path(),
        &database,
        &[],
        RefreshScope::Incremental,
        false,
        false,
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
fn refresh_with_an_unreadable_source_succeeds_with_a_rejection() -> Result<()> {
    let directory = tempfile::tempdir()?;
    fs::write(directory.path().join("video.ts"), [0xff, 0xfe])?;
    let database = directory.path().join("watch.db");

    let result = run_refresh(
        directory.path(),
        &database,
        &[],
        RefreshScope::Full,
        false,
        false,
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
        &[],
        RefreshScope::Full,
        false,
        false,
    )?;

    fs::write(directory.path().join("a.ts"), "export const a = 3;\n")?;
    let refreshed = run_refresh(
        directory.path(),
        &database,
        &[],
        RefreshScope::Incremental,
        false,
        false,
    )?;

    assert_eq!(
        (refreshed.outcome.indexed, refreshed.outcome.unchanged),
        (1, 1)
    );
    assert_eq!(refreshed.outcome.removed, 0);
    assert!(refreshed.outcome.projection_rebuilt);
    Ok(())
}
