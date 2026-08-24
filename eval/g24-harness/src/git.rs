//! A real-git laboratory. Every provenance claim in the plan is checked against
//! an actual `git` process, never against a model of git.
//!
//! Hermetic by construction: all git invocations MUST run with
//! [`hermetic_env`] so the developer's global/system config (aliases,
//! `blame.ignoreRevsFile`, gpg signing, default branch) cannot leak into a
//! result. A test that silently inherited ambient config would prove nothing.

use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};

use crate::proc::{run, CmdOut};

/// Environment that isolates git from user/system configuration and pins
/// identity and timestamps. Includes `GIT_CONFIG_GLOBAL=/dev/null`,
/// `GIT_CONFIG_SYSTEM=/dev/null`, author/committer identity, and
/// `GIT_TERMINAL_PROMPT=0`.
pub fn hermetic_env() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GIT_CONFIG_GLOBAL", "/dev/null"),
        ("GIT_CONFIG_SYSTEM", "/dev/null"),
        ("GIT_CONFIG_NOSYSTEM", "1"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("GIT_AUTHOR_NAME", "Harness Author"),
        ("GIT_AUTHOR_EMAIL", "author@harness.test"),
        ("GIT_COMMITTER_NAME", "Harness Committer"),
        ("GIT_COMMITTER_EMAIL", "committer@harness.test"),
    ]
}

/// Run git in `cwd` under [`hermetic_env`] plus `extra` environment entries.
/// Every git invocation in this module funnels through here, so ambient
/// configuration can never leak into a measurement.
fn git_in(cwd: &Path, args: &[&str], extra: &[(&str, &str)]) -> CmdOut {
    let mut env = hermetic_env();
    env.extend(extra.iter().copied());
    run(Path::new("git"), args, cwd, &env)
}

fn git_checked(cwd: &Path, args: &[&str], extra: &[(&str, &str)]) -> Result<String> {
    let out = git_in(cwd, args, extra);
    if !out.ok {
        bail!(
            "git {:?} failed (code {:?}):\n{}",
            args,
            out.code,
            out.combined()
        );
    }
    Ok(out.stdout.trim().to_string())
}

/// A disposable git repository in a temp directory.
///
/// Implementation requirements:
/// - `git init -b main` (or `-c init.defaultBranch=main`) so branch naming is
///   deterministic across git versions;
/// - `commit.gpgsign=false` set locally;
/// - every invocation goes through [`hermetic_env`].
pub struct GitLab {
    pub dir: tempfile::TempDir,
}

impl GitLab {
    /// Create and initialize an empty repository.
    pub fn init() -> Result<Self> {
        let dir = tempfile::Builder::new().prefix("g24-git-").tempdir()?;
        let lab = GitLab { dir };
        // `git init -b` needs git >= 2.28; `symbolic-ref` works everywhere and
        // pins the branch name without depending on init.defaultBranch.
        git_checked(lab.path(), &["init", "--quiet"], &[])?;
        git_checked(
            lab.path(),
            &["symbolic-ref", "HEAD", "refs/heads/main"],
            &[],
        )?;
        lab.apply_local_config()?;
        Ok(lab)
    }

    /// Local config that must hold for every repository this lab produces,
    /// including clones (which do not inherit the source repo's local config).
    fn apply_local_config(&self) -> Result<()> {
        for (key, value) in [
            ("commit.gpgsign", "false"),
            ("tag.gpgsign", "false"),
            ("user.name", "Harness Author"),
            ("user.email", "author@harness.test"),
            ("core.autocrlf", "false"),
            ("gc.auto", "0"),
            ("advice.detachedHead", "false"),
        ] {
            git_checked(self.path(), &["config", "--local", key, value], &[])?;
        }
        Ok(())
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// Write `contents` to `rel`, creating parent directories.
    pub fn write(&self, rel: &str, contents: &str) -> Result<()> {
        let target = self.path().join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&target, contents).with_context(|| format!("writing {}", target.display()))
    }

    /// Delete `rel` from the worktree.
    pub fn remove(&self, rel: &str) -> Result<()> {
        let target = self.path().join(rel);
        std::fs::remove_file(&target).with_context(|| format!("removing {}", target.display()))
    }

    /// Run git, returning trimmed stdout; errors on a non-zero exit.
    pub fn git(&self, args: &[&str]) -> Result<String> {
        git_checked(self.path(), args, &[])
    }

    /// Run git, never failing; the caller inspects the outcome.
    pub fn git_raw(&self, args: &[&str]) -> CmdOut {
        git_in(self.path(), args, &[])
    }

    /// `git add -A`.
    pub fn add_all(&self) -> Result<()> {
        self.git(&["add", "-A"])?;
        Ok(())
    }

    /// Stage everything and commit; returns the new commit SHA.
    /// Author and committer time are both "now".
    pub fn commit(&self, message: &str) -> Result<String> {
        self.add_all()?;
        git_checked(
            self.path(),
            &["commit", "--quiet", "--allow-empty", "-m", message],
            &[],
        )?;
        self.head()
    }

    /// Stage everything and commit with explicitly controlled author and
    /// committer epochs (via `GIT_AUTHOR_DATE` / `GIT_COMMITTER_DATE`).
    /// This is what makes author-vs-committer divergence testable.
    pub fn commit_at(
        &self,
        message: &str,
        author_epoch: i64,
        committer_epoch: i64,
    ) -> Result<String> {
        self.add_all()?;
        let author = format!("{author_epoch} +0000");
        let committer = format!("{committer_epoch} +0000");
        git_checked(
            self.path(),
            &["commit", "--quiet", "--allow-empty", "-m", message],
            &[
                ("GIT_AUTHOR_DATE", author.as_str()),
                ("GIT_COMMITTER_DATE", committer.as_str()),
            ],
        )?;
        self.head()
    }

    /// Current `HEAD` SHA.
    pub fn head(&self) -> Result<String> {
        self.git(&["rev-parse", "HEAD"])
    }

    /// Shallow-clone this repository into a fresh temp directory.
    ///
    /// NOTE: a local path clone ignores `--depth`; the implementation MUST use a
    /// `file://` URL so the clone is genuinely shallow. Verify by asserting
    /// `.git/shallow` exists in the result.
    pub fn clone_shallow(&self, depth: usize) -> Result<GitLab> {
        let source = self
            .path()
            .canonicalize()
            .context("canonicalizing clone source")?;
        let url = format!("file://{}", source.display());
        let dir = tempfile::Builder::new().prefix("g24-clone-").tempdir()?;
        let clone = GitLab { dir };
        let depth = depth.to_string();
        let dest = clone.path().to_string_lossy().into_owned();
        git_checked(
            clone.path(),
            &[
                "clone",
                "--quiet",
                "--depth",
                depth.as_str(),
                url.as_str(),
                dest.as_str(),
            ],
            &[],
        )?;
        clone.apply_local_config()?;
        // A plain local-path clone silently ignores --depth and produces a full
        // repository with no `.git/shallow`. Refuse to hand back a clone that
        // is not actually shallow, so no test can be fooled by one.
        let shallow = git_dir(clone.path())?.join("shallow");
        if !shallow.is_file() {
            bail!(
                "clone at {} is not shallow: {} missing",
                clone.path().display(),
                shallow.display()
            );
        }
        Ok(clone)
    }

    /// `git fetch --deepen <n>` on a shallow clone.
    pub fn deepen(&self, depth: usize) -> Result<()> {
        let depth = depth.to_string();
        self.git(&["fetch", "--quiet", "--deepen", depth.as_str()])?;
        Ok(())
    }
}

/// One line of `git blame --line-porcelain` output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    pub sha: String,
    /// True when the porcelain header carried the `boundary` marker, i.e. the
    /// commit is a shallow-clone/`--since` boundary and its timestamp does NOT
    /// establish when the line was written.
    pub boundary: bool,
    /// True for the all-zero SHA git uses for uncommitted worktree content.
    pub not_committed_yet: bool,
    pub author: String,
    pub author_time: i64,
    pub committer_time: i64,
    /// 1-based line number in the final file.
    pub final_line: usize,
    pub content: String,
    pub filename: Option<String>,
    pub previous_filename: Option<String>,
}

impl BlameLine {
    fn new() -> Self {
        BlameLine {
            sha: String::new(),
            boundary: false,
            not_committed_yet: false,
            author: String::new(),
            author_time: 0,
            committer_time: 0,
            final_line: 0,
            content: String::new(),
            filename: None,
            previous_filename: None,
        }
    }
}

/// Run `git blame --line-porcelain <extra_args> -- <file>` and parse it.
/// `extra_args` lets a test add `--no-replace-objects`, `-M`, `-C`,
/// `-c blame.ignoreRevsFile=`, and similar.
pub fn blame_porcelain(repo: &Path, file: &str, extra_args: &[&str]) -> Result<Vec<BlameLine>> {
    // OBSERVED (git 2.49.0): `git blame --no-replace-objects` fails with
    // "unknown option `--no-replace-objects'" (exit 129). Replacement-object
    // suppression and `-c key=value` are git-LEVEL options and must precede the
    // subcommand. The plan's phrase "provenance Git commands disable
    // replacement objects with `--no-replace-objects`" is only implementable
    // with that placement, so those arguments are hoisted ahead of `blame`.
    let mut leading: Vec<&str> = Vec::new();
    let mut trailing: Vec<&str> = Vec::new();
    let mut index = 0;
    while index < extra_args.len() {
        match extra_args[index] {
            "-c" if index + 1 < extra_args.len() => {
                leading.push("-c");
                leading.push(extra_args[index + 1]);
                index += 2;
            }
            "--no-replace-objects" | "--no-optional-locks" | "--literal-pathspecs" => {
                leading.push(extra_args[index]);
                index += 1;
            }
            other => {
                trailing.push(other);
                index += 1;
            }
        }
    }

    let mut args: Vec<&str> = Vec::new();
    args.extend(leading);
    args.push("blame");
    args.push("--line-porcelain");
    args.extend(trailing);
    args.push("--");
    args.push(file);

    let out = git_in(repo, &args, &[]);
    if !out.ok {
        bail!(
            "git {:?} failed (code {:?}):\n{}",
            args,
            out.code,
            out.combined()
        );
    }
    Ok(parse_line_porcelain(&out.stdout))
}

fn parse_line_porcelain(text: &str) -> Vec<BlameLine> {
    let mut lines = Vec::new();
    let mut current: Option<BlameLine> = None;

    for raw in text.split('\n') {
        if let Some(content) = raw.strip_prefix('\t') {
            // The tab-prefixed line terminates one blame entry.
            if let Some(mut entry) = current.take() {
                entry.content = content.to_string();
                lines.push(entry);
            }
            continue;
        }
        if let Some((sha, orig, final_line)) = parse_header(raw) {
            let mut entry = BlameLine::new();
            entry.not_committed_yet = !sha.is_empty() && sha.bytes().all(|b| b == b'0');
            entry.sha = sha;
            entry.final_line = final_line;
            let _ = orig;
            current = Some(entry);
            continue;
        }
        let Some(entry) = current.as_mut() else {
            continue;
        };
        let (key, value) = match raw.split_once(' ') {
            Some((key, value)) => (key, value),
            None => (raw, ""),
        };
        match key {
            "author" => entry.author = value.to_string(),
            "author-time" => entry.author_time = value.trim().parse().unwrap_or(0),
            "committer-time" => entry.committer_time = value.trim().parse().unwrap_or(0),
            "boundary" => entry.boundary = true,
            "filename" => entry.filename = Some(value.to_string()),
            "previous" => {
                // `previous <sha> <filename>`
                entry.previous_filename = value.split_once(' ').map(|(_, name)| name.to_string());
            }
            _ => {}
        }
    }

    lines
}

/// `<sha> <orig-line> <final-line> [<num-lines>]`
fn parse_header(line: &str) -> Option<(String, usize, usize)> {
    let mut parts = line.split(' ');
    let sha = parts.next()?;
    if sha.len() < 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let orig: usize = parts.next()?.parse().ok()?;
    let final_line: usize = parts.next()?.parse().ok()?;
    // A fourth field (group size) is optional; anything else means this was not
    // a header line.
    if let Some(extra) = parts.next() {
        if extra.parse::<usize>().is_err() {
            return None;
        }
    }
    if parts.next().is_some() {
        return None;
    }
    Some((sha.to_string(), orig, final_line))
}

/// `git log -1 --format=%H -- <path>`: the newest commit touching that path.
/// `None` when no commit touches it. This is the plan's blame-cache key
/// component, so its invalidation behavior is load-bearing.
pub fn path_tip_commit(repo: &Path, path: &str) -> Result<Option<String>> {
    let out = git_in(repo, &["log", "-1", "--format=%H", "--", path], &[]);
    if !out.ok {
        // An unborn branch (no commits yet) is a legitimate "no tip", not a
        // failure. Anything else is a real error and must surface.
        if out.combined().contains("does not have any commits") {
            return Ok(None);
        }
        bail!("git log failed (code {:?}):\n{}", out.code, out.combined());
    }
    let sha = out.stdout.trim();
    Ok(if sha.is_empty() {
        None
    } else {
        Some(sha.to_string())
    })
}

/// Fingerprint of the shallow boundary: a stable hash of the sorted
/// `.git/shallow` contents, or `None` when the repository is not shallow.
/// Changes when the clone is deepened.
pub fn shallow_boundary_fingerprint(repo: &Path) -> Result<Option<String>> {
    // `shallow` lives in the COMMON git dir, so a linked worktree must not look
    // in its own per-worktree gitdir and conclude the repository is complete.
    let mut candidates = vec![git_common_dir(repo)?.join("shallow")];
    let per_worktree = git_dir(repo)?.join("shallow");
    if !candidates.contains(&per_worktree) {
        candidates.push(per_worktree);
    }
    let Some(shallow) = candidates.into_iter().find(|path| path.is_file()) else {
        return Ok(None);
    };
    let text = match std::fs::read_to_string(&shallow) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(anyhow!(error).context(format!("reading {}", shallow.display()))),
    };
    let mut entries: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    entries.sort_unstable();
    Ok(Some(crate::md::hash_hex(entries.join("\n").as_bytes())))
}

/// Locate the `.git` directory for `repo` (handles worktrees and clones).
pub fn git_dir(repo: &Path) -> Result<PathBuf> {
    resolve_git_dir(repo, "--git-dir")
}

/// The shared git directory: identical to [`git_dir`] outside linked worktrees.
pub fn git_common_dir(repo: &Path) -> Result<PathBuf> {
    resolve_git_dir(repo, "--git-common-dir")
}

fn resolve_git_dir(repo: &Path, flag: &str) -> Result<PathBuf> {
    let out = git_in(repo, &["rev-parse", flag], &[]);
    if !out.ok {
        bail!(
            "git rev-parse {flag} failed in {}:\n{}",
            repo.display(),
            out.combined()
        );
    }
    let path = PathBuf::from(out.stdout.trim());
    Ok(if path.is_absolute() {
        path
    } else {
        repo.join(path)
    })
}
