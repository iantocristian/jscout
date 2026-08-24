//! G24 reality suite: drives the REAL `jscout` release binary to test the
//! review claims about the EXISTING system — the claims that forced the
//! separate-database design in the G24 plan.
//!
//! Methodology: every assertion documents ACTUAL observed behaviour of the
//! shipped binary. Where reality contradicts the plan the assertion still
//! encodes what really happens, and a comment names the plan claim and the
//! divergence. Nothing here is weakened to make the suite green.
//!
//! Safety: every test operates inside its own `tempfile::TempDir`. Nothing in
//! this file references the real repository root, and no command that could
//! reach a network or a model is ever invoked (only `config`, `index`,
//! `search`, `stats`, `chunks`, `events`, `who-uses`, `overview`, `memory`,
//! `neighborhood` — all local and deterministic).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use g24_harness::proc::{self, CmdOut};

// ---------------------------------------------------------------------------
// harness-private helpers (core must not be modified, so these live here)
// ---------------------------------------------------------------------------

const SQLITE3: &str = "/usr/bin/sqlite3";

/// The real binary, or `None` with a printed skip note.
fn jscout() -> Option<PathBuf> {
    match proc::jscout_binary() {
        Some(path) => Some(path),
        None => {
            println!("SKIP: jscout release binary not present; nothing to drive.");
            None
        }
    }
}

/// The system sqlite3 CLI, or `None` with a printed skip note.
fn sqlite() -> Option<PathBuf> {
    let path = Path::new(SQLITE3);
    if path.is_file() {
        Some(path.to_path_buf())
    } else {
        println!("SKIP: {SQLITE3} not present; cannot inspect databases.");
        None
    }
}

/// Run one SQL statement against `db`, returning the raw CLI outcome.
fn sql(db: &Path, statement: &str) -> CmdOut {
    let cwd = db.parent().expect("database has a parent directory");
    proc::run(
        Path::new(SQLITE3),
        &[db.to_str().unwrap(), statement],
        cwd,
        &[],
    )
}

/// Non-empty stdout lines from a sqlite3 query.
fn sql_rows(db: &Path, statement: &str) -> Vec<String> {
    let out = sql(db, statement);
    out.stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// The sorted `meta` key set of a jscout database.
fn meta_keys(db: &Path) -> Vec<String> {
    sql_rows(db, "select key from meta order by key;")
}

fn meta_value(db: &Path, key: &str) -> Option<String> {
    let statement = format!("select value from meta where key='{key}';");
    sql_rows(db, &statement).into_iter().next()
}

fn blake3_file(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read file for hashing");
    blake3::hash(&bytes).to_hex().to_string()
}

/// A canonicalized temp directory holding a small synthetic JS/TS repository
/// with real imports and exports. Never touches the user's checkout.
fn synthetic_repo() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let root = dir.path().canonicalize().expect("canonicalize temp root");
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create src");

    std::fs::write(
        src.join("a.js"),
        "import { helper } from './b.js';\n\
         export function alpha(x) { return helper(x) + 1; }\n\
         export const ALPHA_CONST = 42;\n",
    )
    .unwrap();
    std::fs::write(
        src.join("b.js"),
        "export function helper(y) { return y * 2; }\n",
    )
    .unwrap();
    std::fs::write(
        src.join("c.ts"),
        "import { alpha } from './a.js';\n\
         export interface Shape { kind: string }\n\
         export function gamma(s: Shape): number { return alpha(s.kind.length); }\n",
    )
    .unwrap();
    (dir, root)
}

/// A wider synthetic repo, used where an index must take long enough to be
/// interrupted mid-run.
fn wide_repo(files: usize) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::TempDir::new().expect("create temp dir");
    let root = dir.path().canonicalize().expect("canonicalize temp root");
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create src");
    for i in 0..files {
        let next = (i + 1) % files;
        let body = format!(
            "import {{ h{next} }} from './f{next}.js';\n\
             export function h{i}(x) {{ return h{next}(x) + {i}; }}\n\
             export const C{i} = {i};\n"
        );
        std::fs::write(src.join(format!("f{i}.js")), body).unwrap();
    }
    (dir, root)
}

/// The `[docs]` configuration section exactly as docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md specifies it.
const PLAN_DOCS_SECTION: &str = r#"
[docs]
include = ["**/*.md"]
exclude = []
freshness = true
max_rank_movement = 2

[docs.database]
path = ".jscout-docs.db"

[docs.search]
vector = true
rerank = true
limit = 10
response_bytes = 24000
"#;

/// Pull every `"score": <float>` out of a `--debug-json` payload, in order.
/// Avoids adding a JSON dependency the crate does not have.
fn debug_json_scores(json: &str) -> Vec<f64> {
    let mut scores = Vec::new();
    let mut rest = json;
    while let Some(at) = rest.find("\"score\":") {
        rest = &rest[at + "\"score\":".len()..];
        let value: String = rest
            .chars()
            .skip_while(|c| c.is_whitespace())
            .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == 'e' || *c == '-' || *c == '+')
            .collect();
        if let Ok(parsed) = value.parse::<f64>() {
            scores.push(parsed);
        }
    }
    scores
}

// ---------------------------------------------------------------------------
// J1 — THE CONFIG BREAK
// ---------------------------------------------------------------------------

/// PLAN CLAIM (PLAN.md G24 entry #1 and docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md "Configuration"):
/// "Compatibility of a committed `[docs]` configuration section with pre-docs
/// jscout binaries is explicitly not a requirement."
///
/// This test quantifies exactly how bad that accepted non-requirement is on
/// the shipped binary: configuration parsing uses `deny_unknown_fields` and
/// runs before every repository command, so a committed `[docs]` section is
/// not a docs-only inconvenience — it bricks EVERY repository command for
/// anyone on a pre-docs binary.
#[test]
fn j1_docs_section_breaks_every_repository_command() {
    let Some(bin) = jscout() else { return };
    let (_guard, root) = synthetic_repo();
    let root_str = root.to_str().unwrap();

    // Baseline: a clean checkout with no configuration indexes and searches.
    let baseline_index = proc::run(&bin, &["index", root_str], &root, &[]);
    assert!(
        baseline_index.ok,
        "baseline index must succeed before the config break is meaningful: {}",
        baseline_index.combined()
    );

    // Write the plan's own `[docs]` block on top of the binary's own template.
    let config_init = proc::run(&bin, &["config", "init", root_str], &root, &[]);
    assert!(config_init.ok, "config init: {}", config_init.combined());
    let config_path = root.join(".jscout.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap();
    config.push_str(PLAN_DOCS_SECTION);
    std::fs::write(&config_path, &config).unwrap();

    // Every repository command, docs-related or not.
    let commands: Vec<Vec<&str>> = vec![
        vec!["config", "show", root_str],
        vec!["config", "validate", root_str],
        vec!["index", root_str],
        vec!["search", root_str, "helper"],
        vec!["stats", root_str],
        vec!["chunks", root_str],
        vec!["events", root_str],
        vec!["who-uses", root_str, "helper"],
        vec!["overview", root_str],
        vec!["memory", root_str, "helper"],
        vec!["neighborhood", root_str, "src/a.js"],
    ];

    let mut broken = 0usize;
    for argv in &commands {
        let out = proc::run(&bin, argv, &root, &[]);
        let combined = out.combined();
        assert!(
            !out.ok,
            "OBSERVED: `jscout {}` unexpectedly SUCCEEDED with a [docs] section; \
             the pre-docs binary was expected to reject the whole file: {combined}",
            argv.join(" ")
        );
        assert_eq!(
            out.code,
            Some(1),
            "`jscout {}` exit code; output: {combined}",
            argv.join(" ")
        );
        assert!(
            combined.contains("unknown field `docs`"),
            "`jscout {}` must fail on the unknown `docs` field, got: {combined}",
            argv.join(" ")
        );
        assert!(
            combined.contains("parse configuration"),
            "`jscout {}` must fail during configuration parse, got: {combined}",
            argv.join(" ")
        );
        broken += 1;
        println!(
            "J1 broken: jscout {} -> exit {:?}",
            argv.join(" "),
            out.code
        );
    }
    assert_eq!(
        broken,
        commands.len(),
        "every repository command must break, not just docs-shaped ones"
    );

    // The exact message the plan's accepted non-requirement produces.
    let show = proc::run(&bin, &["config", "show", root_str], &root, &[]);
    let message = show.combined();
    assert!(
        message.contains("TOML parse error"),
        "captured message: {message}"
    );
    assert!(
        message.contains("expected one of `version`, `database`, `search`, `embedding`"),
        "the error enumerates the accepted top-level sections: {message}"
    );
    println!("J1 exact error:\n{message}");

    // The break is configuration-gated, not a total binary failure: argument
    // parsing still works, so `--version` and `--help` are unaffected.
    let version = proc::run(&bin, &["--version"], &root, &[]);
    assert!(
        version.ok,
        "`--version` never reads the config file: {}",
        version.combined()
    );
    println!("J1 unaffected: --version -> {}", version.stdout.trim());

    // Recovery is removal of the section, nothing subtler.
    std::fs::write(&config_path, config.replace(PLAN_DOCS_SECTION, "")).unwrap();
    let healed = proc::run(&bin, &["search", root_str, "helper", "--json"], &root, &[]);
    assert!(
        healed.ok,
        "removing [docs] restores every command: {}",
        healed.combined()
    );
}

/// The mechanism behind J1: `deny_unknown_fields` at every level. A bare
/// `[docs.database]` table with no `[docs]` table breaks identically, and an
/// unknown key inside a *known* section breaks the same way — so there is no
/// "put it somewhere harmless" escape hatch for a forward-compatible section.
#[test]
fn j1b_deny_unknown_fields_is_the_mechanism() {
    let Some(bin) = jscout() else { return };
    let (_guard, root) = synthetic_repo();
    let root_str = root.to_str().unwrap();
    let config_path = root.join(".jscout.toml");

    // Only the nested table, no `[docs]` header of its own.
    std::fs::write(
        &config_path,
        "version = 1\n[docs.database]\npath = \".jscout-docs.db\"\n",
    )
    .unwrap();
    let nested = proc::run(&bin, &["config", "show", root_str], &root, &[]);
    assert!(!nested.ok, "bare [docs.database]: {}", nested.combined());
    assert!(
        nested.combined().contains("unknown field `docs`"),
        "bare [docs.database] is still an unknown top-level `docs`: {}",
        nested.combined()
    );

    // Unknown key inside a known section.
    std::fs::write(
        &config_path,
        "version = 1\n[search]\nlimit = 5\nbogus_key = 3\n",
    )
    .unwrap();
    let nested_key = proc::run(&bin, &["config", "show", root_str], &root, &[]);
    assert!(!nested_key.ok, "unknown key in [search] must fail");
    assert!(
        nested_key.combined().contains("unknown field `bogus_key`"),
        "deny_unknown_fields applies inside sections too: {}",
        nested_key.combined()
    );
    println!(
        "J1b nested deny_unknown_fields:\n{}",
        nested_key.combined().trim()
    );

    // Control: the same file minus the unknown key parses.
    std::fs::write(&config_path, "version = 1\n[search]\nlimit = 5\n").unwrap();
    let ok = proc::run(&bin, &["config", "show", root_str], &root, &[]);
    assert!(ok.ok, "control config must parse: {}", ok.combined());
}

// ---------------------------------------------------------------------------
// J2 — THE GLOBAL SCHEMA VERSION
// ---------------------------------------------------------------------------

/// PLAN CLAIM (docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md "Separate documentation database"): the shared
/// store "has one global schema version whose upgrade path rebuilds
/// source-derived tables".
///
/// Observed: `meta.schema_version` is a single global integer (v29 for jscout
/// 0.4.0). Writing ANY other value — higher or lower — makes every database
/// read refuse, AND makes `jscout index` refuse to rebuild the file at all.
/// A docs plane sharing this database could not carry its own version.
#[test]
fn j2_global_schema_version_gates_the_whole_database() {
    let Some(bin) = jscout() else { return };
    let Some(_) = sqlite() else { return };
    let (_guard, root) = synthetic_repo();
    let root_str = root.to_str().unwrap();
    let db = root.join(".jscout.db");

    let indexed = proc::run(&bin, &["index", root_str], &root, &[]);
    assert!(indexed.ok, "index: {}", indexed.combined());

    // There is exactly one schema-version row for the entire database.
    let versions = sql_rows(&db, "select key from meta where key like '%schema%';");
    assert_eq!(
        versions,
        vec!["schema_version".to_string()],
        "exactly one global schema-version key exists, not one per plane"
    );
    let native = meta_value(&db, "schema_version").expect("schema_version present");
    println!("J2 native schema_version = {native}");
    assert!(
        native.parse::<i64>().is_ok(),
        "schema_version is a plain integer: {native}"
    );

    // Baseline read works.
    let before = proc::run(&bin, &["search", root_str, "helper", "--json"], &root, &[]);
    assert!(before.ok, "baseline search: {}", before.combined());

    for foreign in ["999", "1"] {
        sql(
            &db,
            &format!("update meta set value='{foreign}' where key='schema_version';"),
        );

        let read = proc::run(&bin, &["search", root_str, "helper"], &root, &[]);
        assert!(!read.ok, "search must refuse schema v{foreign}");
        assert_eq!(read.code, Some(1));
        assert!(
            read.combined().contains(&format!("uses schema v{foreign}"))
                && read.combined().contains(&format!("requires v{native}")),
            "search refusal names both versions: {}",
            read.combined()
        );

        // The gate is not just read-side: index refuses to repair the file.
        let rebuild = proc::run(&bin, &["index", root_str], &root, &[]);
        assert!(
            !rebuild.ok,
            "OBSERVED: index unexpectedly rebuilt over schema v{foreign}: {}",
            rebuild.combined()
        );
        assert!(
            rebuild
                .combined()
                .contains(&format!("unsupported durable schema v{foreign}")),
            "index refusal for v{foreign}: {}",
            rebuild.combined()
        );
        assert_eq!(
            meta_value(&db, "schema_version").as_deref(),
            Some(foreign),
            "a refused index leaves the foreign version in place; recovery is a NEW file"
        );
        println!(
            "J2 v{foreign}: search -> {}; index -> {}",
            read.combined().lines().next().unwrap_or("").trim(),
            rebuild.combined().lines().next().unwrap_or("").trim()
        );

        sql(
            &db,
            &format!("update meta set value='{native}' where key='schema_version';"),
        );
    }

    // The gate is at database open, not at process start: a command that never
    // opens the database still works with a foreign version present.
    sql(
        &db,
        "update meta set value='999' where key='schema_version';",
    );
    let stats = proc::run(&bin, &["stats", root_str], &root, &[]);
    assert!(
        stats.ok,
        "OBSERVED: `stats` parses source without opening the database, so it is \
         unaffected by the schema gate: {}",
        stats.combined()
    );
    println!("J2 gate location: `stats` still exits 0 with schema v999 on disk");
}

/// A second, independent global gate. `projection_version` behaves exactly
/// like `schema_version` on the read path, while `extraction_version`,
/// `resolution_hash` and `root` do not gate reads at all. The shared database
/// therefore carries at least TWO global version gates a docs plane would be
/// subject to, reinforcing the plan's separate-database decision.
#[test]
fn j2b_projection_version_is_a_second_global_gate() {
    let Some(bin) = jscout() else { return };
    let Some(_) = sqlite() else { return };
    let (_guard, root) = synthetic_repo();
    let root_str = root.to_str().unwrap();
    let db = root.join(".jscout.db");
    assert!(proc::run(&bin, &["index", root_str], &root, &[]).ok);

    let mut gating = Vec::new();
    let mut inert = Vec::new();
    for key in [
        "extraction_version",
        "projection_version",
        "resolution_hash",
        "root",
    ] {
        let original = meta_value(&db, key).unwrap_or_else(|| panic!("{key} present"));
        sql(
            &db,
            &format!("update meta set value='777' where key='{key}';"),
        );
        let read = proc::run(&bin, &["search", root_str, "helper", "--json"], &root, &[]);
        if read.ok {
            inert.push(key);
        } else {
            gating.push(key);
            println!(
                "J2b {key} gates reads: {}",
                read.combined().lines().next().unwrap_or("").trim()
            );
        }
        sql(
            &db,
            &format!("update meta set value='{original}' where key='{key}';"),
        );
        assert!(
            proc::run(&bin, &["search", root_str, "helper", "--json"], &root, &[]).ok,
            "restoring {key} restores readability"
        );
    }

    assert_eq!(
        gating,
        vec!["projection_version"],
        "OBSERVED: projection_version is the only additional read gate"
    );
    assert_eq!(
        inert,
        vec!["extraction_version", "resolution_hash", "root"],
        "OBSERVED: these meta keys do not gate reads"
    );
}

// ---------------------------------------------------------------------------
// J3 — THE STRUCTURAL SNAPSHOT GATE
// ---------------------------------------------------------------------------

/// PLAN CLAIM (docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md "Separate documentation database"): "every
/// read-only open requires a published structural snapshot".
///
/// A docs-only database — one that never runs a JS/TS index — would have no
/// `meta.snapshot` row. This test deletes that row and shows that EVERY
/// reader refuses, including `memory`, which belongs to the semantic plane
/// and has nothing to do with structural chunks. That is the concrete reason
/// a documentation plane cannot live in the shared database.
#[test]
fn j3_missing_structural_snapshot_refuses_every_reader() {
    let Some(bin) = jscout() else { return };
    let Some(_) = sqlite() else { return };
    let (_guard, root) = synthetic_repo();
    let root_str = root.to_str().unwrap();
    let db = root.join(".jscout.db");
    assert!(proc::run(&bin, &["index", root_str], &root, &[]).ok);

    let published = meta_value(&db, "snapshot").expect("a published snapshot exists after index");
    assert_eq!(
        published.len(),
        64,
        "the snapshot key is a 32-byte hex digest: {published}"
    );

    // Simulate a database with no published structural snapshot.
    sql(&db, "delete from meta where key='snapshot';");
    assert!(
        meta_value(&db, "snapshot").is_none(),
        "snapshot key removed"
    );

    let readers: Vec<Vec<&str>> = vec![
        vec!["search", root_str, "helper"],
        vec!["who-uses", root_str, "helper"],
        vec!["events", root_str],
        vec!["overview", root_str],
        vec!["memory", root_str, "helper"],
        vec!["neighborhood", root_str, "src/a.js"],
    ];
    for argv in &readers {
        let out = proc::run(&bin, argv, &root, &[]);
        assert!(
            !out.ok,
            "OBSERVED: `jscout {}` read a database with no published snapshot: {}",
            argv.join(" "),
            out.combined()
        );
        assert_eq!(out.code, Some(1), "`jscout {}`", argv.join(" "));
        assert!(
            out.combined()
                .contains("has no published structural snapshot"),
            "`jscout {}` refusal text: {}",
            argv.join(" "),
            out.combined()
        );
    }
    let sample = proc::run(&bin, &["memory", root_str, "helper"], &root, &[]);
    println!(
        "J3 exact refusal (semantic-plane command, structural gate):\n{}",
        sample.combined().trim()
    );

    // Only a structural index republishes it.
    assert!(proc::run(&bin, &["index", root_str], &root, &[]).ok);
    assert_eq!(
        meta_value(&db, "snapshot").as_deref(),
        Some(published.as_str()),
        "reindexing unchanged sources republishes the identical snapshot digest"
    );
    assert!(proc::run(&bin, &["search", root_str, "helper", "--json"], &root, &[]).ok);
}

// ---------------------------------------------------------------------------
// J4 — PUBLISH / UNPUBLISH WINDOW
// ---------------------------------------------------------------------------

/// PLAN CLAIM (review finding behind docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md): a code index unpublishes
/// and republishes the structural snapshot key, so a killed index leaves the
/// database unsearchable.
///
/// Observed and CONFIRMED: SIGKILL during the write phase leaves the database
/// with FULL content (`files` and `chunks` fully populated) but only two meta
/// keys — `schema_version` and `extraction_version`. `snapshot`,
/// `projection_version`, `resolution_hash` and `root` are all gone, so every
/// reader refuses. The content is present; the readiness gate is closed.
///
/// The assertion that always holds regardless of where the kill lands is the
/// invariant "snapshot key absent <=> readers refuse", which this test checks
/// on every sample; it additionally requires that at least one kill in the
/// sweep lands inside the unpublished window, since that is the actual claim.
#[test]
fn j4_killed_index_unpublishes_the_snapshot_while_content_remains() {
    let Some(bin) = jscout() else { return };
    let Some(_) = sqlite() else { return };
    let (_guard, root) = wide_repo(3000);
    let root_str = root.to_str().unwrap();
    let db = root.join(".jscout.db");

    // Measure one clean index, then aim kills at fractions of that duration.
    let started = Instant::now();
    let clean = proc::run(&bin, &["index", root_str], &root, &[]);
    let duration = started.elapsed();
    assert!(clean.ok, "clean index: {}", clean.combined());
    let healthy_keys = meta_keys(&db);
    assert_eq!(
        healthy_keys,
        vec![
            "extraction_version",
            "projection_version",
            "resolution_hash",
            "root",
            "schema_version",
            "snapshot"
        ],
        "meta key set after a normal index"
    );
    let healthy_files: i64 = sql_rows(&db, "select count(*) from files;")[0]
        .parse()
        .unwrap();
    let healthy_chunks: i64 = sql_rows(&db, "select count(*) from chunks;")[0]
        .parse()
        .unwrap();
    println!(
        "J4 clean index: {:?}, meta={:?}, files={healthy_files}, chunks={healthy_chunks}",
        duration, healthy_keys
    );

    let mut unpublished_samples = 0usize;
    let mut observed = Vec::new();
    for step in 0..10u32 {
        // 0.45 .. 0.99 of the measured index duration. The unpublished window
        // is the write phase, which observation puts in the last ~35% of the
        // run, so the sweep is weighted toward the tail.
        let fraction = 0.45 + 0.06 * f64::from(step);
        let delay = duration.mul_f64(fraction).max(Duration::from_millis(20));

        let mut child = Command::new(&bin)
            .arg("index")
            .arg(root_str)
            .current_dir(&root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn index");
        std::thread::sleep(delay);
        let _ = child.kill(); // SIGKILL on unix
        let _ = child.wait();

        let keys = meta_keys(&db);
        let snapshot_present = keys.iter().any(|key| key == "snapshot");
        let read = proc::run(&bin, &["search", root_str, "h1", "--json"], &root, &[]);

        // The invariant: readability tracks the snapshot key exactly.
        assert_eq!(
            snapshot_present,
            read.ok,
            "snapshot key presence must match readability; keys={keys:?} search={}",
            read.combined()
        );

        if !snapshot_present {
            unpublished_samples += 1;
            assert!(
                read.combined()
                    .contains("has no published structural snapshot"),
                "unpublished database refusal text: {}",
                read.combined()
            );
            // The data is still there — only the gate is closed.
            let files: i64 = sql_rows(&db, "select count(*) from files;")[0]
                .parse()
                .unwrap();
            let chunks: i64 = sql_rows(&db, "select count(*) from chunks;")[0]
                .parse()
                .unwrap();
            assert_eq!(
                (files, chunks),
                (healthy_files, healthy_chunks),
                "a killed index leaves full content behind; only meta is unpublished"
            );
            // OBSERVED: the unpublished key set is not a single fixed value —
            // `root` is rewritten slightly before the publication keys, so the
            // window shows either {schema_version, extraction_version} or that
            // set plus `root`. What is invariant is that the three publication
            // keys go away together and the two durable-format keys survive.
            for durable in ["schema_version", "extraction_version"] {
                assert!(
                    keys.iter().any(|key| key == durable),
                    "`{durable}` survives the unpublished window; keys={keys:?}"
                );
            }
            for publication in ["snapshot", "projection_version", "resolution_hash"] {
                assert!(
                    !keys.iter().any(|key| key == publication),
                    "OBSERVED: `{publication}` must be cleared together with `snapshot`; \
                     keys={keys:?}"
                );
            }
            observed.push(format!("{:?} -> UNPUBLISHED keys={keys:?}", delay));
        } else {
            observed.push(format!("{:?} -> still published", delay));
        }

        // Restore for the next sample.
        assert!(proc::run(&bin, &["index", root_str], &root, &[]).ok);
    }

    println!("J4 kill sweep: {observed:?}");
    assert!(
        unpublished_samples > 0,
        "expected at least one kill to land inside the unpublish window; \
         samples: {observed:?}"
    );
    println!("J4: {unpublished_samples}/10 kills left the database complete-but-unsearchable");
}

/// The same claim from the other side, and a genuine NUANCE the plan does not
/// state: not every *failed* index unpublishes. A failure raised before the
/// write transaction opens (here: a read-only database file) leaves the
/// previous snapshot intact and the database fully searchable.
///
/// So "a killed index leaves the database unsearchable" is confirmed, but
/// "any failed index does" is not: the unpublished window is the write phase,
/// not the whole command.
#[test]
fn j4b_failure_before_the_write_phase_preserves_the_published_snapshot() {
    use std::os::unix::fs::PermissionsExt;

    let Some(bin) = jscout() else { return };
    let Some(_) = sqlite() else { return };
    let (_guard, root) = synthetic_repo();
    let root_str = root.to_str().unwrap();
    let db = root.join(".jscout.db");
    assert!(proc::run(&bin, &["index", root_str], &root, &[]).ok);
    let published = meta_value(&db, "snapshot").expect("snapshot published");

    let original = std::fs::metadata(&db).unwrap().permissions();
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
    let failed = proc::run(&bin, &["index", root_str], &root, &[]);
    std::fs::set_permissions(&db, original).unwrap();

    assert!(!failed.ok, "a read-only database must fail the index");
    assert!(
        failed.combined().contains("readonly database"),
        "failure mode: {}",
        failed.combined()
    );
    assert_eq!(
        meta_value(&db, "snapshot").as_deref(),
        Some(published.as_str()),
        "OBSERVED: this failure class preserves the published snapshot verbatim"
    );
    let read = proc::run(&bin, &["search", root_str, "helper", "--json"], &root, &[]);
    assert!(
        read.ok,
        "the database remains searchable after this failure class: {}",
        read.combined()
    );
    println!(
        "J4b: pre-write failure `{}` left snapshot {} searchable",
        failed.combined().lines().next().unwrap_or("").trim(),
        &published[..12]
    );

    // A per-file read rejection is likewise not a publication failure: the
    // index succeeds, republishes, and simply reports the rejected input.
    let unreadable = root.join("src").join("b.js");
    let original_file = std::fs::metadata(&unreadable).unwrap().permissions();
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();
    let degraded = proc::run(&bin, &["index", root_str], &root, &[]);
    std::fs::set_permissions(&unreadable, original_file).unwrap();
    assert!(
        degraded.ok,
        "a rejected input file does not fail the index: {}",
        degraded.combined()
    );
    assert!(
        degraded.combined().contains("rejected=1"),
        "the rejection is reported: {}",
        degraded.combined()
    );
    let republished = meta_value(&db, "snapshot").expect("snapshot present");
    assert_ne!(
        republished, published,
        "a smaller corpus publishes a different snapshot digest"
    );
    assert!(proc::run(&bin, &["search", root_str, "helper", "--json"], &root, &[]).ok);
}

// ---------------------------------------------------------------------------
// J5 — SEPARATE DATABASE ISOLATION (positive control)
// ---------------------------------------------------------------------------

/// PLAN CLAIM (PLAN.md G24 entry #1): "The main database, structural
/// snapshots, configuration fingerprints, watch generations, and semantic
/// freshness are untouched by every docs operation", and (acceptance) "a code
/// reindex and any docs operation are mutually invisible, with foreign-plane
/// state byte-identical either way".
///
/// This is the positive control for the central architectural decision: two
/// sqlite files in one directory, each with its own schema version, are fully
/// independent. A corrupt / version-bumped docs database leaves the main one
/// fully readable, a broken main database leaves the docs file byte-identical,
/// and a code reindex never touches the docs file's bytes.
#[test]
fn j5_separate_databases_are_mutually_isolated() {
    let Some(bin) = jscout() else { return };
    let Some(_) = sqlite() else { return };
    let (_guard, root) = synthetic_repo();
    let root_str = root.to_str().unwrap();
    let main_db = root.join(".jscout.db");
    let docs_db = root.join(".jscout-docs.db");

    assert!(proc::run(&bin, &["index", root_str], &root, &[]).ok);
    let native = meta_value(&main_db, "schema_version").unwrap();

    // A docs-plane database with its OWN schema version and its OWN readiness
    // key, exactly as the plan describes.
    let create = sql(
        &docs_db,
        "create table meta(key text primary key, value text not null); \
         insert into meta values('schema_version','1'),('doc_snapshot','abc'); \
         create table doc_chunks(id integer primary key, path text, body text); \
         insert into doc_chunks(path,body) values('README.md','hello docs');",
    );
    assert!(create.ok, "create docs db: {}", create.combined());
    assert_eq!(meta_value(&docs_db, "schema_version").as_deref(), Some("1"));
    assert_ne!(
        native, "1",
        "the two planes deliberately carry different schema versions"
    );

    // Direction 1: a code reindex must not touch the docs database.
    let docs_hash_before = blake3_file(&docs_db);
    let reindex = proc::run(&bin, &["index", root_str], &root, &[]);
    assert!(reindex.ok, "reindex: {}", reindex.combined());
    assert!(
        reindex.combined().contains("indexed 3 files"),
        "the docs database is not itself indexed as a source file: {}",
        reindex.combined()
    );
    assert_eq!(
        blake3_file(&docs_db),
        docs_hash_before,
        "OBSERVED: the docs database is byte-identical after a code reindex"
    );

    // Direction 2: version-bump then hard-corrupt the docs database.
    sql(
        &docs_db,
        "update meta set value='999' where key='schema_version';",
    );
    let mut bytes = std::fs::read(&docs_db).unwrap();
    bytes[..18].copy_from_slice(b"GARBAGE-NOT-SQLITE");
    std::fs::write(&docs_db, &bytes).unwrap();
    let docs_unreadable = sql(&docs_db, "select count(*) from meta;");
    assert!(
        !docs_unreadable.ok || docs_unreadable.combined().contains("not a database"),
        "the docs database must now be genuinely unreadable: {}",
        docs_unreadable.combined()
    );

    for argv in [
        vec!["search", root_str, "helper", "--json"],
        vec!["who-uses", root_str, "helper"],
        vec!["index", root_str],
    ] {
        let out = proc::run(&bin, &argv, &root, &[]);
        assert!(
            out.ok,
            "main plane must be unaffected by a corrupt docs database; \
             `jscout {}` -> {}",
            argv.join(" "),
            out.combined()
        );
    }
    println!("J5: main plane fully operational with a corrupt .jscout-docs.db");

    // Direction 3: break the main database; the docs file must not move.
    let docs_hash_corrupt = blake3_file(&docs_db);
    sql(
        &main_db,
        "update meta set value='999' where key='schema_version';",
    );
    let broken = proc::run(&bin, &["search", root_str, "helper"], &root, &[]);
    assert!(!broken.ok, "main database is now version-broken");
    assert!(
        broken.combined().contains("uses schema v999"),
        "{}",
        broken.combined()
    );
    assert_eq!(
        blake3_file(&docs_db),
        docs_hash_corrupt,
        "OBSERVED: the docs database is byte-identical after the main plane breaks"
    );
    println!("J5: docs database byte-identical across a main-plane schema break");

    // And the main plane recovers without any docs involvement.
    sql(
        &main_db,
        &format!("update meta set value='{native}' where key='schema_version';"),
    );
    assert!(proc::run(&bin, &["search", root_str, "helper", "--json"], &root, &[]).ok);
}

/// The negative control for the same decision: if the docs plane tried to use
/// the SAME database machinery, jscout would reject the file outright. This
/// shows the isolation in J5 comes from the file boundary, not from jscout
/// tolerating foreign schemas.
#[test]
fn j5b_jscout_refuses_a_foreign_database_file() {
    let Some(bin) = jscout() else { return };
    let Some(_) = sqlite() else { return };
    let (_guard, root) = synthetic_repo();
    let root_str = root.to_str().unwrap();
    assert!(proc::run(&bin, &["index", root_str], &root, &[]).ok);

    // A well-formed sqlite file that is not a jscout index.
    let foreign = root.join("docs-plane.db");
    assert!(
        sql(
            &foreign,
            "create table meta(key text primary key, value text not null); \
         insert into meta values('schema_version','1'),('doc_snapshot','abc');",
        )
        .ok
    );
    let foreign_str = foreign.to_str().unwrap();

    let read = proc::run(
        &bin,
        &["search", root_str, "helper", "--database", foreign_str],
        &root,
        &[],
    );
    assert!(!read.ok, "reading a foreign database must fail");
    assert!(
        read.combined().contains("uses schema v1"),
        "{}",
        read.combined()
    );

    let write = proc::run(
        &bin,
        &["index", root_str, "--database", foreign_str],
        &root,
        &[],
    );
    assert!(!write.ok, "indexing into a foreign database must fail");
    assert!(
        write.combined().contains("unsupported durable schema v1"),
        "{}",
        write.combined()
    );

    // A non-sqlite file is refused with a distinct, specific message.
    let junk = root.join("not-a-db.bin");
    std::fs::write(&junk, b"GARBAGE-NOT-SQLITE-AT-ALL").unwrap();
    let junk_read = proc::run(
        &bin,
        &[
            "search",
            root_str,
            "helper",
            "--database",
            junk.to_str().unwrap(),
        ],
        &root,
        &[],
    );
    assert!(!junk_read.ok);
    assert!(
        junk_read.combined().contains("has no readable schema"),
        "{}",
        junk_read.combined()
    );
    println!(
        "J5b refusals: foreign-schema -> `{}`; non-sqlite -> `{}`",
        read.combined().lines().next().unwrap_or("").trim(),
        junk_read.combined().lines().next().unwrap_or("").trim()
    );

    // The real database is untouched by all of that.
    assert!(proc::run(&bin, &["search", root_str, "helper", "--json"], &root, &[]).ok);
}

// ---------------------------------------------------------------------------
// J6 — RRF CONSTANT AND FRESHNESS-RELEVANT RETRIEVAL BEHAVIOUR
// ---------------------------------------------------------------------------

/// PLAN CLAIM (PLAN.md G24 entry #3 and docs/plans/g24-markdown-retrieval-proposal-2026-08-24.md "Freshness ordering"):
/// retrieval uses "reciprocal-rank fusion", and the pipeline the plan mirrors
/// is BM25 + vector -> RRF -> optional rerank -> freshness reordering.
///
/// Observed on the real binary: fused hit scores are exactly `1/(60 + rank)`
/// for every rank measured, so the RRF constant is k = 60. The plan never
/// names k; this test pins the real value so the docs plane can reuse the same
/// fusion, and so the "unbounded decay" argument below has a concrete basis.
#[test]
fn j6_rrf_constant_is_sixty() {
    let Some(bin) = jscout() else { return };
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..12 {
        std::fs::write(
            src.join(format!("m{i}.js")),
            format!(
                "export function widget{i}(x) {{ return widgetHelper(x) + {i}; }}\n\
                 export function widgetHelper(y) {{ return y * 2; }}\n"
            ),
        )
        .unwrap();
    }
    let root_str = root.to_str().unwrap();
    assert!(proc::run(&bin, &["index", root_str], &root, &[]).ok);

    let out = proc::run(
        &bin,
        &[
            "search",
            root_str,
            "widgetHelper",
            "--limit",
            "20",
            "--debug-json",
        ],
        &root,
        &[],
    );
    assert!(out.ok, "debug-json search: {}", out.combined());

    // Retrieval status is reported explicitly; with no [embedding] provider
    // the vector and reranker legs are inert, so scores are pure BM25-rank RRF.
    assert!(
        out.stdout.contains("\"lexical\": \"active\""),
        "lexical leg active: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("\"vector\": \"disabled\""),
        "vector leg disabled without a provider: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("\"reranker\": \"disabled\""),
        "reranker disabled without a profile: {}",
        out.stdout
    );

    let scores = debug_json_scores(&out.stdout);
    assert!(
        scores.len() >= 10,
        "need a decent rank ladder, got {}",
        scores.len()
    );
    for (index, score) in scores.iter().enumerate() {
        let rank = index as f64 + 1.0;
        let k = 1.0 / score - rank;
        assert!(
            (k - 60.0).abs() < 1e-6,
            "rank {rank}: score {score} implies k={k}, expected exactly 60"
        );
    }
    for pair in scores.windows(2) {
        assert!(
            pair[0] > pair[1],
            "fused scores are strictly decreasing: {pair:?}"
        );
    }
    println!(
        "J6 RRF: k=60 confirmed over {} ranks; score(1)={}, score({})={}",
        scores.len(),
        scores[0],
        scores.len(),
        scores[scores.len() - 1]
    );

    // `--lexical-only` yields the identical ladder: the fusion formula, not the
    // leg count, produces these values.
    let lexical = proc::run(
        &bin,
        &[
            "search",
            root_str,
            "widgetHelper",
            "--limit",
            "20",
            "--lexical-only",
            "--debug-json",
        ],
        &root,
        &[],
    );
    assert!(lexical.ok);
    assert_eq!(
        debug_json_scores(&lexical.stdout),
        scores,
        "--lexical-only reproduces the same RRF ladder"
    );

    // Determinism: the same query twice gives byte-identical output.
    let again = proc::run(
        &bin,
        &[
            "search",
            root_str,
            "widgetHelper",
            "--limit",
            "20",
            "--debug-json",
        ],
        &root,
        &[],
    );
    assert_eq!(again.stdout, out.stdout, "search output is deterministic");
}

/// PLAN CLAIM (PLAN.md G24 entry preamble): "a multiplicative score decay is
/// not bounded in effect once applied to rank-fusion scores" — the review
/// finding that replaced score decay with the bounded order-based freshness
/// rule (`max_rank_movement`, default 2).
///
/// This computes the claim against the REAL score ladder measured above. With
/// k = 60 the gap between adjacent fused scores is ~1.6% at the top of the
/// list, so even a mild multiplicative penalty displaces a hit by many ranks —
/// far past any `max_rank_movement` bound. CONFIRMED.
#[test]
fn j6b_multiplicative_decay_on_rrf_scores_is_unbounded_in_rank_movement() {
    let Some(bin) = jscout() else { return };
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let src = root.join("src");
    std::fs::create_dir_all(&src).unwrap();
    for i in 0..20 {
        std::fs::write(
            src.join(format!("d{i}.js")),
            format!(
                "export function decayProbe{i}(x) {{ return decayProbeShared(x) + {i}; }}\n\
                 export function decayProbeShared(y) {{ return y - 1; }}\n"
            ),
        )
        .unwrap();
    }
    let root_str = root.to_str().unwrap();
    assert!(proc::run(&bin, &["index", root_str], &root, &[]).ok);
    let out = proc::run(
        &bin,
        &[
            "search",
            root_str,
            "decayProbeShared",
            "--limit",
            "30",
            "--debug-json",
        ],
        &root,
        &[],
    );
    assert!(out.ok, "{}", out.combined());
    let scores = debug_json_scores(&out.stdout);
    assert!(scores.len() >= 20, "need >=20 ranks, got {}", scores.len());

    // Adjacent-rank separation at the head of the list.
    let head_gap = (scores[0] - scores[1]) / scores[0];
    assert!(
        head_gap < 0.02,
        "adjacent RRF scores at rank 1/2 differ by {:.4}%, expected under 2%",
        head_gap * 100.0
    );

    // How far does a multiplicative decay push the top hit?
    let mut movements = Vec::new();
    for factor in [0.99_f64, 0.95, 0.90, 0.75, 0.50] {
        let decayed = scores[0] * factor;
        // New one-based rank of the decayed top hit among the untouched rest.
        let passed = scores[1..].iter().filter(|s| **s > decayed).count();
        movements.push((factor, passed));
    }
    println!("J6b decay of the rank-1 hit -> ranks lost: {movements:?}");

    let bound = 2usize; // the plan's default max_rank_movement
    let ten_percent = movements
        .iter()
        .find(|(factor, _)| (*factor - 0.90).abs() < 1e-9)
        .map(|(_, passed)| *passed)
        .unwrap();
    assert!(
        ten_percent > bound,
        "OBSERVED: a 10% multiplicative decay moved the top hit {ten_percent} ranks, \
         which must exceed the plan's max_rank_movement bound of {bound} for the \
         review finding to hold"
    );
    let half = movements
        .iter()
        .find(|(factor, _)| (*factor - 0.50).abs() < 1e-9)
        .map(|(_, passed)| *passed)
        .unwrap();
    assert!(
        half >= scores.len() - 1,
        "OBSERVED: a 0.5 decay sinks the top hit past every retrieved candidate \
         ({half} of {} passed)",
        scores.len() - 1
    );
    // Monotonic: a harsher decay never moves the hit less far.
    for pair in movements.windows(2) {
        assert!(
            pair[1].1 >= pair[0].1,
            "movement must be monotone in the decay factor: {movements:?}"
        );
    }
}

/// Freshness-relevant behaviour of the SHIPPED binary, which is the baseline
/// the plan's docs plane departs from.
///
/// PLAN CLAIM: every docs hit "exposes its freshness basis (`git`,
/// `working_tree`, `observed`, `unknown`), the basis value, base rank, and
/// movement", and docs `--vector` "require[s] vector participation: error when
/// no [embedding] provider is configured".
///
/// Observed: code search hits carry NO temporal metadata of any kind, and code
/// search's `--vector` does NOT error without a provider — it silently reports
/// `vector: disabled` and exits 0. Both docs behaviours are therefore genuinely
/// new surface, which is exactly why the plan says the docs CLI contract is
/// "defined directly rather than by analogy with code search". Recorded here so
/// the divergence is deliberate rather than accidental.
#[test]
fn j6c_code_search_has_no_freshness_surface_and_vector_never_errors() {
    let Some(bin) = jscout() else { return };
    let (_guard, root) = synthetic_repo();
    let root_str = root.to_str().unwrap();
    assert!(proc::run(&bin, &["index", root_str], &root, &[]).ok);

    let out = proc::run(
        &bin,
        &["search", root_str, "helper", "--debug-json"],
        &root,
        &[],
    );
    assert!(out.ok, "{}", out.combined());

    // No temporal or rank-movement surface anywhere in the diagnostic payload.
    for absent in [
        "freshness",
        "basis",
        "base_rank",
        "movement",
        "author_time",
        "committer_time",
        "working_tree",
        "mtime",
        "observed_at",
    ] {
        assert!(
            !out.stdout.contains(absent),
            "OBSERVED: code search debug JSON unexpectedly contains `{absent}`: {}",
            out.stdout
        );
    }
    println!(
        "J6c: code-search --debug-json exposes no temporal field; docs freshness is new surface"
    );

    // `--vector` with no configured provider: exit 0, vector reported disabled.
    // No provider is configured in this temp repo, so nothing can be called.
    let forced = proc::run(
        &bin,
        &["search", root_str, "helper", "--vector", "--debug-json"],
        &root,
        &[],
    );
    assert!(
        forced.ok,
        "OBSERVED: code search `--vector` does NOT error without a provider; \
         the plan's docs `--vector` must therefore add that behaviour itself: {}",
        forced.combined()
    );
    assert!(
        forced.stdout.contains("\"vector\": \"disabled\""),
        "`--vector` degrades silently to disabled: {}",
        forced.stdout
    );
    println!("J6c: `search --vector` without a provider -> exit 0, vector=\"disabled\"");
}
