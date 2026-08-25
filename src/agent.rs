use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

pub const GUIDE: &str = include_str!("../integrations/jscout/SKILL.md");

const TARGET: &str = ".agents/skills/jscout/SKILL.md";
const UPDATE_TEMP_ATTEMPTS: usize = 64;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

fn target(root: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve repository root {}", root.display()))?;
    Ok(root.join(TARGET))
}

pub fn install(root: &Path) -> Result<PathBuf> {
    let target = target(root)?;
    if target.exists() {
        bail!("agent guide already exists: {}", target.display());
    }
    let parent = target
        .parent()
        .context("agent guide has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("create agent skill directory {}", parent.display()))?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&target)
        .with_context(|| format!("create agent guide {}", target.display()))?;
    file.write_all(GUIDE.as_bytes())?;
    file.flush()?;
    Ok(target)
}

/// Write the current shipped guide to its one supported project-local path.
///
/// Unlike [`install`], this is an explicit overwrite operation. It also
/// creates a missing target, which makes one command sufficient to bring an
/// existing checkout to the current guide version. The replacement is staged
/// beside the target and renamed only after the complete guide has been
/// flushed, so a failure before replacement leaves the previous guide intact.
pub fn update(root: &Path) -> Result<PathBuf> {
    let target = target(root)?;
    let parent = target
        .parent()
        .context("agent guide has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create agent skill directory {}", parent.display()))?;

    let (temporary, mut file) = create_update_temp(parent)?;
    let write_result = (|| -> Result<()> {
        file.write_all(GUIDE.as_bytes())
            .with_context(|| format!("write temporary agent guide {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("flush temporary agent guide {}", temporary.display()))?;
        drop(file);
        fs::rename(&temporary, &target).with_context(|| {
            format!(
                "replace agent guide {} with {}",
                target.display(),
                temporary.display()
            )
        })?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result?;
    Ok(target)
}

fn create_update_temp(parent: &Path) -> Result<(PathBuf, File)> {
    for _ in 0..UPDATE_TEMP_ATTEMPTS {
        let sequence = NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".SKILL.md.jscout-update-{}-{sequence}.tmp",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("create temporary agent guide {}", temporary.display())
                });
            }
        }
    }
    bail!(
        "could not allocate a temporary agent guide beside {} after {UPDATE_TEMP_ATTEMPTS} attempts",
        parent.display()
    )
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::{GUIDE, install, update};

    #[test]
    fn guide_encodes_the_investigation_and_inquiry_contracts() {
        for marker in [
            "Investigation loop",
            "Inquiry loop",
            "exhaustive: true",
            "next_cursor",
            "abandon it immediately",
            "do not page merely because a cursor",
            "abandoned query does not need cursor completion",
            "total_chunks",
            "match_lines",
            "response_budget_too_small",
            "minimum_bytes=<N>",
            "one exact returned `sym:` anchor",
            "strip only the leading `file:`",
            "human-authored `symbol` mode",
            "search's explicit `origins` allowlist",
            "Never synthesize follow-up arguments from echoed",
            "`scope.origins`",
            "separate non-exhaustive",
            "Baseline forces both unavailable stages off",
            "semantic_memory",
            "localize that evidence first",
            "exact `anchor` or `file`",
            "Simple occurrence",
            "anchor-free",
            "repository_overview` once",
            "include_memory: false",
            "expand: false",
            "one orientation expansion",
            "Restart the affected exhaustive traversal",
            "repository convention",
            "documentation_search",
            "shares the repository snapshot",
            "selection predicate",
            "selected subject's metadata",
        ] {
            assert!(GUIDE.contains(marker), "missing guide contract: {marker}");
        }
        assert!(!GUIDE.contains("initial result limit at 10"));
        assert!(!GUIDE.contains("Stop only when"));
    }

    #[test]
    fn installs_once_without_overwriting() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let target = install(repo.path())?;
        assert_eq!(std::fs::read_to_string(&target)?, GUIDE);
        assert!(
            install(repo.path())
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
        Ok(())
    }

    #[test]
    fn update_replaces_only_the_supported_target() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let target = install(repo.path())?;
        std::fs::write(&target, "locally stale guide\n")?;
        let unrelated = repo.path().join(".claude/skills/jscout/SKILL.md");
        std::fs::create_dir_all(unrelated.parent().unwrap())?;
        std::fs::write(&unrelated, "unrelated guide\n")?;

        assert_eq!(update(repo.path())?, target);
        assert_eq!(std::fs::read_to_string(&target)?, GUIDE);
        assert_eq!(std::fs::read_to_string(unrelated)?, "unrelated guide\n");
        assert!(target.parent().unwrap().read_dir()?.all(|entry| {
            !entry
                .expect("directory entry")
                .file_name()
                .to_string_lossy()
                .contains("jscout-update")
        }));
        Ok(())
    }

    #[test]
    fn update_creates_a_missing_installation() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let target = update(repo.path())?;
        assert_eq!(
            target,
            repo.path()
                .canonicalize()?
                .join(".agents/skills/jscout/SKILL.md")
        );
        assert_eq!(std::fs::read_to_string(target)?, GUIDE);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn update_replaces_the_exact_symlink_without_following_it() -> Result<()> {
        use std::os::unix::fs::symlink;

        let repo = tempfile::tempdir()?;
        let target = repo.path().join(".agents/skills/jscout/SKILL.md");
        std::fs::create_dir_all(target.parent().unwrap())?;
        let linked = repo.path().join("elsewhere.md");
        std::fs::write(&linked, "do not overwrite\n")?;
        symlink(&linked, &target)?;

        update(repo.path())?;

        assert!(!std::fs::symlink_metadata(&target)?.file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(target)?, GUIDE);
        assert_eq!(std::fs::read_to_string(linked)?, "do not overwrite\n");
        Ok(())
    }
}
