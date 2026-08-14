use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::{Result, bail};
use notify::{RecursiveMode, Watcher};

use crate::{checker, embed, indexer, store, walk};

pub struct WatchOptions<'a> {
    pub embed_on_change: bool,
    pub dependencies: &'a [String],
    pub enrich_on_change: bool,
    pub enrich_timeout: Duration,
    pub checker_sidecar: Option<&'a Path>,
}

/// Paths that should trigger re-indexing even though they aren't source files:
/// resolution config changes alter the module graph.
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
            | "tsconfig.json"
            | "jsconfig.json"
    ) || (name.starts_with("tsconfig.") && name.ends_with(".json"))
        || name.ends_with(".d.ts")
        || name.ends_with(".d.mts")
        || name.ends_with(".d.cts")
}

fn is_noise(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("node_modules") | Some(".git") | Some("dist") | Some("build") | Some(".next")
        )
    }) || path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with(store::DB_FILE))
}

pub fn watch(root: &Path, options: &WatchOptions<'_>) -> Result<()> {
    if options.enrich_on_change && options.enrich_timeout.is_zero() {
        bail!("--enrich-timeout must be greater than zero seconds");
    }
    let root = root.canonicalize()?;
    // Subscribe before the initial index/enrichment. Those passes can be long;
    // events that arrive while they run must remain queued for the first loop
    // iteration instead of falling into an unobserved startup window.
    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;

    let conn = store::open(&root)?;
    let index_options = indexer::IndexOptions {
        dependencies: options.dependencies.to_vec(),
        ..Default::default()
    };
    let outcome = indexer::index_repo_with_options(&root, &conn, &index_options)?;
    eprintln!(
        "initial: {} indexed, {} unchanged, {} failed — watching {} for changes (ctrl-c to stop)",
        outcome.indexed,
        outcome.unchanged,
        outcome.failed,
        root.display()
    );
    indexer::report_failures(&outcome);
    let provider = if options.embed_on_change {
        embed::Provider::from_env()?
    } else {
        None
    };
    if options.embed_on_change && provider.is_none() {
        eprintln!("warning: --embed set but no provider configured; skipping embeddings");
    }
    if options.enrich_on_change {
        if outcome.failed > 0 {
            eprintln!(
                "checker enrichment deferred because initial indexing failed; watch will retry"
            );
        } else {
            run_checker_enrichment(&root, options);
        }
    }

    while let Ok(first) = rx.recv() {
        // The outer receive blocks until something happens; then debounce
        // until 300ms of quiet.
        let mut pending: Vec<PathBuf> = Vec::new();
        if let Ok(ev) = first {
            pending.extend(ev.paths);
        }
        loop {
            match rx.recv_timeout(Duration::from_millis(300)) {
                Ok(Ok(ev)) => pending.extend(ev.paths),
                Ok(Err(_)) => {}
                Err(mpsc::RecvTimeoutError::Timeout) => break,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
            }
        }
        if !pending
            .iter()
            .any(|path| !is_noise(path) && is_relevant(path))
        {
            continue;
        }
        let started = std::time::Instant::now();
        match indexer::index_repo_with_options(&root, &conn, &index_options) {
            Ok(o) => {
                let changed = o.indexed > 0 || o.projection_rebuilt;
                if o.indexed > 0 || o.failed > 0 {
                    eprintln!(
                        "re-indexed {} files ({} failed) in {:?}",
                        o.indexed,
                        o.failed,
                        started.elapsed()
                    );
                    indexer::report_failures(&o);
                }
                if options.enrich_on_change {
                    if o.failed > 0 {
                        eprintln!(
                            "checker enrichment deferred because indexing failed; checker edges remain absent"
                        );
                    } else {
                        run_checker_enrichment(&root, options);
                    }
                }
                if changed && let Some(p) = &provider {
                    match embed::embed_missing(&conn, p, 64) {
                        Ok((done, _)) if done > 0 => eprintln!("embedded {done} new chunks"),
                        Ok(_) => {}
                        Err(e) => eprintln!("embed failed: {e}"),
                    }
                }
            }
            Err(e) => eprintln!("re-index failed: {e}"),
        }
    }
    Ok(())
}

/// Replenish the checker plane for the structural snapshot just published.
/// Failed project work stays in inactive staging for an exact-plan retry.
/// Structural indexing owns invalidation when it publishes a new snapshot.
fn run_checker_enrichment(root: &Path, options: &WatchOptions<'_>) -> bool {
    match checker::enrich(
        root,
        &checker::EnrichOptions {
            database: None,
            sidecar: options.checker_sidecar,
            timeout: options.enrich_timeout,
            files: Vec::new(),
            packages: Vec::new(),
            members: Vec::new(),
            roles: Vec::new(),
            max_occurrences: None,
            include_all: false,
            dry_run: false,
        },
    ) {
        Ok(report) => {
            eprintln!(
                "checker enriched {} facts from {} occurrence(s) across {} configured project(s)",
                report.facts_published, report.occurrences_queried, report.projects
            );
            true
        }
        Err(error) => {
            eprintln!(
                "checker enrichment failed; completed work remains staged for an exact-snapshot retry: {error}"
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{is_noise, is_relevant};

    #[test]
    fn lockfiles_trigger_reconciliation_without_watching_node_modules() {
        assert!(is_relevant(Path::new("pnpm-lock.yaml")));
        assert!(is_relevant(Path::new("package-lock.json")));
        assert!(is_relevant(Path::new("yarn.lock")));
        assert!(is_relevant(Path::new("tsconfig.server.json")));
        assert!(is_relevant(Path::new("types/ambient.d.ts")));
        assert!(is_noise(Path::new("node_modules/dep/index.js")));
        assert!(!is_noise(Path::new("pnpm-lock.yaml")));
    }
}
