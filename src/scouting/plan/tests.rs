use anyhow::Result;
use rusqlite::params;
use serde_json::json;

use crate::{indexer, store};

fn publish_vocabulary_artifact(
    conn: &rusqlite::Connection,
    artifact_type: &str,
    name: Option<&str>,
    body: serde_json::Value,
    supports: &[(&str, &str, &str, i64, i64)],
    supersedes: Option<i64>,
) -> Result<i64> {
    let snapshot = crate::structural::current_snapshot(conn)?;
    let body = serde_json::to_string(&body)?;
    conn.execute(
        "INSERT INTO semantic_artifacts(
           supersedes_artifact_id,artifact_type,canonical_name,body_json,model,
           prompt_version,confidence,source_snapshot,created_at,artifact_fingerprint
         ) VALUES(?1,?2,?3,?4,'test','test/v1','likely',?5,
                  '2026-08-12T00:00:00Z',?6)",
        params![
            supersedes,
            artifact_type,
            name,
            body,
            snapshot,
            format!("fp-{artifact_type}-{}", conn.last_insert_rowid() + 1),
        ],
    )?;
    let artifact_id = conn.last_insert_rowid();
    for (claim_path, anchor, file, start, end) in supports {
        let source_hash: String =
            conn.query_row("SELECT hash FROM files WHERE path=?1", [file], |row| {
                row.get(0)
            })?;
        let context_hash = crate::semantic::context_hash(conn, anchor)?;
        conn.execute(
            "INSERT INTO semantic_supports(
               artifact_id,claim_path,anchor_key,evidence_file,evidence_start_line,
               evidence_end_line,source_hash,context_hash,confidence
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'likely')",
            params![
                artifact_id,
                claim_path,
                anchor,
                file,
                start,
                end,
                source_hash,
                context_hash,
            ],
        )?;
    }
    Ok(artifact_id)
}

#[test]
fn heavier_referenced_subjects_lead_their_tier() -> Result<()> {
    let repo = tempfile::tempdir()?;
    // `aardvark` sorts first by key; `zebra` carries the references. The
    // weighted order must beat the key order within the same tier, so a
    // bounded selection cap keeps the load-bearing symbol.
    std::fs::create_dir_all(repo.path().join("aardvark"))?;
    std::fs::create_dir_all(repo.path().join("zebra"))?;
    std::fs::write(
        repo.path().join("aardvark/index.ts"),
        "export function aardvark() { return 1; }\n",
    )?;
    std::fs::write(
        repo.path().join("zebra/index.ts"),
        "export function zebra() { return 2; }\n",
    )?;
    std::fs::write(
        repo.path().join("caller-one.ts"),
        "import { zebra } from './zebra';\nexport function one() { return zebra(); }\n",
    )?;
    std::fs::write(
        repo.path().join("caller-two.ts"),
        "import { zebra } from './zebra';\nexport function two() { return zebra(); }\n",
    )?;
    std::fs::write(
        repo.path().join("caller-three.ts"),
        "import { zebra } from './zebra';\nexport function three() { return zebra(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let cards = super::automatic_card_subjects(&conn)?;
    let card_keys = cards
        .iter()
        .map(|(anchor, _)| anchor.as_str())
        .collect::<Vec<_>>();
    let zebra_card = card_keys
        .iter()
        .position(|key| key.contains("::zebra@"))
        .expect("zebra card subject");
    let aardvark_card = card_keys
        .iter()
        .position(|key| key.contains("::aardvark@"))
        .expect("aardvark card subject");
    assert!(
        zebra_card < aardvark_card,
        "weighted order must beat key order: {card_keys:?}"
    );

    let seeds = super::automatic_seeds(repo.path(), &conn)?;
    let seed_keys = seeds
        .iter()
        .map(|(anchor, _)| anchor.as_str())
        .collect::<Vec<_>>();
    let zebra_seed = seed_keys
        .iter()
        .position(|key| key.contains("::zebra@"))
        .expect("zebra seed");
    let aardvark_seed = seed_keys
        .iter()
        .position(|key| key.contains("::aardvark@"))
        .expect("aardvark seed");
    assert!(
        zebra_seed < aardvark_seed,
        "weighted order must beat key order: {seed_keys:?}"
    );
    Ok(())
}

fn vocabulary_fixture() -> Result<(tempfile::TempDir, rusqlite::Connection)> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("domain.ts"),
        "export function settle() { return 'invoice'; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    Ok((repo, conn))
}

#[test]
fn concepts_group_only_supported_current_vocabulary() -> Result<()> {
    let (_repo, conn) = vocabulary_fixture()?;
    let anchor = "sym:domain.ts#::settle@1";
    let workflow = publish_vocabulary_artifact(
        &conn,
        "workflow",
        Some("  Invoice   Settlement  "),
        json!({
            "description": "prose must not become vocabulary",
            "participants": [{"anchor": anchor, "role": "settles"}],
        }),
        &[("/name", anchor, "domain.ts", 1, 1)],
        None,
    )?;
    publish_vocabulary_artifact(
        &conn,
        "card",
        Some(anchor),
        json!({
            "purpose": "free prose also stays out",
            "domain_terms": ["invoice settlement", "Invoice-Settlement", "unsupported term"],
            "side_effects": ["another prose field"],
        }),
        &[
            ("/purpose", anchor, "domain.ts", 1, 1),
            ("/domain_terms/0", anchor, "domain.ts", 1, 1),
            ("/domain_terms/1", anchor, "domain.ts", 1, 1),
            ("/side_effects/0", anchor, "domain.ts", 1, 1),
        ],
        None,
    )?;

    // A supported successor excludes its predecessor from current
    // vocabulary, including the old workflow name.
    publish_vocabulary_artifact(
        &conn,
        "workflow",
        Some("Payment Completion"),
        json!({
            "description": "successor",
            "participants": [{"anchor": anchor, "role": "settles"}],
        }),
        &[("/name", anchor, "domain.ts", 1, 1)],
        Some(workflow),
    )?;

    let plan = super::concepts(&conn, &[])?;
    assert_eq!(
        plan.items
            .iter()
            .map(|item| item.canonical_name.as_str())
            .collect::<Vec<_>>(),
        [
            "invoice settlement",
            "invoice-settlement",
            "payment completion"
        ]
    );
    assert_eq!(plan.groups_discovered, 3);
    assert!(
        plan.items
            .iter()
            .all(|item| item.rendered.contains("- body:")),
        "validated child bodies are model context even though their free prose cannot create groups"
    );
    assert!(
        plan.items
            .iter()
            .all(|item| item.canonical_name != "unsupported term"),
        "a card term without a support at its exact claim path is excluded"
    );
    assert!(
        plan.items
            .iter()
            .all(|item| item.canonical_name != "invoice settlement" || item.child_count == 1),
        "the superseded workflow must not remain a child"
    );
    Ok(())
}

#[test]
fn concepts_group_exact_unicode_normalization_but_not_near_variants() -> Result<()> {
    let (_repo, conn) = vocabulary_fixture()?;
    let anchor = "sym:domain.ts#::settle@1";
    // Full-width Latin normalizes under NFKC; punctuation remains a hard
    // boundary, so the hyphenated spelling gets a different group.
    publish_vocabulary_artifact(
        &conn,
        "card",
        Some(anchor),
        json!({"domain_terms": ["ＣＡＦÉ   Ledger", "café ledger", "café-ledger"]}),
        &[
            ("/domain_terms/0", anchor, "domain.ts", 1, 1),
            ("/domain_terms/1", anchor, "domain.ts", 1, 1),
            ("/domain_terms/2", anchor, "domain.ts", 1, 1),
        ],
        None,
    )?;

    let automatic = super::concepts(&conn, &[])?;
    assert_eq!(automatic.items.len(), 2);
    let exact = automatic
        .items
        .iter()
        .find(|item| item.canonical_name == "café ledger")
        .expect("NFKC/lower/whitespace group");
    assert_eq!(exact.aliases, ["CAFÉ Ledger", "café ledger"]);
    assert_eq!(exact.sources[0].aliases.len(), 2);
    assert_eq!(exact.sources[0].aliases[0].claim_path, "/domain_terms/0");
    assert!(exact.rendered.contains("artifact_id:"));
    assert!(exact.rendered.contains("fingerprint:"));
    assert!(exact.rendered.contains("claim_path:"));
    assert!(exact.rendered.contains("lines=1-1"));

    let explicit = super::concepts(&conn, &["  CAFÉ\tLedger ".into(), "ＣＡＦÉ Ledger".into()])?;
    assert_eq!(explicit.mode, "explicit");
    assert_eq!(explicit.items.len(), 1, "repeated normalized terms dedupe");
    assert_eq!(explicit.items[0].canonical_name, "café ledger");
    let error = super::concepts(&conn, &["cafe ledger".into()])
        .expect_err("accent-less near variant does not resolve fuzzily");
    assert!(error.to_string().contains("no current supported"));
    Ok(())
}

#[test]
fn concept_planning_refuses_non_fresh_children_before_reuse_or_model_spend() -> Result<()> {
    let (repo, conn) = vocabulary_fixture()?;
    let anchor = "sym:domain.ts#::settle@1";
    let child = publish_vocabulary_artifact(
        &conn,
        "card",
        Some(anchor),
        json!({"domain_terms": ["invoice settlement"]}),
        &[("/domain_terms/0", anchor, "domain.ts", 1, 1)],
        None,
    )?;
    assert_eq!(
        crate::semantic::load_artifact(&conn, child)?
            .expect("child")
            .freshness,
        "fresh"
    );

    std::fs::write(
        repo.path().join("domain.ts"),
        "export function settle() { return 'revised invoice'; }\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    assert_ne!(
        crate::semantic::load_artifact(&conn, child)?
            .expect("child")
            .freshness,
        "fresh"
    );

    let automatic = super::concepts(&conn, &[])?;
    assert!(automatic.items.is_empty());
    assert_eq!(automatic.skipped.len(), 1);
    assert!(
        automatic.skipped[0]
            .reason
            .contains("refresh those children first")
    );

    let error = super::concepts(&conn, &["invoice settlement".into()])
        .expect_err("explicit scouting must fail before reuse or a provider call");
    assert!(error.to_string().contains("refresh those children first"));
    Ok(())
}

#[test]
fn concept_support_overflow_is_reported_and_never_truncated() -> Result<()> {
    let (_repo, conn) = vocabulary_fixture()?;
    let anchor = "sym:domain.ts#::settle@1";
    let artifact_id = publish_vocabulary_artifact(
        &conn,
        "card",
        Some(anchor),
        json!({"domain_terms": ["invoice settlement"]}),
        &[("/domain_terms/0", anchor, "domain.ts", 1, 1)],
        None,
    )?;
    let source_hash: String =
        conn.query_row("SELECT hash FROM files WHERE path='domain.ts'", [], |row| {
            row.get(0)
        })?;
    for index in 1..=super::MAX_CONCEPT_SOURCE_SUPPORTS {
        let anchor = format!("anchor:{index}");
        let context_hash = crate::semantic::context_hash(&conn, &anchor)?;
        conn.execute(
            "INSERT INTO semantic_supports(
               artifact_id,claim_path,anchor_key,evidence_file,evidence_start_line,
               evidence_end_line,source_hash,context_hash,confidence
             ) VALUES(?1,'/domain_terms/0',?2,'domain.ts',1,1,
                      ?3,?4,'likely')",
            params![artifact_id, anchor, source_hash, context_hash],
        )?;
    }

    let automatic = super::concepts(&conn, &[])?;
    assert!(automatic.items.is_empty());
    assert_eq!(automatic.skipped.len(), 1);
    assert_eq!(automatic.skipped[0].children, 1);
    assert!(
        automatic.skipped[0]
            .reason
            .contains("161 exact source coordinates")
    );
    assert!(
        automatic.skipped[0]
            .reason
            .contains("never silently truncated")
    );

    let error = super::concepts(&conn, &["invoice settlement".into()])
        .expect_err("explicit overflow must fail before a model call");
    assert!(error.to_string().contains("161 exact source coordinates"));
    Ok(())
}

#[test]
fn automatic_plans_are_deterministic_and_dedupe_equal_boundaries() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("index.ts"),
        "export function first() { return shared(); }\n\
         export function second() { return shared(); }\n\
         function shared() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let first = super::workflows(repo.path(), &conn, &[], 2, 31)?;
    let second = super::workflows(repo.path(), &conn, &[], 2, 31)?;
    assert_eq!(
        serde_json::to_value(&first)?,
        serde_json::to_value(&second)?
    );
    assert_eq!(first.mode, "automatic");
    assert!(!first.items.is_empty());
    assert!(
        first
            .items
            .iter()
            .all(|item| item.sources == ["exported-entry-point"])
    );
    let original = &first.items[0].candidate_set;
    let mut alternate_seed = original.clone();
    alternate_seed.seeds = vec!["sym:index.ts#::alternate@1".into()];
    for candidate in &mut alternate_seed.candidates {
        candidate.seed = !candidate.seed;
    }
    assert_eq!(
        super::candidate_boundary_fingerprint(original),
        super::candidate_boundary_fingerprint(&alternate_seed),
        "automatic dedupe must use the closed member set, not seed identity",
    );
    Ok(())
}

#[test]
fn automatic_seed_directions_follow_runtime_boundary_roles() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let conn = store::open(repo.path())?;
    conn.execute(
        "INSERT INTO files(path, hash, role) VALUES('runtime.ts','h','production')",
        [],
    )?;
    let production_file = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO files(path, hash, role) VALUES('runtime.test.ts','t','test')",
        [],
    )?;
    let test_file = conn.last_insert_rowid();
    for (key, kind, file) in [
        (
            "sym:runtime.ts#::dispatch@1",
            "symbol",
            Some(production_file),
        ),
        ("sym:runtime.ts#::inject@3", "symbol", Some(production_file)),
        ("sym:runtime.ts#::handle@2", "symbol", Some(production_file)),
        (
            "sym:runtime.test.ts#::testOnly@1",
            "symbol",
            Some(test_file),
        ),
        ("entity:registry:job", "entity", None),
    ] {
        conn.execute(
            "INSERT INTO graph_nodes(node_key,node_kind,display_name,file_id,meta_json)
             VALUES(?1,?2,?1,?3,'{}')",
            params![key, kind, file],
        )?;
    }
    for (src, dst, kind) in [
        (
            "sym:runtime.ts#::dispatch@1",
            "entity:registry:job",
            "dispatches",
        ),
        (
            "entity:registry:job",
            "sym:runtime.ts#::handle@2",
            "registered_handler",
        ),
        (
            "entity:registry:job",
            "sym:runtime.ts#::handle@2",
            "handles_route",
        ),
        (
            "sym:runtime.test.ts#::testOnly@1",
            "entity:registry:job",
            "produces_job",
        ),
        (
            "sym:runtime.ts#::inject@3",
            "entity:registry:job",
            "injects",
        ),
    ] {
        conn.execute(
            "INSERT INTO resolved_edges(src_key,dst_key,kind,confidence,provenance)
             VALUES(?1,?2,?3,'certain','test')",
            params![src, dst, kind],
        )?;
    }

    let seeds = super::automatic_seeds(repo.path(), &conn)?;
    assert_eq!(seeds.len(), 3, "test-role endpoints stay out of auto seeds");
    assert_eq!(seeds[0].0, "sym:runtime.ts#::handle@2");
    assert_eq!(
        seeds[0].1,
        ["runtime:handles_route", "runtime:registered_handler"]
    );
    assert_eq!(seeds[1].0, "sym:runtime.ts#::dispatch@1");
    assert_eq!(seeds[1].1, ["runtime:dispatches"]);
    assert_eq!(seeds[2].0, "sym:runtime.ts#::inject@3");
    assert_eq!(seeds[2].1, ["runtime:injects"]);
    Ok(())
}

#[test]
fn automatic_card_selection_unions_its_sources_deterministically() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("index.ts"),
        "export function first() { return helper(); }\n\
         function helper() { return 1; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let first = super::cards(repo.path(), &conn, &[])?;
    let second = super::cards(repo.path(), &conn, &[])?;
    assert_eq!(
        serde_json::to_value(&first)?,
        serde_json::to_value(&second)?,
        "card planning must be deterministic"
    );
    assert_eq!(first.mode, "automatic");
    assert_eq!(first.items.len(), 1, "only exported symbols are subjects");
    assert_eq!(first.items[0].sources, ["exported-symbol"]);
    assert_eq!(first.sources["exported-symbol"], 1);
    assert_eq!(first.anchors_discovered, Some(1));
    assert!(!first.anchor_limit_reached);
    assert!(
        first.items[0].evidence.rendered.contains("## Subject"),
        "a card pack leads with its subject, not a candidate set"
    );
    assert!(
        first.items[0]
            .evidence
            .rendered
            .contains("## Direct structural context"),
    );
    assert!(first.items[0].context_edges > 0, "helper call is depth-1");

    // A published workflow participant becomes a card subject even when it
    // is not exported.
    let helper = "sym:index.ts#::helper@1";
    let snapshot = crate::structural::current_snapshot(&conn)?;
    conn.execute(
        "INSERT INTO semantic_artifacts(
           artifact_type,canonical_name,body_json,model,prompt_version,confidence,
           source_snapshot,created_at
         ) VALUES('workflow','flow','{\"participants\":[{\"role\":\"helper\"}]}',
                  'agent-reported','annotate/v2','likely',?1,'2026-08-10T00:00:00Z')",
        params![snapshot],
    )?;
    let artifact_id = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO semantic_supports(
           artifact_id,claim_path,anchor_key,evidence_file,evidence_start_line,
           evidence_end_line,source_hash,context_hash,confidence
         ) VALUES(?1,'/participants/0/role',?2,'index.ts',2,2,'h','c','likely')",
        params![artifact_id, helper],
    )?;
    let widened = super::cards(repo.path(), &conn, &[])?;
    assert_eq!(widened.items.len(), 2);
    let participant = widened
        .items
        .iter()
        .find(|item| item.anchor == helper)
        .expect("participant subject");
    assert_eq!(participant.sources, ["workflow-participant"]);
    assert_eq!(
        widened.items[0].anchor, helper,
        "workflow participants outrank plain exports in a capped plan"
    );
    Ok(())
}

#[test]
fn automatic_card_selection_reports_its_cap() -> Result<()> {
    // Every subject's pack renders its whole declaring file, so the
    // fixture spreads symbols over files: one huge file would make this
    // test quadratic in the cap.
    const PER_FILE: usize = 16;
    let files = (super::CARD_LIMIT + 4).div_ceil(PER_FILE);
    let discovered = files * PER_FILE;
    let repo = tempfile::tempdir()?;
    for file in 0..files {
        let mut source = String::new();
        for index in 0..PER_FILE {
            source.push_str(&format!(
                "export function symbol{file}_{index}() {{ return {index}; }}\n"
            ));
        }
        std::fs::write(repo.path().join(format!("module{file}.ts")), source)?;
    }
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let plan = super::cards(repo.path(), &conn, &[])?;
    assert_eq!(plan.anchors_discovered, Some(discovered));
    assert_eq!(plan.anchor_limit, Some(super::CARD_LIMIT));
    assert!(plan.anchor_limit_reached);
    assert_eq!(plan.items.len(), super::CARD_LIMIT);
    assert_eq!(
        plan.discovered_sources["exported-symbol"], discovered,
        "a capped plan still reports everything discovery found"
    );
    assert_eq!(plan.sources["exported-symbol"], super::CARD_LIMIT);
    assert_eq!(
        plan.scope_coverage
            .values()
            .map(|coverage| coverage.discovered)
            .sum::<usize>(),
        discovered
    );
    assert_eq!(
        plan.scope_coverage
            .values()
            .map(|coverage| coverage.omitted)
            .sum::<usize>(),
        discovered - super::CARD_LIMIT
    );
    Ok(())
}

#[test]
fn card_scope_allocation_covers_scopes_before_repeating_one() {
    let subjects = vec![
        (
            "a1".into(),
            vec!["exported-symbol".into()],
            "scope-a".into(),
        ),
        (
            "a2".into(),
            vec!["exported-symbol".into()],
            "scope-a".into(),
        ),
        (
            "a3".into(),
            vec!["exported-symbol".into()],
            "scope-a".into(),
        ),
        (
            "b1".into(),
            vec!["exported-symbol".into()],
            "scope-b".into(),
        ),
        (
            "b2".into(),
            vec!["exported-symbol".into()],
            "scope-b".into(),
        ),
    ];
    let (selected, coverage) = super::stratify_card_subjects(subjects, 3);
    assert_eq!(
        selected
            .iter()
            .map(|(anchor, _, _)| anchor.as_str())
            .collect::<Vec<_>>(),
        ["a1", "b1", "a2"]
    );
    assert_eq!(coverage["scope-a"].selected, 2);
    assert_eq!(coverage["scope-a"].omitted, 1);
    assert_eq!(coverage["scope-b"].selected, 1);
    assert_eq!(coverage["scope-b"].omitted, 1);
}

#[test]
fn targeted_card_files_and_recon_subjects_never_widen() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::create_dir_all(repo.path().join("apps/target"))?;
    std::fs::write(
        repo.path().join("apps/target/index.ts"),
        "export function targetEntry() { return targetHelper(); }\n\
         function targetHelper() { return 1; }\n",
    )?;
    std::fs::write(
        repo.path().join("unrelated.ts"),
        "export function unrelated() { return 2; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let file_plan = super::cards_with_selectors(
        repo.path(),
        &conn,
        &super::CardSelectors {
            files: vec!["apps/target/index.ts".into()],
            ..super::CardSelectors::default()
        },
    )?;
    assert_eq!(file_plan.mode, "targeted");
    assert_eq!(file_plan.items.len(), 2);
    assert!(
        file_plan
            .items
            .iter()
            .all(|item| item.file == "apps/target/index.ts"
                && item.selection_scope == "file:apps/target/index.ts")
    );

    conn.execute(
        "INSERT INTO scout_runs(
           scout_kind,status,gateway_protocol,provider,model,billing_path,
           prompt_version,source_snapshot,input_fingerprint,request_hash,
           config_json,started_at,completed_at
         ) VALUES('repository','completed',1,'test','test','custom','test/v1',
                  'snapshot','target-scope','target-scope','{}','now','now')",
        [],
    )?;
    let run_id = conn.last_insert_rowid();
    let selector = crate::recon::SubjectSelector::RepositoryArea {
        scope: "apps/target".into(),
        direct_only: false,
    };
    let state = crate::recon::build_scope_state(
        repo.path(),
        &conn,
        "area:repository:apps/target".into(),
        selector.clone(),
    )?;
    crate::recon::persist_classification(
        &conn,
        &crate::recon::NewClassification {
            run_id,
            subject_key: "area:repository:apps/target",
            subject_kind: "area",
            selector: &selector,
            parent_subject_key: None,
            depth: 1,
            role: "runtime",
            confidence: "likely",
            explanation: "target runtime area",
            citations_json: "[\"E001\"]",
            cited_evidence_json: "[]",
            evidence_fingerprint: &state.evidence_fingerprint,
            classification_fingerprint: "target-classification",
            source_snapshot: "snapshot",
        },
    )?;
    crate::recon::reconcile_file_policy(repo.path(), &conn)?;

    let subject_plan = super::cards_with_selectors(
        repo.path(),
        &conn,
        &super::CardSelectors {
            reconnaissance_subjects: vec!["area:repository:apps/target".into()],
            ..super::CardSelectors::default()
        },
    )?;
    assert_eq!(subject_plan.items.len(), 2);
    assert!(subject_plan.items.iter().all(|item| {
        item.file == "apps/target/index.ts" && item.selection_scope == "area:repository:apps/target"
    }));
    assert!(
        subject_plan
            .items
            .iter()
            .all(|item| !item.anchor.contains("unrelated"))
    );
    Ok(())
}

#[test]
fn explicit_card_anchors_resolve_uniquely_or_fail_loudly() -> Result<()> {
    let repo = tempfile::tempdir()?;
    std::fs::write(
        repo.path().join("index.ts"),
        "export function only() { return 1; }\n",
    )?;
    std::fs::write(
        repo.path().join("other.ts"),
        "export function shared() { return 1; }\n",
    )?;
    std::fs::write(
        repo.path().join("second.ts"),
        "export function shared() { return 2; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let plan = super::cards(repo.path(), &conn, &["index.ts:only".into()])?;
    assert_eq!(plan.mode, "explicit");
    assert_eq!(plan.items.len(), 1);
    assert_eq!(plan.items[0].sources, ["agent-supplied"]);
    assert_eq!(plan.items[0].file, "index.ts");

    // Repeating one anchor still spends one run.
    let repeated = super::cards(
        repo.path(),
        &conn,
        &["index.ts:only".into(), "index.ts:only".into()],
    )?;
    assert_eq!(repeated.items.len(), 1);

    let error = super::cards(repo.path(), &conn, &["shared".into()]).expect_err("ambiguous anchor");
    assert!(error.to_string().contains("ambiguous"));
    let error = super::cards(repo.path(), &conn, &["file:index.ts".into()])
        .expect_err("file anchors are not card subjects");
    assert!(error.to_string().contains("not a file-backed symbol"));
    Ok(())
}
