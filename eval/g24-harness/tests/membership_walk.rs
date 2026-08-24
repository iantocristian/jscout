//! G24 membership precedence, tested empirically against the real `ignore`
//! crate (0.4, the same crate and version the code plane uses in
//! `src/walk.rs`).
//!
//! The plan under test is `docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md`, "Markdown corpus specification →
//! Membership":
//!
//! > The first rule that applies decides membership:
//! > 1. Deterministic skips and repository ignore files prune traversal with
//! >    the same ignore semantics as the code plane; `.git` is always a hard
//! >    skip.
//! > 2. The docs walker additionally descends into the fixed root-level hidden
//! >    directory allowlist `.github`, `.claude`, and `.agents`. All other
//! >    hidden paths remain excluded.
//! > 3. `exclude` globs, anchored at the indexed root, matching files.
//! > 4. `include` globs, anchored at the indexed root, matching files; default
//! >    `**/*.md`.
//! >
//! > Exclude beats include; ignore beats both; include cannot resurrect an
//! > ignored file in v1. Files larger than 4 MiB (4,194,304 bytes …) are
//! > excluded from admission. `docs status` reports the deciding rule per
//! > encountered file (`indexed`, `excluded`, `not-included`,
//! > `hidden-not-allowlisted`, `oversized`, `non-utf8`) and per pruned
//! > directory (`hard-skip`, `ignored`, `hidden-not-allowlisted`), without
//! > enumerating descendants beneath pruned directories.
//!
//! `docs_inventory` below is a faithful implementation of exactly that, built
//! on `ignore::WalkBuilder`. Every place the plan left a decision open is
//! marked `INVENTED:` at the point of use and repeated in the harness report.
//!
//! METHODOLOGY: where reality contradicts the plan the assertion documents the
//! ACTUAL behavior and a comment names the plan claim that failed. No
//! assertion here is weakened to produce a green run.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use ignore::{IncrementalIgnore, WalkBuilder};

// ---------------------------------------------------------------------------
// The membership walker under test
// ---------------------------------------------------------------------------

/// Rule 2's fixed root-level hidden allowlist, verbatim from the plan.
const HIDDEN_ALLOWLIST: &[&str] = &[".github", ".claude", ".agents"];

/// Rule 1's "deterministic skips". Copied from `walk.rs::SKIP_DIRS` so the
/// docs plane prunes what the code plane prunes.
const SKIP_DIRS: &[&str] = &["node_modules", "dist", ".next", "coverage", "out"];

/// "Files larger than 4 MiB (4,194,304 bytes, an evaluation hypothesis) are
/// excluded from admission."
const MAX_ADMITTED_BYTES: u64 = 4 * 1024 * 1024;

/// `[docs]` membership settings.
#[derive(Debug, Clone)]
struct DocsConfig {
    include: Vec<String>,
    exclude: Vec<String>,
    max_bytes: u64,
}

impl Default for DocsConfig {
    fn default() -> Self {
        Self {
            include: vec!["**/*.md".to_string()],
            exclude: Vec::new(),
            max_bytes: MAX_ADMITTED_BYTES,
        }
    }
}

/// The deciding rule for one encountered file, in `docs status` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FileRule {
    Indexed,
    Excluded,
    NotIncluded,
    HiddenNotAllowlisted,
    Oversized,
    NonUtf8,
}

/// The deciding rule for one pruned directory, in `docs status` vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DirRule {
    HardSkip,
    HiddenNotAllowlisted,
    // `Ignored` is in the plan's vocabulary but is NOT constructible from
    // inside a walk: see `ignored_dirs_are_invisible_to_the_walk_filter` and
    // the SPEC GAP note on `ignore_probe`.
    #[allow(dead_code)]
    Ignored,
}

/// One walk's complete decision record, in traversal order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Walked {
    files: Vec<(String, FileRule)>,
    pruned: Vec<(String, DirRule)>,
}

impl Walked {
    fn admitted(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self
            .files
            .iter()
            .filter(|(_, rule)| *rule == FileRule::Indexed)
            .map(|(path, _)| path.as_str())
            .collect();
        out.sort_unstable();
        out
    }

    fn decisions(&self) -> BTreeMap<&str, FileRule> {
        self.files
            .iter()
            .map(|(path, rule)| (path.as_str(), *rule))
            .collect()
    }

    fn pruned_map(&self) -> BTreeMap<&str, DirRule> {
        self.pruned
            .iter()
            .map(|(path, rule)| (path.as_str(), *rule))
            .collect()
    }

    /// True when nothing at or beneath `prefix` was ever enumerated. The plan
    /// requires `docs status` to report a pruned directory "without
    /// enumerating descendants beneath pruned directories".
    fn nothing_beneath(&self, prefix: &str) -> bool {
        let under = format!("{prefix}/");
        !self.files.iter().any(|(path, _)| path.starts_with(&under))
            && !self.pruned.iter().any(|(path, _)| path.starts_with(&under))
    }
}

/// Rule 1 and rule 2 for directories, evaluated in plan order.
///
/// The `ignore` crate applies its own ignore matching BEFORE this predicate
/// runs (`walk.rs::skip_entry` calls `should_skip_entry` first), so repository
/// ignore files already decided anything this function sees. That ordering is
/// what makes "ignore beats both" structurally true rather than a convention.
fn deterministic_dir_prune(rel: &str) -> Option<DirRule> {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    // "`.git` is always a hard skip." NOTE FOR IMPLEMENTERS: the `ignore`
    // crate does NOT skip `.git` on its own — it only skips it as a *hidden*
    // path. A docs walker that turns `hidden(false)` off to reach the
    // allowlist therefore has to skip `.git` explicitly or it walks the object
    // store. Proven by `w3_git_directory_is_never_traversed`.
    if name == ".git" {
        return Some(DirRule::HardSkip);
    }
    if SKIP_DIRS.contains(&name) {
        return Some(DirRule::HardSkip);
    }
    None
}

/// Rule 2's hidden policy.
///
/// INVENTED: the plan never defines "hidden". This uses the same notion the
/// `ignore` crate uses — a path component whose name starts with `.` — applied
/// to components relative to the INDEXED ROOT, so a tempdir or checkout that
/// itself sits under a dotted directory does not hide the whole corpus.
///
/// INVENTED: "root-level" is read literally — the allowlist only excuses a dot
/// component that is the FIRST component under the indexed root. A nested
/// `packages/app/.github` is not excused, and neither is a hidden directory
/// nested inside an allowlisted one (`.github/.private`). See W9.
fn hidden_not_allowlisted(rel: &str) -> bool {
    for (depth, component) in rel.split('/').enumerate() {
        if !component.starts_with('.') {
            continue;
        }
        if depth == 0 && HIDDEN_ALLOWLIST.contains(&component) {
            continue;
        }
        return true;
    }
    false
}

/// Build the include/exclude glob matcher.
///
/// INVENTED: the plan says "globs" without naming a dialect. This uses the
/// `ignore` crate's own gitignore/globset dialect, which is the only glob
/// engine already in the code plane's dependency tree, with paths presented
/// relative to the indexed root ("anchored at the indexed root").
fn glob_matcher(root: &Path, patterns: &[String]) -> Gitignore {
    let mut builder = GitignoreBuilder::new(root);
    for pattern in patterns {
        builder.add_line(None, pattern).expect("valid glob");
    }
    builder.build().expect("glob set builds")
}

/// Path relative to `root`, with `/` separators. `None` when `path` is outside
/// `root`; `Some("")` for the root itself.
fn rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let mut out = String::new();
    for component in rel.components() {
        if !out.is_empty() {
            out.push('/');
        }
        out.push_str(&component.as_os_str().to_string_lossy());
    }
    Some(out)
}

/// The docs walker: rules 1-4 plus the admission bound, exactly as specified.
fn docs_inventory(root: &Path, config: &DocsConfig) -> Walked {
    let include = glob_matcher(root, &config.include);
    let exclude = glob_matcher(root, &config.exclude);

    let log: Arc<Mutex<Walked>> = Arc::new(Mutex::new(Walked::default()));

    let filter_root = root.to_path_buf();
    let filter_log = Arc::clone(&log);
    let mut builder = WalkBuilder::new(root);
    builder
        // Rule 2 requires descending into hidden allowlisted directories, and
        // the `ignore` crate's hidden filter runs before any caller predicate
        // and cannot be selectively overridden. So hidden filtering moves into
        // `filter_entry` wholesale. Rules 1 and 2 are therefore evaluated in
        // one predicate, after the crate's ignore matching.
        .hidden(false)
        // Same as the code plane: `parents(true)` (the crate default) and
        // repository ignore files on.
        .parents(true)
        .git_ignore(true)
        .git_exclude(true)
        // DELIBERATE HARNESS DEVIATION: the code plane sets `git_global(true)`.
        // That reads the developer's `core.excludesFile`, which would make
        // these assertions depend on the machine. Membership semantics under
        // test are unaffected; the plan does not say whether docs membership
        // should depend on a user's global gitignore (reported as a spec gap).
        .git_global(false)
        .filter_entry(move |entry| {
            let Some(rel) = rel_path(&filter_root, entry.path()) else {
                return true;
            };
            if rel.is_empty() {
                return true; // the indexed root itself
            }
            let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir());
            if is_dir {
                if let Some(rule) = deterministic_dir_prune(&rel) {
                    filter_log.lock().unwrap().pruned.push((rel, rule));
                    return false;
                }
            }
            if hidden_not_allowlisted(&rel) {
                let mut log = filter_log.lock().unwrap();
                if is_dir {
                    log.pruned.push((rel, DirRule::HiddenNotAllowlisted));
                } else {
                    log.files.push((rel, FileRule::HiddenNotAllowlisted));
                }
                return false;
            }
            true
        });

    for entry in builder.build() {
        let entry = entry.expect("walk entry");
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let Some(rel) = rel_path(root, entry.path()) else {
            continue;
        };
        let rule = classify_file(root, &rel, config, &include, &exclude);
        log.lock().unwrap().files.push((rel, rule));
    }

    let walked = log.lock().unwrap().clone();
    walked
}

/// Rules 3 and 4 plus the admission bound, for a file that survived traversal.
fn classify_file(
    root: &Path,
    rel: &str,
    config: &DocsConfig,
    include: &Gitignore,
    exclude: &Gitignore,
) -> FileRule {
    // Rule 3: exclude beats include.
    if exclude.matched(rel, false).is_ignore() {
        return FileRule::Excluded;
    }
    // Rule 4: include.
    if !include.matched(rel, false).is_ignore() {
        return FileRule::NotIncluded;
    }
    // "Version one admits `.md` only".
    //
    // INVENTED: the plan gives `docs status` no reason code for "matched an
    // include glob but is not Markdown", so it is folded into `not-included`.
    if !rel.ends_with(".md") {
        return FileRule::NotIncluded;
    }
    let Ok(metadata) = fs::metadata(root.join(rel)) else {
        return FileRule::NonUtf8; // unreadable; not a case these tests exercise
    };
    // INVENTED boundary: "larger than 4 MiB … are excluded" is read as strictly
    // greater, so a file of exactly 4,194,304 bytes is admitted. W5 pins both
    // sides of that boundary.
    if metadata.len() > config.max_bytes {
        return FileRule::Oversized;
    }
    match fs::read(root.join(rel)) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(_) => FileRule::Indexed,
            Err(_) => FileRule::NonUtf8,
        },
        Err(_) => FileRule::NonUtf8,
    }
}

/// A matcher over the same ignore configuration, used only to NAME the
/// deciding rule for a path the walk never enumerated.
///
/// SPEC GAP: `docs status` is supposed to report `ignored` per pruned
/// directory, but the `ignore` crate prunes ignored entries *before* any
/// caller predicate (`should_skip_entry` precedes the filter in
/// `walk.rs::skip_entry`), so an in-walk implementation cannot see them at
/// all. Naming that reason requires a second matcher pass like this one — a
/// cost the plan does not mention.
fn ignore_probe(root: &Path) -> IncrementalIgnore {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .parents(true)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(false);
    let mut matchers = builder.build_matchers();
    matchers.pop().expect("one matcher per configured root")
}

fn is_ignored(probe: &mut IncrementalIgnore, rel: &str, is_dir: bool) -> bool {
    probe.matched(rel, is_dir).is_ignore()
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

fn write(root: &Path, rel: &str, contents: &str) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent");
    }
    fs::write(path, contents).expect("write fixture");
}

/// A bare `.git` marker directory. `ignore` only checks that `.git` exists
/// (`dir.rs`: `dir.join(".git").metadata()`), which is exactly how the code
/// plane's own walker tests set up a repository.
fn git_marker(root: &Path) {
    fs::create_dir_all(root.join(".git")).expect("create .git");
}

fn tempdir() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

// ---------------------------------------------------------------------------
// W1 — exclude beats include, ignore beats both
// ---------------------------------------------------------------------------

#[test]
fn w1_exclude_beats_include_and_ignore_beats_both() {
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    write(root, ".gitignore", "ignored.md\nboth.md\n");
    write(root, "a.md", "# a\n");
    write(root, "deep/nested/b.md", "# b\n");
    write(root, "excluded.md", "# excluded\n");
    write(root, "ignored.md", "# ignored\n");
    write(root, "both.md", "# both\n");
    write(root, "notes.txt", "not markdown\n");

    // `both.md` is simultaneously gitignored, exclude-matched and
    // include-matched; `excluded.md` is simultaneously exclude- and
    // include-matched. Same file, competing rules — that is the point.
    let config = DocsConfig {
        include: vec!["**/*.md".to_string()],
        exclude: vec!["excluded.md".to_string(), "both.md".to_string()],
        ..Default::default()
    };
    let walked = docs_inventory(root, &config);
    let decisions = walked.decisions();
    println!("W1 decisions: {decisions:?}");

    // Rule 4 with the default include reaches both the root and nested depths.
    // (`**/*.md` under globset matches zero or more leading directories.)
    assert_eq!(decisions.get("a.md"), Some(&FileRule::Indexed));
    assert_eq!(decisions.get("deep/nested/b.md"), Some(&FileRule::Indexed));

    // Exclude beats include.
    assert_eq!(decisions.get("excluded.md"), Some(&FileRule::Excluded));

    // Ignore beats both: the ignored files are not merely "excluded", they are
    // never enumerated, so no rule-3/rule-4 decision exists for them.
    assert_eq!(decisions.get("ignored.md"), None);
    assert_eq!(decisions.get("both.md"), None);
    let mut probe = ignore_probe(root);
    assert!(is_ignored(&mut probe, "ignored.md", false));
    assert!(is_ignored(&mut probe, "both.md", false));

    // A non-Markdown file that survives traversal is reported, not silently
    // dropped: `docs status` needs a reason for every encountered file.
    assert_eq!(decisions.get("notes.txt"), Some(&FileRule::NotIncluded));

    assert_eq!(walked.admitted(), vec!["a.md", "deep/nested/b.md"]);
}

// ---------------------------------------------------------------------------
// W2 — the hidden allowlist actually reaches agent-facing documentation
// ---------------------------------------------------------------------------

#[test]
fn w2_hidden_allowlist_admits_agent_docs() {
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    // Real agent-facing documentation shapes.
    write(root, ".github/PULL_REQUEST_TEMPLATE.md", "# pr\n");
    write(root, ".github/ISSUE_TEMPLATE/bug.md", "# bug\n");
    write(root, ".github/copilot-instructions.md", "# copilot\n");
    write(root, ".claude/agents/reviewer.md", "# reviewer\n");
    write(root, ".claude/commands/ship.md", "# ship\n");
    write(root, ".agents/rules.md", "# rules\n");
    write(root, "README.md", "# readme\n");
    // Hidden paths outside the allowlist.
    write(root, ".notes.md", "# private note\n");
    write(root, ".private/x.md", "# private\n");
    write(root, ".cursor/rules/style.md", "# cursor\n");
    write(root, ".vscode/notes.md", "# vscode\n");

    let walked = docs_inventory(root, &DocsConfig::default());
    println!("W2 admitted: {:?}", walked.admitted());
    println!("W2 pruned:   {:?}", walked.pruned_map());

    // THE POINT: the allowlist reaches agent-facing docs at every depth under
    // the three allowlisted roots, not just their top level.
    assert_eq!(
        walked.admitted(),
        vec![
            ".agents/rules.md",
            ".claude/agents/reviewer.md",
            ".claude/commands/ship.md",
            ".github/ISSUE_TEMPLATE/bug.md",
            ".github/PULL_REQUEST_TEMPLATE.md",
            ".github/copilot-instructions.md",
            "README.md",
        ]
    );

    // A hidden FILE at the root is encountered and reported with its reason.
    assert_eq!(
        walked.decisions().get(".notes.md"),
        Some(&FileRule::HiddenNotAllowlisted)
    );

    // Hidden DIRECTORIES are reported as pruned, and nothing beneath them is
    // enumerated ("without enumerating descendants beneath pruned
    // directories").
    let pruned = walked.pruned_map();
    assert_eq!(pruned.get(".private"), Some(&DirRule::HiddenNotAllowlisted));
    assert_eq!(pruned.get(".cursor"), Some(&DirRule::HiddenNotAllowlisted));
    assert_eq!(pruned.get(".vscode"), Some(&DirRule::HiddenNotAllowlisted));
    assert!(walked.nothing_beneath(".private"));
    assert!(walked.nothing_beneath(".cursor"));

    // OBSERVATION, not a plan violation: the allowlist is exactly three
    // directories, so other agent-tool conventions in wide use today —
    // `.cursor/rules/*.md`, `.vscode`, `.devcontainer`, `.gitlab` — are
    // excluded. Documented here so the choice is visible rather than implied.
    assert_eq!(HIDDEN_ALLOWLIST, &[".github", ".claude", ".agents"]);
}

// ---------------------------------------------------------------------------
// W3 — `.git` is never traversed
// ---------------------------------------------------------------------------

#[test]
fn w3_git_directory_is_never_traversed() {
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    // Markdown planted inside the repository's own metadata directory.
    write(root, ".git/x.md", "# inside git\n");
    write(root, ".git/objects/deep/note.md", "# deep inside git\n");
    write(
        root,
        ".git/info/exclude",
        "excluded-by-git-info-exclude.md\n",
    );
    write(root, "README.md", "# readme\n");
    write(
        root,
        "excluded-by-git-info-exclude.md",
        "# repo-local exclude\n",
    );

    let walked = docs_inventory(root, &DocsConfig::default());
    println!("W3 files:  {:?}", walked.decisions());
    println!("W3 pruned: {:?}", walked.pruned_map());

    // `.git` is pruned as a hard skip, and nothing inside it is visible — not
    // as a file decision and not as a nested prune record.
    assert_eq!(walked.pruned_map().get(".git"), Some(&DirRule::HardSkip));
    assert!(walked.nothing_beneath(".git"));
    assert!(!walked
        .files
        .iter()
        .any(|(path, _)| path.starts_with(".git/")));
    assert_eq!(walked.admitted(), vec!["README.md"]);

    // The hard skip prunes TRAVERSAL only: `.git/info/exclude` is still read
    // as a repository ignore file, so rule 1 still applies through it.
    assert!(!walked
        .admitted()
        .contains(&"excluded-by-git-info-exclude.md"));
    let mut probe = ignore_probe(root);
    assert!(is_ignored(
        &mut probe,
        "excluded-by-git-info-exclude.md",
        false
    ));

    // The `.git` skip must be explicit. This is what the crate does WITHOUT a
    // hard skip when hidden filtering is off (which rule 2 forces): it walks
    // straight into the object store.
    let naive: Vec<String> = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .build()
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| rel_path(root, entry.path()))
        .filter(|rel| rel.starts_with(".git/"))
        .collect();
    assert!(
        naive.contains(&".git/x.md".to_string()),
        "ignore 0.4 does not skip .git by itself; the docs walker must: {naive:?}"
    );
}

// ---------------------------------------------------------------------------
// W4 — include cannot resurrect a gitignored file
// ---------------------------------------------------------------------------

#[test]
fn w4_include_cannot_resurrect_a_gitignored_file() {
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    write(root, ".gitignore", "generated/\nsecret.md\n");
    write(root, "secret.md", "# secret\n");
    write(root, "generated/api.md", "# generated\n");
    write(root, "docs/keep.md", "# keep\n");

    // An include list that names the ignored paths as explicitly as possible.
    let config = DocsConfig {
        include: vec![
            "**/*.md".to_string(),
            "secret.md".to_string(),
            "generated/**/*.md".to_string(),
            "generated/api.md".to_string(),
        ],
        ..Default::default()
    };
    let walked = docs_inventory(root, &config);
    println!("W4 decisions: {:?}", walked.decisions());

    assert_eq!(walked.admitted(), vec!["docs/keep.md"]);
    assert_eq!(walked.decisions().get("secret.md"), None);
    assert_eq!(walked.decisions().get("generated/api.md"), None);
    assert!(walked.nothing_beneath("generated"));

    // The include globs themselves DO match those paths — the resurrection
    // fails because rule 1 pruned them before rules 3/4 ever ran, not because
    // the patterns were wrong.
    let include = glob_matcher(root, &config.include);
    assert!(include.matched("secret.md", false).is_ignore());
    assert!(include.matched("generated/api.md", false).is_ignore());

    let mut probe = ignore_probe(root);
    assert!(is_ignored(&mut probe, "secret.md", false));
    assert!(is_ignored(&mut probe, "generated", true));
    assert!(is_ignored(&mut probe, "generated/api.md", false));
}

// ---------------------------------------------------------------------------
// W5 — the 4 MiB admission bound
// ---------------------------------------------------------------------------

#[test]
fn w5_four_mib_admission_bound() {
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    assert_eq!(
        MAX_ADMITTED_BYTES, 4_194_304,
        "the plan names 4,194,304 bytes"
    );

    // Just under / exactly at / just over the bound.
    fs::write(root.join("exact.md"), vec![b'a'; 4_194_304]).expect("write exact");
    fs::write(root.join("under.md"), vec![b'a'; 4_194_303]).expect("write under");
    fs::write(root.join("over.md"), vec![b'a'; 4_194_305]).expect("write over");
    // The remaining `docs status` reason in the same vocabulary.
    fs::write(root.join("bad.md"), [b'#', b' ', 0xff, 0xfe, b'\n']).expect("write bad");

    let walked = docs_inventory(root, &DocsConfig::default());
    let decisions = walked.decisions();
    println!("W5 decisions: {decisions:?}");

    assert_eq!(decisions.get("under.md"), Some(&FileRule::Indexed));
    // "larger than 4 MiB … are excluded" — 4,194,304 is not larger, so it is
    // admitted. This boundary is INVENTED (the plan gives one number, not a
    // comparison operator); both sides are pinned so a change is visible.
    assert_eq!(decisions.get("exact.md"), Some(&FileRule::Indexed));
    assert_eq!(decisions.get("over.md"), Some(&FileRule::Oversized));
    assert_eq!(decisions.get("bad.md"), Some(&FileRule::NonUtf8));

    // Oversized is a REPORTED decision, not a silent drop: the file is
    // enumerated and named. (`ignore`'s own `max_filesize` would have dropped
    // it invisibly instead, which is why the bound is applied by the caller.)
    assert!(walked.files.iter().any(|(path, _)| path == "over.md"));
    assert_eq!(walked.admitted(), vec!["exact.md", "under.md"]);
}

// ---------------------------------------------------------------------------
// W6 — determinism
// ---------------------------------------------------------------------------

#[test]
fn w6_repeated_walks_are_identical() {
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    write(root, ".gitignore", "vendor/\n*.draft.md\n");
    write(root, ".git/info/exclude", "local-only.md\n");
    write(root, "README.md", "# readme\n");
    write(root, "local-only.md", "# local\n");
    write(root, "notes.draft.md", "# draft\n");
    write(root, ".notes.md", "# hidden\n");
    write(root, ".github/workflows/ci.md", "# ci\n");
    write(root, ".claude/agents/a.md", "# a\n");
    write(root, ".agents/rules.md", "# rules\n");
    write(root, ".private/secret.md", "# secret\n");
    write(root, "vendor/lib/doc.md", "# vendor\n");
    write(root, "node_modules/pkg/readme.md", "# dep\n");
    write(root, "dist/out.md", "# dist\n");
    write(root, "docs/a.md", "# a\n");
    write(root, "docs/b.md", "# b\n");
    write(root, "docs/deep/c.md", "# c\n");
    write(root, "docs/deep/deeper/d.md", "# d\n");
    write(root, "docs/skip.txt", "text\n");
    write(root, "excluded/e.md", "# e\n");

    let config = DocsConfig {
        include: vec!["**/*.md".to_string()],
        exclude: vec!["excluded/**".to_string()],
        ..Default::default()
    };

    let first = docs_inventory(root, &config);
    println!(
        "W6 traversal order: {:?}",
        first.files.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    for round in 1..8 {
        let again = docs_inventory(root, &config);
        // Identical DECISIONS in identical ORDER — not merely the same set.
        assert_eq!(again, first, "walk {round} differed from walk 0");
    }

    // ...but "identical order" only holds for THE SAME TREE. Traversal order
    // is the filesystem's readdir order, not a content-determined order. Two
    // trees with byte-identical contents, created in different orders, are
    // walked in different orders. The code plane copes by sorting
    // (`walk.rs::source_inventory` ends with `files.sort()`); the docs plan
    // never says the corpus is canonically ordered, and its snapshot
    // "corpus fingerprint" would differ between two identical checkouts if it
    // were computed in traversal order.
    let forward = tempdir();
    let backward = tempdir();
    let names = [
        "a.md", "b.md", "c.md", "d.md", "e.md", "f.md", "g.md", "h.md",
    ];
    git_marker(forward.path());
    git_marker(backward.path());
    for name in names {
        write(forward.path(), name, "# x\n");
    }
    for name in names.iter().rev() {
        write(backward.path(), name, "# x\n");
    }
    let forward_walk = docs_inventory(forward.path(), &DocsConfig::default());
    let backward_walk = docs_inventory(backward.path(), &DocsConfig::default());
    let forward_order: Vec<&str> = forward_walk.files.iter().map(|(p, _)| p.as_str()).collect();
    let backward_order: Vec<&str> = backward_walk
        .files
        .iter()
        .map(|(p, _)| p.as_str())
        .collect();
    println!("W6 creation-order forward:  {forward_order:?}");
    println!("W6 creation-order backward: {backward_order:?}");
    // The canonical (sorted) view is identical either way — that is the
    // determinism a corpus fingerprint can rely on.
    assert_eq!(forward_walk.admitted(), backward_walk.admitted());
    assert_eq!(forward_walk.decisions(), backward_walk.decisions());
    // ACTUAL observed behavior (macOS/APFS): raw traversal order is NOT
    // creation-order dependent — APFS orders directory entries by a hash of
    // the name — so the two trees are walked identically. But that order is
    // also NOT lexicographic: it is a filesystem artifact, so "identical
    // order" is a per-filesystem property, not a property of the corpus.
    // ext4 without dir_index, for instance, returns creation order.
    assert_eq!(
        forward_order, backward_order,
        "APFS name-hash readdir order"
    );
    let mut lexicographic = forward_order.clone();
    lexicographic.sort_unstable();
    assert_ne!(
        forward_order, lexicographic,
        "traversal order is a filesystem artifact, not a sorted corpus order; \
         a canonical corpus needs an explicit sort like walk.rs's files.sort()"
    );
    println!("W6 lexicographic order would be: {lexicographic:?}");

    // The interesting content is present, so the determinism claim is not
    // vacuously true over an empty tree.
    assert_eq!(
        first.admitted(),
        vec![
            ".agents/rules.md",
            ".claude/agents/a.md",
            ".github/workflows/ci.md",
            "README.md",
            "docs/a.md",
            "docs/b.md",
            "docs/deep/c.md",
            "docs/deep/deeper/d.md",
        ]
    );
    assert_eq!(
        first.decisions().get("excluded/e.md"),
        Some(&FileRule::Excluded)
    );
    assert_eq!(
        first.pruned_map().get("node_modules"),
        Some(&DirRule::HardSkip)
    );
    assert_eq!(first.pruned_map().get("dist"), Some(&DirRule::HardSkip));
    assert_eq!(
        first.pruned_map().get(".private"),
        Some(&DirRule::HiddenNotAllowlisted)
    );
    // `vendor/` is gitignored: pruned by rule 1, therefore invisible to the
    // walker's own prune log. See the SPEC GAP on `ignore_probe`.
    assert_eq!(first.pruned_map().get("vendor"), None);
    assert!(first.nothing_beneath("vendor"));
}

// ---------------------------------------------------------------------------
// W7 — glob anchoring when the indexed root is a subdirectory
// ---------------------------------------------------------------------------

#[test]
fn w7_glob_anchoring_with_indexed_root_below_the_repository_root() {
    let dir = tempdir();
    let repo = dir.path();
    git_marker(repo);
    write(repo, ".gitignore", "drafts/\n");
    write(repo, "docs/guide.md", "# guide\n");
    write(repo, "docs/api/v2/spec.md", "# spec\n");
    write(repo, "docs/drafts/wip.md", "# wip\n");
    write(repo, "outside.md", "# outside the indexed root\n");
    write(repo, "other/also-outside.md", "# outside\n");

    let indexed_root = repo.join("docs");

    // (a) Repository ignore files above the indexed root still prune, because
    // the walker keeps `parents(true)` like the code plane. `drafts/` is
    // written in the REPOSITORY's .gitignore, one level above the indexed root.
    let default_walk = docs_inventory(&indexed_root, &DocsConfig::default());
    println!("W7 default admitted: {:?}", default_walk.admitted());
    assert_eq!(default_walk.admitted(), vec!["api/v2/spec.md", "guide.md"]);
    assert!(default_walk.nothing_beneath("drafts"));
    // Nothing above the indexed root is enumerated at all.
    assert!(!default_walk
        .files
        .iter()
        .any(|(path, _)| path.contains("outside")));

    // (b) Globs anchored at the INDEXED ROOT work as specified.
    let anchored = DocsConfig {
        exclude: vec!["api/**".to_string()],
        ..Default::default()
    };
    let anchored_walk = docs_inventory(&indexed_root, &anchored);
    assert_eq!(
        anchored_walk.decisions().get("api/v2/spec.md"),
        Some(&FileRule::Excluded)
    );
    assert_eq!(anchored_walk.admitted(), vec!["guide.md"]);

    // (c) VIOLATED — the plan is internally inconsistent here. Membership
    // globs are "anchored at the indexed root" (Membership, rules 3-4) while
    // the `path` field a hit exposes is "repository-relative" (Field
    // composition table). When the indexed root is a subdirectory those two
    // strings are DIFFERENT, so an exclude glob written against the path a
    // user actually sees in search results silently matches nothing.
    let repo_relative = rel_path(repo, &indexed_root.join("api/v2/spec.md")).unwrap();
    let root_relative = rel_path(&indexed_root, &indexed_root.join("api/v2/spec.md")).unwrap();
    assert_eq!(repo_relative, "docs/api/v2/spec.md");
    assert_eq!(root_relative, "api/v2/spec.md");
    assert_ne!(repo_relative, root_relative);

    let repo_shaped = DocsConfig {
        exclude: vec!["docs/api/**".to_string()],
        ..Default::default()
    };
    let repo_shaped_walk = docs_inventory(&indexed_root, &repo_shaped);
    // ACTUAL behavior: the file stays indexed. The exclude glob is a no-op.
    assert_eq!(
        repo_shaped_walk.decisions().get("api/v2/spec.md"),
        Some(&FileRule::Indexed)
    );
    assert_eq!(
        repo_shaped_walk.admitted(),
        vec!["api/v2/spec.md", "guide.md"]
    );
    println!(
        "W7 anchoring gap: exclude 'docs/api/**' did not exclude {repo_relative} \
         (matched against '{root_relative}')"
    );

    // (d) The mirror-image trap: a glob written against the indexed root
    // accidentally matches a DIFFERENT repository path when the same config is
    // used at the repository root. Same pattern, two roots, two answers.
    let at_repo_root = docs_inventory(repo, &anchored);
    assert_eq!(
        at_repo_root.decisions().get("docs/api/v2/spec.md"),
        Some(&FileRule::Indexed)
    );
    assert!(at_repo_root.admitted().contains(&"docs/api/v2/spec.md"));
}

// ---------------------------------------------------------------------------
// W8 — nested ignore files and negation
// ---------------------------------------------------------------------------

#[test]
fn w8_nested_gitignore_and_negation_follow_ignore_crate_semantics() {
    // (a) A nested .gitignore can re-include a file its parent ignored.
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    write(root, ".gitignore", "*.md\n");
    write(root, "docs/.gitignore", "!keep.md\n");
    write(root, "docs/keep.md", "# keep\n");
    write(root, "docs/drop.md", "# drop\n");
    write(root, "top.md", "# top\n");

    let walked = docs_inventory(root, &DocsConfig::default());
    println!("W8a admitted: {:?}", walked.admitted());
    assert_eq!(walked.admitted(), vec!["docs/keep.md"]);
    assert_eq!(walked.decisions().get("docs/drop.md"), None);
    assert_eq!(walked.decisions().get("top.md"), None);

    // (b) A negation cannot re-include a file whose PARENT DIRECTORY is
    // excluded — the directory is pruned before its .gitignore is ever read.
    // This is git's documented rule and the ignore crate reproduces it.
    let dir_b = tempdir();
    let root_b = dir_b.path();
    git_marker(root_b);
    write(root_b, ".gitignore", "secret/\n");
    write(root_b, "secret/.gitignore", "!allowed.md\n");
    write(root_b, "secret/allowed.md", "# allowed\n");
    write(root_b, "open.md", "# open\n");

    let walked_b = docs_inventory(root_b, &DocsConfig::default());
    println!("W8b admitted: {:?}", walked_b.admitted());
    assert_eq!(walked_b.admitted(), vec!["open.md"]);
    assert!(walked_b.nothing_beneath("secret"));
    let mut probe_b = ignore_probe(root_b);
    assert!(is_ignored(&mut probe_b, "secret", true));
    assert!(is_ignored(&mut probe_b, "secret/allowed.md", false));

    // (c) VIOLATED for the non-Git case. The plan says rule 1 prunes with
    // "repository ignore files"; the code plane (and therefore this walker)
    // keeps the ignore crate's `require_git` default, so in a directory with
    // NO `.git`, `.gitignore` is completely inert while `.ignore` still
    // applies. G24 explicitly serves non-Git repositories ("including non-Git
    // repositories" under freshness), so a non-Git documentation tree gets no
    // gitignore filtering at all.
    let dir_c = tempdir();
    let root_c = dir_c.path();
    // deliberately NO git_marker
    write(root_c, ".gitignore", "gitignored/\n");
    write(root_c, ".ignore", "dot-ignored/\n");
    write(root_c, "gitignored/a.md", "# a\n");
    write(root_c, "dot-ignored/b.md", "# b\n");
    write(root_c, "kept.md", "# kept\n");

    let walked_c = docs_inventory(root_c, &DocsConfig::default());
    println!("W8c (no .git) admitted: {:?}", walked_c.admitted());
    // ACTUAL: `.gitignore` did NOT prune; `.ignore` did.
    assert_eq!(walked_c.admitted(), vec!["gitignored/a.md", "kept.md"]);
    assert!(walked_c.nothing_beneath("dot-ignored"));

    // The same tree WITH a `.git` marker prunes both — proving the difference
    // is `require_git`, not the pattern.
    git_marker(root_c);
    let walked_c_git = docs_inventory(root_c, &DocsConfig::default());
    println!("W8c (with .git) admitted: {:?}", walked_c_git.admitted());
    assert_eq!(walked_c_git.admitted(), vec!["kept.md"]);
}

// ---------------------------------------------------------------------------
// W9 — "root-level" allowlist, taken literally
// ---------------------------------------------------------------------------

#[test]
fn w9_allowlist_is_root_level_only() {
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    write(root, ".gitignore", "vendor/\n");
    // Root-level allowlisted directories.
    write(root, ".github/workflows/ci-notes.md", "# ci\n");
    write(root, ".claude/agents/a.md", "# a\n");
    write(root, ".agents/rules.md", "# rules\n");
    // Nested copies of the same names, deeper in the tree.
    write(root, "packages/app/.github/nested.md", "# nested gh\n");
    write(root, "packages/app/.claude/nested.md", "# nested claude\n");
    write(root, "packages/app/.agents/nested.md", "# nested agents\n");
    // An allowlisted name inside an ignored directory.
    write(root, "vendor/.claude/x.md", "# vendored claude\n");
    // A hidden directory nested INSIDE an allowlisted root directory.
    write(root, ".github/.private/deep.md", "# deep\n");
    write(root, "packages/app/README.md", "# app\n");

    let walked = docs_inventory(root, &DocsConfig::default());
    println!("W9 admitted: {:?}", walked.admitted());
    println!("W9 pruned:   {:?}", walked.pruned_map());

    // ACTUAL behavior, matching a literal reading of "root-level": only the
    // three directories directly under the indexed root are excused.
    assert_eq!(
        walked.admitted(),
        vec![
            ".agents/rules.md",
            ".claude/agents/a.md",
            ".github/workflows/ci-notes.md",
            "packages/app/README.md",
        ]
    );

    // Nested `.github` / `.claude` / `.agents` are pruned as hidden. The plan
    // says the allowlist is "root-level" and "All other hidden paths remain
    // excluded", so this is what it specifies — but the CONSEQUENCE is worth
    // stating: in a monorepo, per-package agent instructions are invisible to
    // documentation search while the root-level ones are indexed.
    let pruned = walked.pruned_map();
    assert_eq!(
        pruned.get("packages/app/.github"),
        Some(&DirRule::HiddenNotAllowlisted)
    );
    assert_eq!(
        pruned.get("packages/app/.claude"),
        Some(&DirRule::HiddenNotAllowlisted)
    );
    assert_eq!(
        pruned.get("packages/app/.agents"),
        Some(&DirRule::HiddenNotAllowlisted)
    );
    assert!(walked.nothing_beneath("packages/app/.github"));

    // A hidden directory nested inside an allowlisted root directory is
    // pruned. INVENTED: the plan does not say whether the allowlist excuses
    // only its own component or the whole subtree.
    assert_eq!(
        pruned.get(".github/.private"),
        Some(&DirRule::HiddenNotAllowlisted)
    );
    assert!(walked.nothing_beneath(".github/.private"));

    // `vendor/` is gitignored, so rule 1 decides before rule 2 is consulted:
    // an allowlisted NAME inside an ignored directory stays excluded, and the
    // prune is invisible to the walker (see the SPEC GAP on `ignore_probe`).
    assert_eq!(pruned.get("vendor"), None);
    assert!(walked.nothing_beneath("vendor"));
    let mut probe = ignore_probe(root);
    assert!(is_ignored(&mut probe, "vendor", true));
    assert!(is_ignored(&mut probe, "vendor/.claude/x.md", false));

    // And rule 1 beats rule 2 even for the root-level allowlisted directory
    // itself: gitignoring `.claude/` removes it from the corpus.
    let dir_b = tempdir();
    let root_b = dir_b.path();
    git_marker(root_b);
    write(root_b, ".gitignore", ".claude/\n");
    write(root_b, ".claude/a.md", "# a\n");
    write(root_b, ".github/b.md", "# b\n");
    let walked_b = docs_inventory(root_b, &DocsConfig::default());
    println!("W9b admitted: {:?}", walked_b.admitted());
    assert_eq!(walked_b.admitted(), vec![".github/b.md"]);
    assert!(walked_b.nothing_beneath(".claude"));
}

// ---------------------------------------------------------------------------
// Cross-cutting: the ordering that makes "ignore beats both" structural
// ---------------------------------------------------------------------------

#[test]
fn ignored_dirs_are_invisible_to_the_walk_filter() {
    // Evidence for the SPEC GAP on `docs status`'s per-pruned-directory
    // `ignored` reason: `ignore::WalkBuilder`'s `filter_entry` predicate is
    // never called for an entry the ignore matcher already rejected, so an
    // in-walk implementation cannot report `ignored` at all.
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    write(root, ".gitignore", "ignored-dir/\n");
    write(root, "ignored-dir/a.md", "# a\n");
    write(root, "kept.md", "# kept\n");

    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let filter_seen = Arc::clone(&seen);
    let filter_root = root.to_path_buf();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .filter_entry(move |entry| {
            if let Some(rel) = rel_path(&filter_root, entry.path()) {
                filter_seen.lock().unwrap().push(rel);
            }
            true
        })
        .build();
    for entry in walker {
        entry.expect("walk entry");
    }

    let seen = Arc::try_unwrap(seen).unwrap().into_inner().unwrap();
    println!("filter_entry saw: {seen:?}");
    assert!(seen.iter().any(|rel| rel == "kept.md"));
    // ACTUAL: the ignored directory never reaches the predicate.
    assert!(!seen.iter().any(|rel| rel == "ignored-dir"));
    assert!(!seen.iter().any(|rel| rel.starts_with("ignored-dir/")));
}

#[test]
fn exclude_globs_match_files_only_and_inherit_the_dialect() {
    // The plan says rule 3 is "`exclude` globs, anchored at the indexed root,
    // matching files". Taken literally — and implemented literally — a
    // directory-shaped pattern silently does nothing, because it only ever
    // matches the directory entry, which rule 3 never consults.
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    write(root, "drafts/wip.md", "# wip\n");
    write(root, "keep.md", "# keep\n");

    let dir_shaped = DocsConfig {
        exclude: vec!["drafts/".to_string()],
        ..Default::default()
    };
    let walked = docs_inventory(root, &dir_shaped);
    println!(
        "W-extra exclude 'drafts/' admitted: {:?}",
        walked.admitted()
    );
    // ACTUAL: the file is still indexed.
    assert_eq!(
        walked.decisions().get("drafts/wip.md"),
        Some(&FileRule::Indexed)
    );

    let matcher = glob_matcher(root, &dir_shaped.exclude);
    assert!(
        matcher.matched("drafts", true).is_ignore(),
        "matches the directory"
    );
    assert!(
        !matcher.matched("drafts/wip.md", false).is_ignore(),
        "but not the file under it"
    );
    // The pattern the user meant:
    let subtree = DocsConfig {
        exclude: vec!["drafts/**".to_string()],
        ..Default::default()
    };
    assert_eq!(
        docs_inventory(root, &subtree)
            .decisions()
            .get("drafts/wip.md"),
        Some(&FileRule::Excluded)
    );

    // The glob dialect is not neutral: `!` is a negation in gitignore/globset
    // syntax, so an entry in a list the plan calls "exclude globs" can mean
    // "do NOT exclude". Recorded so the dialect choice is an explicit decision.
    let negated = DocsConfig {
        exclude: vec!["**/*.md".to_string(), "!keep.md".to_string()],
        ..Default::default()
    };
    let negated_walk = docs_inventory(root, &negated);
    println!(
        "W-extra negated exclude admitted: {:?}",
        negated_walk.admitted()
    );
    assert_eq!(negated_walk.admitted(), vec!["keep.md"]);
}

#[test]
fn membership_rules_are_evaluated_in_plan_order_on_one_file() {
    // One file, all four rules applicable, checked one rule at a time by
    // removing the higher-precedence rule and watching the decision change.
    let dir = tempdir();
    let root = dir.path();
    git_marker(root);
    write(root, "doc.md", "# doc\n");

    let include_only = DocsConfig::default();
    assert_eq!(
        docs_inventory(root, &include_only)
            .decisions()
            .get("doc.md"),
        Some(&FileRule::Indexed)
    );

    // Rule 4 alone: not matched by include.
    let narrow = DocsConfig {
        include: vec!["other/**".to_string()],
        ..Default::default()
    };
    assert_eq!(
        docs_inventory(root, &narrow).decisions().get("doc.md"),
        Some(&FileRule::NotIncluded)
    );

    // Rule 3 beats rule 4.
    let excluded = DocsConfig {
        exclude: vec!["doc.md".to_string()],
        ..Default::default()
    };
    assert_eq!(
        docs_inventory(root, &excluded).decisions().get("doc.md"),
        Some(&FileRule::Excluded)
    );

    // Rule 1 beats rules 3 and 4: no decision row at all.
    write(root, ".gitignore", "doc.md\n");
    assert_eq!(
        docs_inventory(root, &excluded).decisions().get("doc.md"),
        None
    );
    assert_eq!(
        docs_inventory(root, &include_only)
            .decisions()
            .get("doc.md"),
        None
    );

    // Rule 2's hidden policy beats rules 3 and 4 too, and sits below rule 1.
    let dir_b = tempdir();
    let root_b = dir_b.path();
    git_marker(root_b);
    write(root_b, ".hidden/doc.md", "# doc\n");
    let walked = docs_inventory(root_b, &include_only);
    assert!(walked.admitted().is_empty());
    assert_eq!(
        walked.pruned_map().get(".hidden"),
        Some(&DirRule::HiddenNotAllowlisted)
    );
}
