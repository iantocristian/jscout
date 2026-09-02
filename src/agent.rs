use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};

/// The core tier teaches the production-selected surface and nothing else.
pub const CORE_GUIDE: &str = include_str!("../integrations/jscout/core/SKILL.md");
/// The full tier adds the memory, overview, graph, and write-back tools.
pub const FULL_GUIDE: &str = include_str!("../integrations/jscout/full/SKILL.md");

const UPDATE_TEMP_ATTEMPTS: usize = 64;
static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

/// Which skill text to install. The tier should match the MCP profile the
/// repository serves: `core` for the baseline tool set, `full` for structural.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Core,
    Full,
}

impl Tier {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "core" => Ok(Self::Core),
            "full" => Ok(Self::Full),
            _ => bail!("agent guide tier must be core or full"),
        }
    }

    pub fn guide(self) -> &'static str {
        match self {
            Self::Core => CORE_GUIDE,
            Self::Full => FULL_GUIDE,
        }
    }
}

/// Project-local skill locations understood by supported agent clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Agents,
    Claude,
    Codex,
}

impl Destination {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "agents" => Ok(Self::Agents),
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => bail!("agent guide destination must be agents, claude, or codex"),
        }
    }

    pub fn relative_path(self) -> &'static str {
        match self {
            Self::Agents => ".agents/skills/jscout/SKILL.md",
            Self::Claude => ".claude/skills/jscout/SKILL.md",
            Self::Codex => ".codex/skills/jscout/SKILL.md",
        }
    }
}

/// Resolve `--tier`/`--dest`. Install and print default to `core` and
/// `agents`; `--update` requires both so an update can never silently target
/// the wrong destination or downgrade an installed tier.
pub fn resolve_selectors(
    updating: bool,
    tier: Option<&str>,
    destination: Option<&str>,
) -> Result<(Tier, Destination)> {
    if updating && (tier.is_none() || destination.is_none()) {
        bail!(
            "agent guide --update requires explicit --tier core|full and --dest agents|claude|codex so it replaces exactly the installed guide"
        );
    }
    Ok((
        Tier::parse(tier.unwrap_or("core"))?,
        Destination::parse(destination.unwrap_or("agents"))?,
    ))
}

fn target(root: &Path, destination: Destination) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("resolve repository root {}", root.display()))?;
    Ok(root.join(destination.relative_path()))
}

pub fn install(root: &Path, tier: Tier, destination: Destination) -> Result<PathBuf> {
    let target = target(root, destination)?;
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
    file.write_all(tier.guide().as_bytes())?;
    file.sync_all()?;
    Ok(target)
}

/// Write the shipped guide for one tier to one project-local destination.
///
/// Unlike [`install`], this is an explicit overwrite operation. It also
/// creates a missing target, which makes one command sufficient to bring an
/// existing checkout to the current guide version. The replacement is staged
/// beside the target and renamed only after the complete guide has been
/// flushed, so a failure before replacement leaves the previous guide intact.
pub fn update(root: &Path, tier: Tier, destination: Destination) -> Result<PathBuf> {
    let target = target(root, destination)?;
    let parent = target
        .parent()
        .context("agent guide has no parent directory")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create agent skill directory {}", parent.display()))?;

    let (temporary, mut file) = create_update_temp(parent)?;
    let write_result = (|| -> Result<()> {
        file.write_all(tier.guide().as_bytes())
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

    use super::{CORE_GUIDE, Destination, FULL_GUIDE, Tier, install, resolve_selectors, update};

    /// The pre-G28 guide was 12,234 bytes; the core tier must stay under a
    /// quarter of it (G28 acceptance).
    const CORE_GUIDE_BYTE_LIMIT: usize = 12_234 / 4;

    const SHARED_MARKERS: &[&str] = &[
        "`exhaustive: true`",
        "`broad_or_query`",
        "abandon it",
        "Never page merely because `next_cursor` exists",
        "`truncated: false`",
        "`total_chunks`",
        "minimum_bytes=N",
        "`response_bytes: N`",
        "`sym:` anchor",
        "copied verbatim",
        "`who_uses`",
        "`calls`",
        "`file_outline`",
        "`events`",
        "`documentation_search`",
        "`publication_snapshot`",
        "not a session ceiling",
        "convention, not proof",
        "restart that surface's traversal",
        "prose is never runtime proof",
        "## On instruction",
        "Scope transfer",
        "or the cursor is rejected",
        "Carry explicitly supplied `origins` and `formats` into `definition` and",
        "from the echoed `scope`",
    ];

    #[test]
    fn core_guide_teaches_only_the_core_surface() {
        for marker in SHARED_MARKERS {
            assert!(
                CORE_GUIDE.contains(marker),
                "missing core contract: {marker}"
            );
        }
        for absent in [
            "semantic_memory",
            "repository_overview",
            "neighborhood",
            "annotate",
            "entities",
            "paths",
            "include_memory",
            "`expand",
        ] {
            assert!(
                !CORE_GUIDE.contains(absent),
                "core guide teaches a full-profile capability: {absent}"
            );
        }
        assert!(
            CORE_GUIDE.len() < CORE_GUIDE_BYTE_LIMIT,
            "core guide is {} bytes; the G28 gate is under {CORE_GUIDE_BYTE_LIMIT}",
            CORE_GUIDE.len()
        );
    }

    #[test]
    fn full_guide_adds_inquiry_and_write_back_without_losing_the_core() {
        for marker in SHARED_MARKERS {
            assert!(
                FULL_GUIDE.contains(marker),
                "missing full contract: {marker}"
            );
        }
        for marker in [
            "`semantic_memory`",
            "`repository_overview`",
            "not a first step",
            "`neighborhood`",
            "`entities`",
            "`paths`",
            "`annotate`",
            "## Flow 3",
            "`include_memory: false`",
            "`expand: false`",
            "at most one `expand: true`",
            "selection predicate",
            "never `certain`",
            "never\n`publication_snapshot`",
            "Memory-first on an anchored question",
        ] {
            assert!(
                FULL_GUIDE.contains(marker),
                "missing full contract: {marker}"
            );
        }
    }

    #[test]
    fn update_requires_explicit_tier_and_destination() -> Result<()> {
        assert_eq!(
            resolve_selectors(false, None, None)?,
            (Tier::Core, Destination::Agents)
        );
        assert_eq!(
            resolve_selectors(true, Some("full"), Some("claude"))?,
            (Tier::Full, Destination::Claude)
        );
        for (tier, dest) in [(None, None), (Some("full"), None), (None, Some("claude"))] {
            assert!(
                resolve_selectors(true, tier, dest)
                    .unwrap_err()
                    .to_string()
                    .contains("--update requires explicit --tier")
            );
        }
        Ok(())
    }

    #[test]
    fn tiers_and_destinations_parse_exactly() -> Result<()> {
        assert_eq!(Tier::parse("core")?, Tier::Core);
        assert_eq!(Tier::parse("full")?, Tier::Full);
        assert!(Tier::parse("structural").is_err());
        assert_eq!(Destination::parse("agents")?, Destination::Agents);
        assert_eq!(Destination::parse("claude")?, Destination::Claude);
        assert_eq!(Destination::parse("codex")?, Destination::Codex);
        assert!(Destination::parse("cursor").is_err());
        Ok(())
    }

    #[test]
    fn installs_once_without_overwriting() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let target = install(repo.path(), Tier::Core, Destination::Agents)?;
        assert_eq!(std::fs::read_to_string(&target)?, CORE_GUIDE);
        assert!(
            install(repo.path(), Tier::Full, Destination::Agents)
                .unwrap_err()
                .to_string()
                .contains("already exists")
        );
        Ok(())
    }

    #[test]
    fn destinations_are_independent_files() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let claude = install(repo.path(), Tier::Full, Destination::Claude)?;
        let codex = install(repo.path(), Tier::Core, Destination::Codex)?;
        assert!(claude.ends_with(".claude/skills/jscout/SKILL.md"));
        assert!(codex.ends_with(".codex/skills/jscout/SKILL.md"));
        assert_eq!(std::fs::read_to_string(&claude)?, FULL_GUIDE);
        assert_eq!(std::fs::read_to_string(&codex)?, CORE_GUIDE);
        assert!(!repo.path().join(".agents").exists());
        Ok(())
    }

    #[test]
    fn update_replaces_only_the_selected_destination() -> Result<()> {
        let repo = tempfile::tempdir()?;
        let target = install(repo.path(), Tier::Core, Destination::Agents)?;
        std::fs::write(&target, "locally stale guide\n")?;
        let unrelated = repo.path().join(".claude/skills/jscout/SKILL.md");
        std::fs::create_dir_all(unrelated.parent().unwrap())?;
        std::fs::write(&unrelated, "unrelated guide\n")?;

        assert_eq!(
            update(repo.path(), Tier::Full, Destination::Agents)?,
            target
        );
        assert_eq!(std::fs::read_to_string(&target)?, FULL_GUIDE);
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
        let target = update(repo.path(), Tier::Core, Destination::Agents)?;
        assert_eq!(
            target,
            repo.path()
                .canonicalize()?
                .join(".agents/skills/jscout/SKILL.md")
        );
        assert_eq!(std::fs::read_to_string(target)?, CORE_GUIDE);
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

        update(repo.path(), Tier::Core, Destination::Agents)?;

        assert!(!std::fs::symlink_metadata(&target)?.file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(target)?, CORE_GUIDE);
        assert_eq!(std::fs::read_to_string(linked)?, "do not overwrite\n");
        Ok(())
    }
}
