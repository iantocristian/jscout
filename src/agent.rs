use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub const GUIDE: &str = include_str!("../integrations/jscout/SKILL.md");

pub fn install(root: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve repository root {}", root.display()))?;
    let target = root.join(".agents/skills/jscout/SKILL.md");
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

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::{GUIDE, install};

    #[test]
    fn guide_encodes_the_investigation_and_inquiry_contracts() {
        for marker in [
            "Investigation loop",
            "Inquiry loop",
            "exhaustive: true",
            "next_cursor",
            "truncated` is false",
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
            "repository_overview` once",
            "include_memory: false",
            "expand: false",
            "one orientation expansion",
            "Restart the affected exhaustive traversal",
            "repository convention",
            "documentation_search",
            "shares the repository snapshot",
        ] {
            assert!(GUIDE.contains(marker), "missing guide contract: {marker}");
        }
        assert!(!GUIDE.contains("initial result limit at 10"));
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
}
