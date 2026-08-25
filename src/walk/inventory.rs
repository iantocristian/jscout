use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use ignore::WalkBuilder;

use super::{
    RepositoryInventory, RepositoryInventoryConsumer, SKIP_DIRS, WalkRejection, is_indexable,
};

enum WalkTask {
    Directory {
        relative: PathBuf,
        source_active: bool,
        consumer_active: bool,
    },
    Entry {
        relative: PathBuf,
        absolute: PathBuf,
        source_active: bool,
        consumer_active: bool,
    },
}

/// Traverse the repository once and feed code selection plus an independent
/// consumer from the same deterministic inventory. Plane-specific membership
/// and extraction remain with the consumer.
pub(super) fn repository_inventory<C: RepositoryInventoryConsumer>(
    root: &Path,
    mut consumer: C,
) -> Result<RepositoryInventory<C::Output>> {
    let canonical_root = root
        .canonicalize()
        .with_context(|| format!("canonicalize repository root {}", root.display()))?;
    if !canonical_root.is_dir() {
        bail!(
            "repository root is not a directory: {}",
            canonical_root.display()
        );
    }

    let mut builder = WalkBuilder::new(&canonical_root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .follow_links(false);
    let mut matchers = builder.build_matchers();
    let mut ignore = matchers
        .pop()
        .ok_or_else(|| anyhow!("repository ignore matcher was not built"))?;
    let mut files = Vec::new();
    let mut rejections = Vec::new();
    let mut pending = vec![WalkTask::Directory {
        relative: PathBuf::new(),
        source_active: true,
        consumer_active: consumer.is_active(),
    }];

    // Entry tasks are pushed in reverse name order so popping the explicit
    // stack preserves sorted recursive depth-first order without a depth cap.
    while let Some(task) = pending.pop() {
        let (relative, absolute, source_active, consumer_active) = match task {
            WalkTask::Entry {
                relative,
                absolute,
                source_active,
                consumer_active,
            } => (relative, absolute, source_active, consumer_active),
            WalkTask::Directory {
                relative,
                source_active,
                consumer_active,
            } => {
                let directory = canonical_root.join(&relative);
                let entries = match fs::read_dir(&directory) {
                    Ok(entries) => entries,
                    Err(error) => {
                        handle_walk_error(&mut rejections, &relative, &directory, error)?;
                        continue;
                    }
                };
                let mut collected = Vec::new();
                let mut failed = false;
                for entry in entries {
                    match entry {
                        Ok(entry) => collected.push(entry),
                        Err(error) => {
                            handle_walk_error(&mut rejections, &relative, &directory, error)?;
                            failed = true;
                            break;
                        }
                    }
                }
                if failed {
                    continue;
                }
                collected.sort_by_key(|entry| entry.file_name());
                pending.extend(collected.into_iter().rev().map(|entry| WalkTask::Entry {
                    relative: relative.join(entry.file_name()),
                    absolute: entry.path(),
                    source_active,
                    consumer_active,
                }));
                continue;
            }
        };

        let metadata = match fs::symlink_metadata(&absolute) {
            Ok(metadata) => metadata,
            Err(error) if crate::io_policy::is_inventory_race(&error) => continue,
            Err(error) if crate::io_policy::is_retryable(&error) => {
                return Err(error).with_context(|| {
                    format!("inspect repository inventory entry {}", absolute.display())
                });
            }
            Err(error) => {
                rejections.push(WalkRejection {
                    path: absolute,
                    stage: "walk",
                    error: error.to_string(),
                });
                continue;
            }
        };
        let file_type = metadata.file_type();
        let is_directory = file_type.is_dir();
        let consumer_path_relevant = consumer.path_relevant(&relative);

        if is_directory && is_hard_skip(&relative) {
            if consumer_active {
                consumer.record_decision(&relative, "directory", "hard-skip", None);
            }
            continue;
        }

        let (ignored, ignore_error) = ignore.matched_with_errors(&relative, is_directory);
        if let Some(error) = ignore_error {
            return Err(anyhow!(error.to_string())).with_context(|| {
                format!("load ignore rules while matching {}", absolute.display())
            });
        }
        if ignored.is_ignore() {
            if consumer_active && (is_directory || consumer_path_relevant) {
                consumer.record_decision(
                    &relative,
                    if is_directory { "directory" } else { "file" },
                    "ignored",
                    None,
                );
            }
            continue;
        }

        // The legacy source walker allows an ignore-file whitelist to reopen
        // a hidden entry. Documentation applies its own fixed allowlist.
        let source_entry_active = source_active
            && (!relative.file_name().is_some_and(os_str_starts_with_dot)
                || ignored.is_whitelist());

        if file_type.is_symlink() {
            if consumer_active {
                consumer.record_decision(&relative, "entry", "symlink-not-followed", None);
            }
            continue;
        }

        let consumer_hidden = consumer_active && consumer.hidden_path_is_excluded(&relative);
        if consumer_hidden && (is_directory || consumer_path_relevant) {
            consumer.record_decision(
                &relative,
                if is_directory { "directory" } else { "file" },
                "hidden-not-allowlisted",
                None,
            );
        }
        let consumer_entry_active = consumer_active && !consumer_hidden;

        if is_directory {
            if source_entry_active || consumer_entry_active {
                pending.push(WalkTask::Directory {
                    relative,
                    source_active: source_entry_active,
                    consumer_active: consumer_entry_active,
                });
            }
            continue;
        }
        if !file_type.is_file() {
            // Unrelated special files remain invisible to the code plane;
            // documentation candidates retain their explicit failure contract.
            if consumer_entry_active {
                consumer.inspect_special_file(&relative, &absolute, file_type)?;
            }
            continue;
        }

        if source_entry_active && is_indexable(&absolute) {
            files.push(absolute);
        }
        if consumer_entry_active {
            consumer.inspect_regular_file(relative);
        }
    }

    files.sort_by(|left, right| left.as_os_str().cmp(right.as_os_str()));
    rejections.sort_by(|left, right| {
        left.path
            .as_os_str()
            .cmp(right.path.as_os_str())
            .then_with(|| left.stage.cmp(right.stage))
    });
    let consumer = consumer.finish(&canonical_root)?;
    Ok(RepositoryInventory {
        files,
        rejections,
        consumer,
    })
}

fn handle_walk_error(
    rejections: &mut Vec<WalkRejection>,
    relative_directory: &Path,
    directory: &Path,
    error: std::io::Error,
) -> Result<()> {
    if relative_directory.as_os_str().is_empty() || crate::io_policy::is_retryable(&error) {
        return Err(error)
            .with_context(|| format!("discover repository under {}", directory.display()));
    }
    if !crate::io_policy::is_inventory_race(&error) {
        rejections.push(WalkRejection {
            path: directory.to_path_buf(),
            stage: "walk",
            error: error.to_string(),
        });
    }
    Ok(())
}

fn is_hard_skip(path: &Path) -> bool {
    path.file_name().is_some_and(|name| {
        name == ".git" || name.to_str().is_some_and(|name| SKIP_DIRS.contains(&name))
    })
}

#[cfg(unix)]
fn os_str_starts_with_dot(value: &std::ffi::OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().first() == Some(&b'.')
}

#[cfg(windows)]
fn os_str_starts_with_dot(value: &std::ffi::OsStr) -> bool {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide().next() == Some(u16::from(b'.'))
}

#[cfg(not(any(unix, windows)))]
fn os_str_starts_with_dot(value: &std::ffi::OsStr) -> bool {
    value.to_string_lossy().as_bytes().first() == Some(&b'.')
}
