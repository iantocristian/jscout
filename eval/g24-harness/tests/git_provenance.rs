//! G24 "Git provenance" assumptions, tested against a real `git` (2.49.0).
//!
//! METHOD: every test here asks whether the PLAN is true, not whether an
//! implementation is convenient. Where real git contradicts the plan, the test
//! keeps an assertion that pins the ACTUAL behavior and a comment naming the
//! plan claim it falsifies. Nothing is weakened to stay green.
//!
//! Plan claims under test (docs/plans/g24-…-2026-08-24.md, "Git provenance"):
//!
//! > both author and committer times are stored; "newest" for freshness means
//! > the latest author time among contributing body lines, because author time
//! > survives rebase and cherry-pick while committer time is rewritten to the
//! > integration date;                                                     (G1)
//! > shallow-clone boundary commits contribute no timestamp; a chunk whose
//! > contributing lines all blame to a boundary commit has unknown git age;
//! >                                                                  (G2, G2b)
//! > provenance Git commands disable replacement objects with
//! > `--no-replace-objects`, and blame clears repository `blame.ignoreRevsFile`
//! > configuration with `-c blame.ignoreRevsFile=`;                    (G3, G4)
//! > the blame mapping cache key includes the repository-relative path, a hash
//! > of the exact file bytes being blamed, the newest commit touching that
//! > path, and the shallow boundary fingerprint;              (G9-G13 support)
//! > modified lines in an already tracked file are labelled `working_tree`
//! > whether staged or unstaged … without inventing a commit;         (G5, G6)
//! > newly added staged files and untracked files have no Git authorship time;
//! >                                                                  (G7, G8)
//! > filesystem modification time is never a fallback;              (G9 rider)
//! > Git absence or a per-file blame failure … degrades that file to
//! > observed/unknown provenance without failing the scan.                (G7)

use anyhow::Result;
use g24_harness::git::{self, BlameLine, GitLab};
use g24_harness::md::hash_hex;
use std::path::Path;

// ---------------------------------------------------------------------------
// Private helpers. The core deliberately does not ship these, and the task
// forbids touching src/, so they live here.
// ---------------------------------------------------------------------------

fn now_epoch() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before the unix epoch")
        .as_secs() as i64
}

/// The single blame line whose content contains `needle`. Panics with the whole
/// blame when it is missing, so a failure shows what git actually produced.
fn line_with<'a>(lines: &'a [BlameLine], needle: &str) -> &'a BlameLine {
    let mut hits = lines.iter().filter(|line| line.content.contains(needle));
    let found = hits.next().unwrap_or_else(|| {
        panic!(
            "no blame line containing {needle:?}; blame was {:?}",
            lines.iter().map(|l| l.content.as_str()).collect::<Vec<_>>()
        )
    });
    assert!(
        hits.next().is_none(),
        "{needle:?} matched more than one blame line; the test needs a unique anchor"
    );
    found
}

/// The plan's freshness value for a git-basis chunk: "the latest author time
/// among contributing body lines".
fn newest_author_time(lines: &[BlameLine]) -> i64 {
    lines
        .iter()
        .map(|line| line.author_time)
        .max()
        .expect("empty blame")
}

/// The plan's blame-cache key, assembled exactly as specified: repository-
/// relative path, hash of the exact file bytes being blamed, newest commit
/// touching that path, shallow boundary fingerprint.
/// `git status --porcelain` with its leading column preserved. `GitLab::git`
/// trims, which would erase the difference between " M" (unstaged) and "M "
/// (staged) — exactly the distinction G5/G6 turn on.
fn status_porcelain(lab: &GitLab) -> String {
    let out = lab.git_raw(&["status", "--porcelain"]);
    assert!(out.ok, "git status failed: {}", out.combined());
    out.stdout
}

fn plan_cache_key(repo: &Path, rel: &str) -> Result<String> {
    let bytes = std::fs::read(repo.join(rel))?;
    Ok(format!(
        "path={rel}|bytes={}|tip={}|shallow={}",
        hash_hex(&bytes),
        git::path_tip_commit(repo, rel)?.unwrap_or_else(|| "-".into()),
        git::shallow_boundary_fingerprint(repo)?.unwrap_or_else(|| "-".into()),
    ))
}

/// Build a replacement commit object for `target` with its `author` header
/// rewritten, and register it as a `git replace` ref. Returns the new object id.
fn install_author_replacement(lab: &GitLab, target: &str, author_header: &str) -> Result<String> {
    let original = lab.git(&["cat-file", "commit", target])?;
    let rewritten: Vec<String> = original
        .lines()
        .map(|line| {
            if line.starts_with("author ") {
                author_header.to_string()
            } else {
                line.to_string()
            }
        })
        .collect();
    // `GitLab::git` trims, so restore the trailing newline of the message.
    let text = format!("{}\n", rewritten.join("\n"));
    let scratch = lab.path().join(".replacement-commit-object");
    std::fs::write(&scratch, text)?;
    let new_oid = lab.git(&[
        "hash-object",
        "-t",
        "commit",
        "-w",
        ".replacement-commit-object",
    ])?;
    std::fs::remove_file(&scratch)?;
    lab.git(&["replace", target, new_oid.as_str()])?;
    Ok(new_oid)
}

// ---------------------------------------------------------------------------
// G1 — author time survives rebase and cherry-pick; committer time is rewritten
// ---------------------------------------------------------------------------

#[test]
fn g1_author_time_survives_rebase_and_cherry_pick() -> Result<()> {
    const AUTHORED: i64 = 1_600_001_000; // 2020-09-13
    const COMMITTED: i64 = 1_600_001_500;
    let test_start = now_epoch();

    // ---- rebase ----
    let lab = GitLab::init()?;
    lab.write("base.md", "# Base\n")?;
    lab.commit_at("base", 1_600_000_000, 1_600_000_000)?;
    lab.git(&["checkout", "--quiet", "-b", "feature"])?;
    lab.write("docs/guide.md", "the original claim\n")?;
    let pre_rebase_sha = lab.commit_at("feat", AUTHORED, COMMITTED)?;

    let before = git::blame_porcelain(lab.path(), "docs/guide.md", &[])?;
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].author_time, AUTHORED);
    assert_eq!(before[0].committer_time, COMMITTED);

    lab.git(&["checkout", "--quiet", "main"])?;
    lab.write("other.md", "unrelated churn\n")?;
    lab.commit_at("other", 1_600_003_000, 1_600_003_000)?;
    lab.git(&["checkout", "--quiet", "feature"])?;
    lab.git(&["rebase", "--quiet", "main"])?;

    let after = git::blame_porcelain(lab.path(), "docs/guide.md", &[])?;
    assert_eq!(after.len(), 1);
    let rebased = &after[0];
    assert_ne!(
        rebased.sha, pre_rebase_sha,
        "rebase must rewrite the commit id"
    );

    // PLAN CLAIM, HOLDS: author time survives the rewrite verbatim.
    assert_eq!(
        rebased.author_time, AUTHORED,
        "rebase changed the author time; the plan's freshness anchor would be unusable"
    );
    // PLAN CLAIM, HOLDS: committer time is rewritten to the integration date.
    assert_ne!(rebased.committer_time, COMMITTED);
    assert!(
        rebased.committer_time >= test_start,
        "committer time {} should be the rebase wall clock (>= {test_start})",
        rebased.committer_time
    );
    println!(
        "G1 rebase: author {} kept; committer {} -> {} (+{} s of fake recency avoided)",
        rebased.author_time,
        COMMITTED,
        rebased.committer_time,
        rebased.committer_time - COMMITTED
    );

    // ---- cherry-pick (separate lab so the evidence cannot be confused) ----
    let cp = GitLab::init()?;
    cp.write("base.md", "# Base\n")?;
    cp.commit_at("base", 1_600_000_000, 1_600_000_000)?;
    cp.git(&["checkout", "--quiet", "-b", "topic"])?;
    cp.write("docs/note.md", "cherry picked claim\n")?;
    let picked = cp.commit_at("topic work", AUTHORED, COMMITTED)?;
    cp.git(&["checkout", "--quiet", "main"])?;
    cp.write("sidecar.md", "divergence\n")?;
    cp.commit_at("sidecar", 1_600_002_000, 1_600_002_000)?;
    cp.git(&["cherry-pick", picked.as_str()])?;

    let picked_blame = git::blame_porcelain(cp.path(), "docs/note.md", &[])?;
    assert_eq!(picked_blame.len(), 1);
    assert_ne!(
        picked_blame[0].sha, picked,
        "cherry-pick must produce a new commit id"
    );
    assert_eq!(
        picked_blame[0].author_time, AUTHORED,
        "cherry-pick lost the author time"
    );
    assert!(picked_blame[0].committer_time >= test_start);
    println!(
        "G1 cherry-pick: author {} kept; committer rewritten to {}",
        picked_blame[0].author_time, picked_blame[0].committer_time
    );

    // The plan's actual decision, restated as an assertion: had freshness used
    // committer time, a five-year-old paragraph would rank as written today.
    assert!(picked_blame[0].committer_time - picked_blame[0].author_time > 5 * 365 * 24 * 3600);
    Ok(())
}

// ---------------------------------------------------------------------------
// G2 — shallow clones: boundary attribution and its timestamp lie
// ---------------------------------------------------------------------------

/// Four commits, each appending one line, one thousand seconds apart.
fn layered_history() -> Result<GitLab> {
    let lab = GitLab::init()?;
    let mut body = String::new();
    for step in 1..=4 {
        body.push_str(&format!("line{step}\n"));
        lab.write("doc.md", &body)?;
        let epoch = 1_600_000_000 + step * 1_000;
        lab.commit_at(&format!("c{step}"), epoch, epoch)?;
    }
    Ok(lab)
}

#[test]
fn g2_shallow_boundary_commits_absorb_and_misdate_older_lines() -> Result<()> {
    let lab = layered_history()?;

    let full = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    assert_eq!(full.len(), 4);
    assert_eq!(line_with(&full, "line1").author_time, 1_600_001_000);
    assert_eq!(line_with(&full, "line4").author_time, 1_600_004_000);
    assert_eq!(
        git::shallow_boundary_fingerprint(lab.path())?,
        None,
        "the source repository is complete, so it has no shallow fingerprint"
    );

    // ---- depth = 1 ----
    let d1 = lab.clone_shallow(1)?;
    let head = d1.head()?;
    let b1 = git::blame_porcelain(d1.path(), "doc.md", &[])?;
    assert_eq!(b1.len(), 4);
    for line in &b1 {
        // PLAN CLAIM, HOLDS: every pre-boundary line collapses onto the boundary
        // commit and the porcelain output carries the `boundary` marker.
        assert_eq!(line.sha, head, "depth=1 must attribute every line to HEAD");
        assert!(
            line.boundary,
            "depth=1 line {:?} lacks the boundary marker",
            line.content
        );
        assert_eq!(line.author_time, 1_600_004_000);
    }

    // The lie, quantified: line1 was authored at c1 but a depth=1 clone dates it
    // at c4. Trusting a boundary timestamp reports content 3000 s newer than it
    // is — which is exactly why the plan refuses to use it.
    let truth = line_with(&full, "line1").author_time;
    let shallow_claim = line_with(&b1, "line1").author_time;
    assert!(shallow_claim > truth);
    assert_eq!(shallow_claim - truth, 3_000);
    println!(
        "G2 depth=1: line1 truly authored {truth}, boundary claims {shallow_claim} (+{} s)",
        shallow_claim - truth
    );

    // ---- depth = N (N = 2) ----
    let d2 = lab.clone_shallow(2)?;
    let b2 = git::blame_porcelain(d2.path(), "doc.md", &[])?;
    assert_eq!(b2.len(), 4);
    let boundary_sha = std::fs::read_to_string(git::git_dir(d2.path())?.join("shallow"))?
        .trim()
        .to_string();
    for needle in ["line1", "line2", "line3"] {
        let line = line_with(&b2, needle);
        assert_eq!(
            line.sha, boundary_sha,
            "{needle} should collapse onto the depth=2 boundary"
        );
        assert!(line.boundary);
        assert_eq!(line.author_time, 1_600_003_000);
    }
    let newest = line_with(&b2, "line4");
    assert!(
        !newest.boundary,
        "the tip commit is inside the depth=2 window"
    );
    assert_eq!(newest.author_time, 1_600_004_000);

    // The plan's rule ("a chunk whose contributing lines ALL blame to a boundary
    // commit has unknown git age") is exactly right here: a chunk over lines 1-3
    // is all-boundary and must be unknown, while a chunk containing line4 has a
    // usable newest author time.
    let all_boundary_chunk: Vec<BlameLine> =
        b2.iter().filter(|l| l.final_line <= 3).cloned().collect();
    assert!(all_boundary_chunk.iter().all(|l| l.boundary));
    let mixed_chunk: Vec<BlameLine> = b2.to_vec();
    assert!(mixed_chunk.iter().any(|l| !l.boundary));
    assert_eq!(newest_author_time(&mixed_chunk), 1_600_004_000);

    println!("G2 depth=2: boundary {boundary_sha} absorbs lines 1-3; line4 keeps its own commit");
    Ok(())
}

// ---------------------------------------------------------------------------
// G2b — the boundary marker is NOT a shallowness test
// ---------------------------------------------------------------------------

#[test]
fn g2b_boundary_marker_also_marks_the_root_commit_of_a_complete_repository() -> Result<()> {
    let lab = layered_history()?;
    let full = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    let root_line = line_with(&full, "line1");

    // OBSERVED (git 2.49.0), CONTRADICTS a literal reading of the plan: in a
    // COMPLETE repository the root commit's lines also carry `boundary`, because
    // the marker means "this commit has no examined parent", not "shallow".
    assert!(
        root_line.boundary,
        "git no longer marks the root commit as a boundary; the plan's rule would need rechecking"
    );
    assert_eq!(
        root_line.author_time, 1_600_001_000,
        "yet its timestamp is perfectly real"
    );
    assert_eq!(
        git::shallow_boundary_fingerprint(lab.path())?,
        None,
        "and the repository is not shallow at all"
    );

    // Consequence: implementing "all contributing lines are boundary => unknown
    // git age" on the marker alone would throw away the true authorship time of
    // every line still owned by an initial commit. The shallow fingerprint, not
    // the marker, is the shallowness test.
    let non_root = line_with(&full, "line4");
    assert!(!non_root.boundary);

    println!(
        "G2b: root-commit line carries boundary=true in a full clone (sha {}), author_time {}",
        &root_line.sha[..8],
        root_line.author_time
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// G3 — replacement objects vs --no-replace-objects
// ---------------------------------------------------------------------------

#[test]
fn g3_replace_ref_rewrites_blame_attribution_until_no_replace_objects() -> Result<()> {
    let lab = layered_history()?;
    let commits: Vec<String> = lab
        .git(&["log", "--format=%H", "--reverse"])?
        .lines()
        .map(str::to_string)
        .collect();
    let c2 = commits[1].clone();

    let baseline = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    assert_eq!(line_with(&baseline, "line2").author, "Harness Author");
    assert_eq!(line_with(&baseline, "line2").author_time, 1_600_002_000);

    install_author_replacement(
        &lab,
        &c2,
        "author Impostor <impostor@evil.test> 1500000000 +0000",
    )?;
    assert!(lab.git(&["replace", "-l"])?.contains(&c2[..8]));

    // PLAN PREMISE, HOLDS: an ambient replace ref really does alter attribution.
    let poisoned = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    let poisoned_line = line_with(&poisoned, "line2");
    assert_eq!(poisoned_line.author, "Impostor");
    assert_eq!(poisoned_line.author_time, 1_500_000_000);
    // Note the trap: the reported SHA is still the ORIGINAL commit id, so a
    // consumer cannot notice the substitution by looking at the sha.
    assert_eq!(poisoned_line.sha, c2);
    assert_eq!(
        newest_author_time(&poisoned),
        1_600_004_000,
        "the newest-line value is unaffected here, but per-line ages are already wrong"
    );

    // PLAN CLAIM, HOLDS: --no-replace-objects neutralizes it. (It must be hoisted
    // ahead of `blame`; git::blame_porcelain does that, see its comment.)
    let hardened = git::blame_porcelain(lab.path(), "doc.md", &["--no-replace-objects"])?;
    let hardened_line = line_with(&hardened, "line2");
    assert_eq!(hardened_line.author, "Harness Author");
    assert_eq!(hardened_line.author_time, 1_600_002_000);
    assert_eq!(hardened_line.sha, c2);
    assert_eq!(
        hardened, baseline,
        "hardened blame must equal the pre-replace blame exactly"
    );

    println!(
        "G3: replace ref moved line2 from {} to {}; --no-replace-objects restored {}",
        1_600_002_000, poisoned_line.author_time, hardened_line.author_time
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// G4 — blame.ignoreRevsFile: it bites, but the plan's antidote does not work
// ---------------------------------------------------------------------------

#[test]
fn g4_configured_ignore_revs_file_changes_blame_and_is_not_cleared_by_dash_c() -> Result<()> {
    let lab = GitLab::init()?;
    lab.write("doc.md", "alpha\nbeta\n")?;
    let authored = lab.commit_at("author the prose", 1_600_001_000, 1_600_001_000)?;
    lab.write("doc.md", "ALPHA\nbeta\n")?;
    let reformat = lab.commit_at("reformat only", 1_600_002_000, 1_600_002_000)?;

    // Half one: without configuration, the reformat commit owns the line.
    let plain = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    assert_eq!(line_with(&plain, "ALPHA").sha, reformat);
    assert_eq!(line_with(&plain, "ALPHA").author_time, 1_600_002_000);

    lab.write(".git-blame-ignore-revs", &format!("{reformat}\n"))?;
    lab.git(&[
        "config",
        "--local",
        "blame.ignoreRevsFile",
        ".git-blame-ignore-revs",
    ])?;

    // PLAN PREMISE, HOLDS: repository configuration really does move attribution,
    // so hardening against it is not pointless. The line is now credited to the
    // older commit, i.e. the chunk looks ~1000 s STALER than default blame says.
    let ignored = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    assert_eq!(line_with(&ignored, "ALPHA").sha, authored);
    assert_eq!(line_with(&ignored, "ALPHA").author_time, 1_600_001_000);
    assert_eq!(newest_author_time(&ignored), 1_600_001_000);
    assert_eq!(newest_author_time(&plain), 1_600_002_000);

    // Half two — PLAN CLAIM VIOLATED. The plan says blame "clears repository
    // `blame.ignoreRevsFile` configuration with `-c blame.ignoreRevsFile=`".
    // OBSERVED (git 2.49.0): it does not. A config-supplied empty value never
    // resets the list; attribution stays ignored. Verified independently of this
    // harness with `git -c blame.ignoreRevsFile= blame --porcelain -- doc.md`,
    // and also when the empty value is appended AFTER the real one in the local
    // config file, so it is not a precedence-ordering accident.
    let plan_antidote =
        git::blame_porcelain(lab.path(), "doc.md", &["-c", "blame.ignoreRevsFile="])?;
    assert_eq!(
        line_with(&plan_antidote, "ALPHA").sha,
        authored,
        "if this ever starts equalling {reformat}, git changed and the plan became true"
    );
    assert_eq!(
        line_with(&plan_antidote, "ALPHA").author_time,
        1_600_001_000
    );
    assert_eq!(
        plan_antidote, ignored,
        "-c blame.ignoreRevsFile= changed nothing at all"
    );

    // What DOES work in git 2.49: the command-line empty file name, which is
    // documented to reset the list ("Empty file names will reset the list of
    // ignored revisions"). This is the fix the plan needs.
    let working_antidote = git::blame_porcelain(lab.path(), "doc.md", &["--ignore-revs-file="])?;
    assert_eq!(line_with(&working_antidote, "ALPHA").sha, reformat);
    assert_eq!(
        line_with(&working_antidote, "ALPHA").author_time,
        1_600_002_000
    );
    assert_eq!(
        working_antidote, plain,
        "--ignore-revs-file= restores unhardened attribution"
    );

    // `--no-ignore-revs-file` is the equivalent negation form and also works.
    let negation = git::blame_porcelain(lab.path(), "doc.md", &["--no-ignore-revs-file"])?;
    assert_eq!(negation, plain);

    println!(
        "G4: config ignore-revs moved ALPHA {} -> {}; `-c blame.ignoreRevsFile=` left it at {}; \
         `--ignore-revs-file=` restored {}",
        1_600_002_000,
        line_with(&ignored, "ALPHA").author_time,
        line_with(&plan_antidote, "ALPHA").author_time,
        line_with(&working_antidote, "ALPHA").author_time
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// G5 — worktree edits are attributable without inventing a commit
// ---------------------------------------------------------------------------

#[test]
fn g5_unstaged_worktree_lines_blame_to_the_all_zero_sha() -> Result<()> {
    let test_start = now_epoch();
    let lab = GitLab::init()?;
    lab.write("doc.md", "first\nsecond\nthird\n")?;
    let base = lab.commit_at("c1", 1_600_001_000, 1_600_001_000)?;

    lab.write("doc.md", "first\nEDITED IN WORKTREE\nthird\n")?;
    // Deliberately NOT staged: index column blank, worktree column M.
    assert_eq!(status_porcelain(&lab), " M doc.md\n");

    let blame = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    assert_eq!(blame.len(), 3);
    let dirty = line_with(&blame, "EDITED IN WORKTREE");

    // PLAN CLAIM, HOLDS: attributable, and no commit is invented.
    assert!(dirty.not_committed_yet);
    assert_eq!(dirty.sha, "0".repeat(40));
    assert_eq!(dirty.author, "Not Committed Yet");
    assert!(
        !lab.git_raw(&["cat-file", "-e", &dirty.sha]).ok,
        "the all-zero sha must not resolve to an object"
    );

    // Its author time is the wall clock, not a commit time: usable as "newest"
    // ordering, useless as authorship evidence. The plan's `working_tree` basis
    // (literal `uncommitted`, not a timestamp) is the right shape for this.
    assert!(dirty.author_time >= test_start);
    assert!(dirty.author_time > 1_600_001_000);

    for clean in ["first", "third"].iter().map(|n| line_with(&blame, n)) {
        assert!(!clean.not_committed_yet);
        assert_eq!(clean.sha, base);
        assert_eq!(clean.author_time, 1_600_001_000);
    }

    // The secondary metadata the plan promises for a working_tree chunk ("the
    // latest committed author time as secondary metadata when one exists") is
    // recoverable from the same blame.
    let latest_committed = blame
        .iter()
        .filter(|l| !l.not_committed_yet)
        .map(|l| l.author_time)
        .max();
    assert_eq!(latest_committed, Some(1_600_001_000));
    println!("G5: 1 uncommitted line, latest committed author time {latest_committed:?}");
    Ok(())
}

// ---------------------------------------------------------------------------
// G6 — staged-but-uncommitted changes
// ---------------------------------------------------------------------------

#[test]
fn g6_staged_changes_blame_as_working_tree_because_blame_reads_the_worktree() -> Result<()> {
    let lab = GitLab::init()?;
    lab.write("doc.md", "first\nsecond\nthird\n")?;
    let base = lab.commit_at("c1", 1_600_001_000, 1_600_001_000)?;

    lab.write("doc.md", "first\nSTAGED EDIT\nthird\n")?;
    lab.git(&["add", "doc.md"])?;
    assert_eq!(
        status_porcelain(&lab),
        "M  doc.md\n",
        "index differs from HEAD, worktree matches index"
    );

    // PLAN CLAIM, HOLDS: a staged modification of a tracked file is visible to
    // blame and is labelled uncommitted, exactly like an unstaged one.
    let staged = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    let dirty = line_with(&staged, "STAGED EDIT");
    assert!(dirty.not_committed_yet);
    assert_eq!(dirty.sha, "0".repeat(40));
    assert_eq!(dirty.author, "Not Committed Yet");
    assert_eq!(line_with(&staged, "first").sha, base);

    // REFINEMENT the plan does not state: blame's input is the WORKTREE file,
    // not the index. Restore the worktree copy to the committed bytes while the
    // index still holds the edit (status "MM") and blame reports zero
    // uncommitted lines — the staged change is invisible.
    lab.write("doc.md", "first\nsecond\nthird\n")?;
    assert_eq!(
        status_porcelain(&lab),
        "MM doc.md\n",
        "index and worktree must now disagree in both directions"
    );
    let reverted = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    assert!(
        reverted.iter().all(|l| !l.not_committed_yet),
        "OBSERVED: blame ignores the index entirely; staged-only content never appears"
    );
    assert!(reverted.iter().all(|l| l.sha == base));

    // This is consistent with, not contrary to, the plan: the cache key hashes
    // "the exact file bytes being blamed", i.e. the worktree bytes, and those
    // bytes are what blame answers about. The plan's "staged or unstaged"
    // wording is true for the only case that produces modified worktree lines.
    println!("G6: staged edit blames uncommitted; staged-only edit with clean worktree does not");
    Ok(())
}

// ---------------------------------------------------------------------------
// G7 — blame on an untracked file fails; the caller can degrade
// ---------------------------------------------------------------------------

#[test]
fn g7_blame_on_an_untracked_file_fails_recoverably() -> Result<()> {
    let lab = GitLab::init()?;
    lab.write("doc.md", "tracked\n")?;
    lab.commit_at("c1", 1_600_001_000, 1_600_001_000)?;
    lab.write("scratch.md", "untracked prose\n")?;

    let raw = lab.git_raw(&["blame", "--line-porcelain", "--", "scratch.md"]);
    assert!(!raw.ok);
    assert_eq!(
        raw.code,
        Some(128),
        "git blame signals a hard failure, not an empty result"
    );
    assert!(
        raw.combined().contains("no such path 'scratch.md' in HEAD"),
        "unexpected message: {}",
        raw.combined()
    );

    // PLAN CLAIM, HOLDS: the failure is an ordinary Err, so a caller degrades to
    // observed/unknown provenance instead of crashing the scan.
    let parsed = git::blame_porcelain(lab.path(), "scratch.md", &[]);
    assert!(parsed.is_err());
    let message = parsed.unwrap_err().to_string();
    assert!(
        message.contains("no such path"),
        "unexpected error: {message}"
    );

    // And the rest of the provenance surface stays usable for that same path:
    // no tip commit, no panic, no partial state.
    assert_eq!(git::path_tip_commit(lab.path(), "scratch.md")?, None);
    assert!(
        git::blame_porcelain(lab.path(), "doc.md", &[]).is_ok(),
        "the scan continues"
    );

    println!("G7: blame exit 128 on untracked path; path_tip_commit -> None; scan survives");
    Ok(())
}

// ---------------------------------------------------------------------------
// G8 — a newly added staged file has no committed authorship time
// ---------------------------------------------------------------------------

#[test]
fn g8_newly_added_staged_file_has_no_committed_authorship_time() -> Result<()> {
    let test_start = now_epoch();
    let lab = GitLab::init()?;
    lab.write("doc.md", "tracked\n")?;
    lab.commit_at("c1", 1_600_001_000, 1_600_001_000)?;

    lab.write("fresh.md", "n1\nn2\n")?;
    lab.git(&["add", "fresh.md"])?;
    assert_eq!(status_porcelain(&lab), "A  fresh.md\n");

    // OBSERVED: unlike the untracked case (G7), blame SUCCEEDS on a staged new
    // file — every line is the all-zero sha.
    let blame = git::blame_porcelain(lab.path(), "fresh.md", &[])?;
    assert_eq!(blame.len(), 2);
    assert!(blame.iter().all(|l| l.not_committed_yet));
    assert!(blame.iter().all(|l| l.sha == "0".repeat(40)));
    assert!(blame.iter().all(|l| l.author == "Not Committed Yet"));
    assert_eq!(blame[0].filename.as_deref(), Some("fresh.md"));

    // PLAN CLAIM, HOLDS: "newly added staged files … have no Git authorship
    // time". Every timestamp present is the wall clock, and no commit touches
    // the path, so there is nothing committed to date it by.
    assert!(blame.iter().all(|l| l.author_time >= test_start));
    assert_eq!(git::path_tip_commit(lab.path(), "fresh.md")?, None);
    let committed_times: Vec<i64> = blame
        .iter()
        .filter(|l| !l.not_committed_yet)
        .map(|l| l.author_time)
        .collect();
    assert!(
        committed_times.is_empty(),
        "a staged-new file must expose no committed time"
    );

    // The plan says such a file "carries observed or unknown provenance". Note
    // that a naive `working_tree` label would also be defensible from the blame
    // alone — nothing in the blame distinguishes G8 from G5/G6; only the missing
    // path tip does. That distinction is available and cheap.
    println!("G8: staged-new file -> all zero-sha lines, path_tip_commit None");
    Ok(())
}

// ---------------------------------------------------------------------------
// G9 — cache key stability across unrelated commits, staging, and mtime churn
// ---------------------------------------------------------------------------

#[test]
fn g9_path_tip_and_cache_key_survive_unrelated_commits() -> Result<()> {
    let lab = GitLab::init()?;
    lab.write("doc.md", "the claim\n")?;
    let doc_commit = lab.commit_at("author doc", 1_600_001_000, 1_600_001_000)?;

    let tip0 = git::path_tip_commit(lab.path(), "doc.md")?;
    let key0 = plan_cache_key(lab.path(), "doc.md")?;
    assert_eq!(tip0.as_deref(), Some(doc_commit.as_str()));

    // PLAN CLAIM, HOLDS: "unrelated commits … do not invalidate it".
    for step in 1..=3 {
        lab.write(&format!("unrelated{step}.md"), "noise\n")?;
        lab.commit_at(
            &format!("unrelated {step}"),
            1_600_002_000 + step,
            1_600_002_000 + step,
        )?;
        assert_ne!(lab.head()?, doc_commit, "HEAD moved, as intended");
        assert_eq!(git::path_tip_commit(lab.path(), "doc.md")?, tip0);
        assert_eq!(plan_cache_key(lab.path(), "doc.md")?, key0);
    }
    // An empty commit moves HEAD without touching any path at all.
    lab.commit_at("empty", 1_600_009_000, 1_600_009_000)?;
    assert_eq!(git::path_tip_commit(lab.path(), "doc.md")?, tip0);
    assert_eq!(plan_cache_key(lab.path(), "doc.md")?, key0);

    // PLAN CLAIM, HOLDS: "staging an unchanged worktree file [does] not
    // invalidate it".
    lab.git(&["add", "doc.md"])?;
    assert_eq!(plan_cache_key(lab.path(), "doc.md")?, key0);

    // PLAN CLAIM, HOLDS: "filesystem modification time is never a fallback".
    // Rewriting identical bytes bumps mtime and changes nothing in the key.
    let before_mtime = std::fs::metadata(lab.path().join("doc.md"))?.modified()?;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    lab.write("doc.md", "the claim\n")?;
    let after_mtime = std::fs::metadata(lab.path().join("doc.md"))?.modified()?;
    assert!(
        after_mtime > before_mtime,
        "mtime must actually have moved for this to prove anything"
    );
    assert_eq!(plan_cache_key(lab.path(), "doc.md")?, key0);

    // And the blame itself is byte-identical throughout, so the cache would have
    // been correct to reuse.
    let blame = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    assert_eq!(blame.len(), 1);
    assert_eq!(blame[0].sha, doc_commit);
    assert_eq!(blame[0].author_time, 1_600_001_000);

    println!("G9: key stable across 4 unrelated commits, `git add`, and an mtime bump: {key0}");
    Ok(())
}

// ---------------------------------------------------------------------------
// G10 — cache invalidation on path-history rewriting
// ---------------------------------------------------------------------------

#[test]
fn g10_path_tip_changes_when_that_paths_history_is_rewritten() -> Result<()> {
    // ---- amend ----
    let lab = GitLab::init()?;
    lab.write("doc.md", "the claim\n")?;
    let original = lab.commit_at("author doc", 1_600_001_000, 1_600_001_000)?;
    let key_before = plan_cache_key(lab.path(), "doc.md")?;
    let blame_before = git::blame_porcelain(lab.path(), "doc.md", &[])?;

    lab.git(&["commit", "--quiet", "--amend", "--no-edit", "--allow-empty"])?;
    let amended = lab.head()?;
    assert_ne!(amended, original);
    let tip_after = git::path_tip_commit(lab.path(), "doc.md")?;
    assert_eq!(tip_after.as_deref(), Some(amended.as_str()));
    let key_after = plan_cache_key(lab.path(), "doc.md")?;
    assert_ne!(
        key_after, key_before,
        "amend must invalidate the blame cache entry"
    );

    // The file bytes are unchanged, so a key WITHOUT the path tip would have
    // reused a stale entry whose sha no longer exists in history.
    let bytes = std::fs::read(lab.path().join("doc.md"))?;
    assert!(key_before.contains(&format!("bytes={}", hash_hex(&bytes))));
    assert!(key_after.contains(&format!("bytes={}", hash_hex(&bytes))));
    let blame_after = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    assert_ne!(
        blame_after[0].sha, blame_before[0].sha,
        "the cached sha would be stale"
    );

    // ---- rebase of a doc-touching commit ----
    let rb = GitLab::init()?;
    rb.write("base.md", "base\n")?;
    rb.commit_at("base", 1_600_000_000, 1_600_000_000)?;
    rb.git(&["checkout", "--quiet", "-b", "feature"])?;
    rb.write("docs/guide.md", "feature prose\n")?;
    let feature_commit = rb.commit_at("feat", 1_600_001_000, 1_600_001_500)?;
    let rb_key_before = plan_cache_key(rb.path(), "docs/guide.md")?;

    rb.git(&["checkout", "--quiet", "main"])?;
    rb.write("other.md", "other\n")?;
    rb.commit_at("other", 1_600_003_000, 1_600_003_000)?;
    rb.git(&["checkout", "--quiet", "feature"])?;
    rb.git(&["rebase", "--quiet", "main"])?;

    let rb_tip = git::path_tip_commit(rb.path(), "docs/guide.md")?;
    assert_eq!(rb_tip.as_deref(), Some(rb.head()?.as_str()));
    assert_ne!(rb_tip.as_deref(), Some(feature_commit.as_str()));
    assert_ne!(plan_cache_key(rb.path(), "docs/guide.md")?, rb_key_before);

    println!(
        "G10: amend {} -> {}; rebase {} -> {}",
        &original[..8],
        &amended[..8],
        &feature_commit[..8],
        &rb_tip.unwrap()[..8]
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// G11 — identical content, and why the path belongs in the cache key
// ---------------------------------------------------------------------------

#[test]
fn g11_identical_content_can_share_a_path_tip_yet_blame_differently() -> Result<()> {
    let lab = GitLab::init()?;
    // c1 seeds three files. `c.md` is already in its final shape.
    lab.write("a.md", "alpha\nbeta\n")?;
    lab.write("b.md", "alpha\nQQQ\n")?;
    lab.write("c.md", "alpha\nbeta\ngamma\n")?;
    let c1 = lab.commit_at("c1", 1_600_001_000, 1_600_001_000)?;
    // c2 converges a.md and b.md onto c.md's exact bytes, touching both.
    lab.write("a.md", "alpha\nbeta\ngamma\n")?;
    lab.write("b.md", "alpha\nbeta\ngamma\n")?;
    let c2 = lab.commit_at("c2", 1_600_002_000, 1_600_002_000)?;

    let bytes_a = std::fs::read(lab.path().join("a.md"))?;
    let bytes_b = std::fs::read(lab.path().join("b.md"))?;
    let bytes_c = std::fs::read(lab.path().join("c.md"))?;
    assert_eq!(bytes_a, bytes_b);
    assert_eq!(bytes_a, bytes_c);
    assert_eq!(hash_hex(&bytes_a), hash_hex(&bytes_b));

    let tip_a = git::path_tip_commit(lab.path(), "a.md")?;
    let tip_b = git::path_tip_commit(lab.path(), "b.md")?;
    let tip_c = git::path_tip_commit(lab.path(), "c.md")?;

    // Benign half — PLAN CLAIM HOLDS: distinct path histories give distinct tips.
    assert_eq!(tip_c.as_deref(), Some(c1.as_str()));
    assert_eq!(tip_a.as_deref(), Some(c2.as_str()));
    assert_ne!(tip_a, tip_c);

    // Adversarial half — the same commit last touched a.md and b.md, so the
    // "(content hash, path tip)" pair is IDENTICAL for two files whose blames
    // are not.
    assert_eq!(tip_a, tip_b);

    let blame_a = git::blame_porcelain(lab.path(), "a.md", &[])?;
    let blame_b = git::blame_porcelain(lab.path(), "b.md", &[])?;
    assert_ne!(
        blame_a, blame_b,
        "the whole point: same bytes, same tip, different provenance"
    );
    assert_eq!(line_with(&blame_a, "beta").sha, c1);
    assert_eq!(line_with(&blame_a, "beta").author_time, 1_600_001_000);
    assert_eq!(line_with(&blame_b, "beta").sha, c2);
    assert_eq!(line_with(&blame_b, "beta").author_time, 1_600_002_000);
    // `filename` differs too, so even a "harmless" reuse leaks the wrong path.
    assert_eq!(
        line_with(&blame_a, "beta").filename.as_deref(),
        Some("a.md")
    );
    assert_eq!(
        line_with(&blame_b, "beta").filename.as_deref(),
        Some("b.md")
    );

    // A chunk covering only lines 1-2 would be dated 1000 s apart depending on
    // which file it came from, while the whole-file newest value coincides.
    let head_a: Vec<BlameLine> = blame_a
        .iter()
        .filter(|l| l.final_line <= 2)
        .cloned()
        .collect();
    let head_b: Vec<BlameLine> = blame_b
        .iter()
        .filter(|l| l.final_line <= 2)
        .cloned()
        .collect();
    assert_eq!(newest_author_time(&head_a), 1_600_001_000);
    assert_eq!(newest_author_time(&head_b), 1_600_002_000);
    assert_eq!(newest_author_time(&blame_a), newest_author_time(&blame_b));

    // Therefore the plan's repository-relative path component is NECESSARY, not
    // belt-and-braces: without it these two files collide on one cache entry.
    let key_a = plan_cache_key(lab.path(), "a.md")?;
    let key_b = plan_cache_key(lab.path(), "b.md")?;
    assert_ne!(key_a, key_b);
    assert_eq!(
        key_a.replace("path=a.md", "path=X"),
        key_b.replace("path=b.md", "path=X"),
        "the path is the ONLY component that separates these two entries"
    );

    println!(
        "G11: a.md/b.md share bytes+tip {}; sub-chunk ages differ by 1000 s",
        &c2[..8]
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// G12 — deepening a shallow clone changes the boundary fingerprint
// ---------------------------------------------------------------------------

#[test]
fn g12_deepening_changes_the_shallow_boundary_fingerprint() -> Result<()> {
    let lab = layered_history()?;
    assert_eq!(git::shallow_boundary_fingerprint(lab.path())?, None);

    let clone = lab.clone_shallow(1)?;
    let f1 = git::shallow_boundary_fingerprint(clone.path())?.expect("depth=1 clone is shallow");
    let key1 = plan_cache_key(clone.path(), "doc.md")?;
    let blame1 = git::blame_porcelain(clone.path(), "doc.md", &[])?;
    assert_eq!(newest_author_time(&blame1), 1_600_004_000);
    assert!(blame1.iter().all(|l| l.boundary));

    clone.deepen(1)?;
    let f2 = git::shallow_boundary_fingerprint(clone.path())?.expect("still shallow after +1");
    // PLAN CLAIM, HOLDS: "clone deepening … resolve[s] correctly" because the
    // fingerprint moves, so the cache entry is rebuilt.
    assert_ne!(f1, f2, "deepening must change the boundary fingerprint");
    let key2 = plan_cache_key(clone.path(), "doc.md")?;
    assert_ne!(key1, key2);
    // The blame really did change, so a stale cache entry would have been wrong.
    // Depth is now 2: the boundary is c3, which absorbs lines 1-3, while line4
    // escapes the boundary and recovers its own commit.
    let blame2 = git::blame_porcelain(clone.path(), "doc.md", &[])?;
    assert_ne!(blame1, blame2);
    assert!(line_with(&blame2, "line3").boundary);
    assert_eq!(line_with(&blame2, "line3").author_time, 1_600_003_000);
    assert!(!line_with(&blame2, "line4").boundary);
    assert_eq!(line_with(&blame2, "line4").author_time, 1_600_004_000);
    // line1's reported age improved from a 3000 s overstatement to 2000 s: each
    // deepening changes the answer, which is why the fingerprint must be keyed.
    assert_eq!(line_with(&blame1, "line1").author_time, 1_600_004_000);
    assert_eq!(line_with(&blame2, "line1").author_time, 1_600_003_000);

    clone.deepen(1)?;
    let f3 = git::shallow_boundary_fingerprint(clone.path())?.expect("still shallow after +2");
    assert_ne!(f2, f3);
    assert_ne!(f1, f3);
    let blame3 = git::blame_porcelain(clone.path(), "doc.md", &[])?;
    assert!(
        !line_with(&blame3, "line3").boundary,
        "depth 3 frees line3 from the boundary"
    );
    assert_eq!(line_with(&blame3, "line3").author_time, 1_600_003_000);
    assert!(line_with(&blame3, "line1").boundary);
    assert_eq!(line_with(&blame3, "line1").author_time, 1_600_002_000);

    // Deepening past the root removes `.git/shallow` entirely: the fingerprint
    // becomes None, which is a fourth distinct key value, and every line
    // recovers its true author time.
    clone.deepen(50)?;
    assert_eq!(
        git::shallow_boundary_fingerprint(clone.path())?,
        None,
        "past the root git drops .git/shallow and the repository is complete"
    );
    let complete = git::blame_porcelain(clone.path(), "doc.md", &[])?;
    let truth = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    let times: Vec<i64> = complete.iter().map(|l| l.author_time).collect();
    assert_eq!(
        times,
        truth.iter().map(|l| l.author_time).collect::<Vec<_>>()
    );
    assert_eq!(
        times,
        vec![1_600_001_000, 1_600_002_000, 1_600_003_000, 1_600_004_000]
    );

    println!(
        "G12: fingerprints {} -> {} -> {} -> None",
        &f1[..8],
        &f2[..8],
        &f3[..8]
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// G13 — renames
// ---------------------------------------------------------------------------

#[test]
fn g13_blame_follows_renames_by_default_and_path_tip_stops_at_the_rename() -> Result<()> {
    // ---- pure rename ----
    let lab = GitLab::init()?;
    lab.write("old.md", "r1\nr2\nr3\n")?;
    let authored = lab.commit_at("author prose", 1_600_001_000, 1_600_001_000)?;
    lab.git(&["mv", "old.md", "new.md"])?;
    let renamed = lab.commit_at("rename only", 1_600_002_000, 1_600_002_000)?;

    // PLAN-ADJACENT CLAIM FALSIFIED: blame does NOT need -M/-C to cross a whole-
    // file rename. Plain `git blame new.md` already reports the pre-rename
    // commit and the pre-rename filename.
    let plain = git::blame_porcelain(lab.path(), "new.md", &[])?;
    assert_eq!(plain.len(), 3);
    assert!(
        plain.iter().all(|l| l.sha == authored),
        "default blame followed the rename"
    );
    assert!(plain.iter().all(|l| l.author_time == 1_600_001_000));
    assert_eq!(
        plain[0].filename.as_deref(),
        Some("old.md"),
        "`filename` is the path AT the blamed commit, i.e. the pre-rename name"
    );
    // -M/-C add nothing here.
    let with_mc = git::blame_porcelain(lab.path(), "new.md", &["-M", "-C"])?;
    assert_eq!(
        with_mc, plain,
        "-M -C produced no additional rename following"
    );

    // Rename WITH an edit in the same commit is also followed by default.
    let mixed = GitLab::init()?;
    mixed.write("old.md", "r1\nr2\nr3\nr4\nr5\n")?;
    let mixed_authored = mixed.commit_at("author prose", 1_600_001_000, 1_600_001_000)?;
    mixed.git(&["mv", "old.md", "new.md"])?;
    mixed.write("new.md", "r1\nr2\nCHANGED\nr4\nr5\n")?;
    let mixed_renamed = mixed.commit_at("rename and edit", 1_600_002_000, 1_600_002_000)?;
    let mixed_blame = git::blame_porcelain(mixed.path(), "new.md", &[])?;
    assert_eq!(line_with(&mixed_blame, "r1").sha, mixed_authored);
    assert_eq!(
        line_with(&mixed_blame, "r1").filename.as_deref(),
        Some("old.md")
    );
    assert_eq!(line_with(&mixed_blame, "CHANGED").sha, mixed_renamed);
    assert_eq!(
        line_with(&mixed_blame, "CHANGED").filename.as_deref(),
        Some("new.md")
    );

    // ---- path_tip_commit across the rename ----
    // It is the rename commit for BOTH names: `git log -1 -- <path>` does not
    // follow renames, so the new path's history starts at the rename and the old
    // path's history ends there.
    assert_eq!(
        git::path_tip_commit(lab.path(), "new.md")?.as_deref(),
        Some(renamed.as_str())
    );
    assert_eq!(
        git::path_tip_commit(lab.path(), "old.md")?.as_deref(),
        Some(renamed.as_str())
    );
    // --follow does see through it, which is precisely what the cache key does
    // NOT use, and does not need to.
    let followed: Vec<String> = lab
        .git(&["log", "--format=%H", "--follow", "--", "new.md"])?
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(followed, vec![renamed.clone(), authored.clone()]);

    // Cache consequence, PLAN CLAIM HOLDS: the rename changes both the path
    // component and the tip component, so nothing stale can be reused.
    let key_new = plan_cache_key(lab.path(), "new.md")?;
    assert!(key_new.contains("path=new.md"));
    assert!(key_new.contains(&format!("tip={renamed}")));

    // TENSION WORTH RECORDING (not a git bug): git provenance for the renamed
    // path reports the ORIGINAL author time (1_600_001_000), while the plan's
    // v1 ledger refuses cross-path predecessors and would restart the blocks as
    // `baseline`. So for a renamed file the git basis says "old" and the
    // observed basis says "new" about the same bytes. The plan already accepts
    // this ("Without usable Git provenance, a pure rename therefore restarts
    // observed freshness"); this assertion pins the git half of the split.
    assert_eq!(newest_author_time(&plain), 1_600_001_000);
    assert!(newest_author_time(&plain) < 1_600_002_000);

    println!(
        "G13: default blame crossed the rename (author_time {}), path tip moved to {}",
        newest_author_time(&plain),
        &renamed[..8]
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// G14 — timestamps are author-controlled and may be in the future
// ---------------------------------------------------------------------------

#[test]
fn g14_git_accepts_future_dated_commits_and_blame_reports_them() -> Result<()> {
    let now = now_epoch();
    let future = now + 10 * 365 * 24 * 3600; // ~10 years ahead
    let lab = GitLab::init()?;
    lab.write("doc.md", "present line\n")?;
    lab.commit_at("present", now - 1_000, now - 1_000)?;
    lab.write("doc.md", "present line\nfuture line\n")?;

    // PLAN-RELEVANT FACT, HOLDS: git accepts a future author AND committer date
    // without warning or error. Timestamps are attacker/author controlled data.
    let future_commit = lab.commit_at("from the future", future, future)?;
    assert_eq!(
        lab.git(&["log", "-1", "--format=%at", future_commit.as_str()])?,
        future.to_string()
    );

    let blame = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    let ahead = line_with(&blame, "future line");
    assert_eq!(ahead.author_time, future);
    assert_eq!(ahead.committer_time, future);
    assert!(ahead.author_time > now_epoch());

    // Consequence for an age computation anchored on HEAD/now: the age is
    // NEGATIVE, and the plan's "latest author time" rule makes this chunk the
    // permanent freshness winner over every honestly dated chunk.
    let age = now_epoch() - ahead.author_time;
    assert!(age < 0, "age against wall clock is negative: {age}");
    assert_eq!(newest_author_time(&blame), future);
    let honest = line_with(&blame, "present line").author_time;
    assert!(future > honest);

    // A working-tree edit does NOT beat it, because working-tree lines are dated
    // by the wall clock. Under the plan's partial order the literal
    // `working_tree` label is what makes the dirty chunk newest; if an
    // implementation compared timestamps instead, the future commit would win.
    lab.write("doc.md", "present line\nfuture line\nedited now\n")?;
    let dirty = git::blame_porcelain(lab.path(), "doc.md", &[])?;
    let uncommitted = line_with(&dirty, "edited now");
    assert!(uncommitted.not_committed_yet);
    assert!(
        uncommitted.author_time < future,
        "a wall-clock uncommitted line ({}) is numerically OLDER than the future commit ({future})",
        uncommitted.author_time
    );
    assert_eq!(
        newest_author_time(&dirty),
        future,
        "timestamp comparison alone picks the future commit"
    );

    println!(
        "G14: future commit accepted at {future} (now {now}); uncommitted line dated {} — \
         timestamp order alone puts the future commit above live edits",
        uncommitted.author_time
    );
    Ok(())
}
