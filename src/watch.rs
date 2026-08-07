use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use notify::{RecursiveMode, Watcher};

use crate::{embed, indexer, store, walk};

/// Paths that should trigger re-indexing even though they aren't source files:
/// resolution config changes alter the module graph.
fn is_relevant(path: &Path) -> bool {
    if walk::is_indexable(path) {
        return true;
    }
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("package.json") | Some("tsconfig.json") | Some("jsconfig.json")
    )
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

pub fn watch(root: &Path, embed_on_change: bool) -> Result<()> {
    let root = root.canonicalize()?;
    let conn = store::open(&root)?;
    let outcome = indexer::index_repo(&root, &conn)?;
    eprintln!(
        "initial: {} indexed, {} unchanged — watching {} for changes (ctrl-c to stop)",
        outcome.indexed,
        outcome.unchanged,
        root.display()
    );
    let provider = if embed_on_change { embed::Provider::from_env() } else { None };
    if embed_on_change && provider.is_none() {
        eprintln!("warning: --embed set but no provider configured; skipping embeddings");
    }

    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })?;
    watcher.watch(&root, RecursiveMode::Recursive)?;

    loop {
        // Block until something happens, then debounce until 300ms of quiet.
        let first = match rx.recv() {
            Ok(ev) => ev,
            Err(_) => break,
        };
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
        if !pending.iter().any(|p| !is_noise(p) && is_relevant(p)) {
            continue;
        }
        let started = std::time::Instant::now();
        match indexer::index_repo(&root, &conn) {
            Ok(o) if o.indexed > 0 || o.failed > 0 => {
                eprintln!(
                    "re-indexed {} files ({} failed) in {:?}",
                    o.indexed,
                    o.failed,
                    started.elapsed()
                );
                if let Some(p) = &provider {
                    match embed::embed_missing(&conn, p, 64) {
                        Ok((done, _)) if done > 0 => eprintln!("embedded {done} new chunks"),
                        Ok(_) => {}
                        Err(e) => eprintln!("embed failed: {e}"),
                    }
                }
            }
            Ok(_) => {}
            Err(e) => eprintln!("re-index failed: {e}"),
        }
    }
    Ok(())
}
