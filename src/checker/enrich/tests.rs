use std::fs;

use anyhow::Result;

use super::*;

fn planned_occurrence(
    id: i64,
    package: &str,
    file: &str,
    role: &str,
    boundary_rank: i64,
) -> Occurrence {
    Occurrence {
        id,
        file_id: id,
        file: file.into(),
        hash: "hash".into(),
        call_start: id,
        call_end: id + 10,
        receiver_start: id,
        receiver_end: id + 3,
        property_start: id + 4,
        property_end: id + 9,
        member: "run".into(),
        role: role.into(),
        package: package.into(),
        boundary_rank,
        deterministically_resolved: false,
        builtin_receiver: false,
        runtime_namesake: true,
    }
}

fn options() -> EnrichOptions<'static> {
    EnrichOptions {
        database: None,
        sidecar: None,
        node: "node",
        timeout: Duration::from_secs(1),
        files: Vec::new(),
        packages: Vec::new(),
        members: Vec::new(),
        roles: Vec::new(),
        max_occurrences: None,
        include_all: false,
        dry_run: false,
        carry_forward: false,
        force_full: false,
        dirty_files: Vec::new(),
    }
}

fn test_project_fingerprints(
    projects: &BTreeMap<String, Vec<Occurrence>>,
) -> BTreeMap<String, String> {
    projects
        .keys()
        .map(|project| (project.clone(), format!("planning:{project}")))
        .collect()
}

#[test]
fn spread_order_covers_packages_and_files_before_repeating_a_prefix() {
    let occurrences = vec![
        planned_occurrence(1, "alpha", "alpha/a.ts", "production", 0),
        planned_occurrence(2, "alpha", "alpha/a.ts", "production", 0),
        planned_occurrence(3, "alpha", "alpha/b.ts", "production", 0),
        planned_occurrence(4, "beta", "beta/a.ts", "production", 0),
        planned_occurrence(5, "beta", "beta/a.ts", "production", 0),
        planned_occurrence(6, "gamma", "gamma/a.ts", "production", 1),
    ];
    let ordered = spread_occurrences(occurrences);
    assert_eq!(
        ordered.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![1, 4, 3, 5, 2, 6]
    );
}

#[test]
fn default_eligibility_excludes_nonproduction_and_exact_resolver_answers() {
    let mut resolved = planned_occurrence(2, "alpha", "alpha/b.ts", "production", 0);
    resolved.deterministically_resolved = true;
    let candidates = vec![
        planned_occurrence(1, "alpha", "alpha/a.ts", "production", 0),
        resolved,
        planned_occurrence(3, "alpha", "alpha/test.ts", "test", 0),
    ];
    assert_eq!(
        select_eligible(candidates.clone(), &options())
            .0
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        vec![1]
    );
    let mut all = options();
    all.include_all = true;
    assert_eq!(select_eligible(candidates, &all).0.len(), 3);
}

#[test]
fn builtin_receivers_are_deprioritized_while_foreign_namesakes_are_excluded() {
    let mut builtin = planned_occurrence(1, "alpha", "alpha/a.ts", "production", 0);
    builtin.builtin_receiver = true;
    let mut foreign = planned_occurrence(2, "alpha", "alpha/b.ts", "production", 0);
    foreign.runtime_namesake = false;
    let keep = planned_occurrence(3, "alpha", "alpha/c.ts", "production", 0);
    let candidates = vec![builtin, foreign, keep];

    let (eligible, skips) = select_eligible(candidates.clone(), &options());
    assert_eq!(
        eligible.iter().map(|item| item.id).collect::<Vec<_>>(),
        vec![1, 3]
    );
    assert_eq!(skips.builtin_receiver, 1);
    assert_eq!(skips.foreign_namesake, 1);
    assert_eq!(
        spread_occurrences(eligible)
            .iter()
            .map(|item| item.id)
            .collect::<Vec<_>>(),
        vec![3, 1]
    );

    let mut all = options();
    all.include_all = true;
    let (eligible, skips) = select_eligible(candidates, &all);
    assert_eq!(eligible.len(), 3);
    assert_eq!(skips.builtin_receiver, 1);
    assert_eq!(skips.foreign_namesake, 0);
}

#[test]
fn tooling_ownership_is_excluded_with_a_non_tooling_owner_and_retained_as_fallback() -> Result<()> {
    let selected = vec![
        planned_occurrence(1, "root", "shared.ts", "production", 0),
        planned_occurrence(2, "root", "lint-only.ts", "production", 0),
    ];
    let ownership = vec![
        FileOwnership {
            file: "shared.ts".into(),
            project_ids: vec!["tsconfig.json".into()],
            excluded_project_ids: vec!["tsconfig.eslint.json".into()],
            tooling_fallback: false,
        },
        FileOwnership {
            file: "lint-only.ts".into(),
            project_ids: vec!["tsconfig.eslint.json".into()],
            excluded_project_ids: Vec::new(),
            tooling_fallback: true,
        },
    ];
    let projects = vec![
        ProjectSummary {
            project_id: "tsconfig.eslint.json".into(),
            file_count: 2,
            purpose: "tooling".into(),
            purpose_reasons: vec!["tooling-filename".into()],
            membership_fingerprint: String::new(),
            config_fingerprint: String::new(),
        },
        ProjectSummary {
            project_id: "tsconfig.json".into(),
            file_count: 1,
            purpose: "general".into(),
            purpose_reasons: Vec::new(),
            membership_fingerprint: String::new(),
            config_fingerprint: String::new(),
        },
    ];

    let planning = build_project_plan(&selected, &ownership, &projects, false)?;
    assert_eq!(planning.occurrences_avoided_by_tooling_filter, 1);
    assert_eq!(planning.occurrences_using_tooling_fallback, 1);
    assert_eq!(planning.projects["tsconfig.json"][0].file, "shared.ts");
    assert_eq!(
        planning.projects["tsconfig.eslint.json"][0].file,
        "lint-only.ts"
    );
    let tooling = planning
        .decisions
        .iter()
        .find(|decision| decision.project_id == "tsconfig.eslint.json")
        .expect("tooling decision");
    assert_eq!(tooling.selected_occurrences, 1);
    assert_eq!(tooling.excluded_occurrences, 1);
    assert_eq!(tooling.fallback_occurrences, 1);
    Ok(())
}

#[test]
fn inferred_projects_are_gated_before_caps_and_all_is_the_escape_hatch() -> Result<()> {
    let configured = planned_occurrence(1, "alpha", "src/app.ts", "production", 0);
    let inferred_a = planned_occurrence(2, "alpha", "scripts/tool.mjs", "production", 0);
    let inferred_b = planned_occurrence(3, "alpha", "scripts/tool.mjs", "production", 0);
    let occurrences = vec![inferred_a, inferred_b, configured.clone()];
    let ownership = vec![
        FileOwnership {
            file: "scripts/tool.mjs".into(),
            project_ids: vec!["inferred:.#node-esm".into()],
            excluded_project_ids: Vec::new(),
            tooling_fallback: false,
        },
        FileOwnership {
            file: "src/app.ts".into(),
            project_ids: vec!["tsconfig.json".into()],
            excluded_project_ids: Vec::new(),
            tooling_fallback: false,
        },
    ];
    let projects = vec![
        ProjectSummary {
            project_id: "inferred:.#node-esm".into(),
            file_count: 1,
            purpose: "inferred".into(),
            purpose_reasons: vec!["no-configured-owner".into()],
            membership_fingerprint: "inferred-members".into(),
            config_fingerprint: "package-type".into(),
        },
        ProjectSummary {
            project_id: "tsconfig.json".into(),
            file_count: 1,
            purpose: "general".into(),
            purpose_reasons: Vec::new(),
            membership_fingerprint: String::new(),
            config_fingerprint: String::new(),
        },
    ];

    let (default, coverage) = gate_inferred_projects(occurrences.clone(), &ownership, false)?;
    assert_eq!(default.iter().map(|item| item.id).collect::<Vec<_>>(), [1]);
    assert_eq!(
        coverage,
        InferredProjectCoverage {
            files_without_configured_project: 1,
            occurrences_without_configured_project: 2,
            occurrences_skipped_inferred_project: 2,
        }
    );
    // The operator cap applies after the inferred gate, so an inferred
    // lexical prefix cannot consume the only selected slot.
    assert_eq!(
        spread_occurrences(default)
            .into_iter()
            .take(1)
            .next()
            .unwrap()
            .id,
        1
    );
    let default_plan = build_project_plan(
        std::slice::from_ref(&configured),
        &ownership,
        &projects,
        false,
    )?;
    assert_eq!(
        default_plan.projects.keys().collect::<Vec<_>>(),
        ["tsconfig.json"]
    );

    let (all, coverage) = gate_inferred_projects(occurrences, &ownership, true)?;
    assert_eq!(all.len(), 3);
    assert_eq!(coverage.files_without_configured_project, 1);
    assert_eq!(coverage.occurrences_without_configured_project, 2);
    assert_eq!(coverage.occurrences_skipped_inferred_project, 0);
    let all_plan = build_project_plan(&all, &ownership, &projects, true)?;
    assert_eq!(
        all_plan.project_roots["inferred:.#node-esm"],
        ["scripts/tool.mjs"]
    );
    assert_eq!(
        projects_in_execution_order(
            &all_plan.projects,
            &BTreeSet::new(),
            &all_plan.first_selected_rank,
        )
        .into_iter()
        .map(|(project, _)| project.as_str())
        .collect::<Vec<_>>(),
        ["tsconfig.json", "inferred:.#node-esm"]
    );
    Ok(())
}

#[test]
fn repository_project_policy_overrides_heuristics_but_preserves_sole_owner_fallback() -> Result<()>
{
    let directory = tempfile::tempdir()?;
    let conn = crate::store::open(directory.path())?;
    for (project_id, role, suffix) in [
        ("tsconfig.runtime.json", "runtime", "runtime"),
        ("tsconfig.tool.json", "tooling", "tooling"),
    ] {
        conn.execute(
            "INSERT INTO scout_runs(
               scout_kind,status,gateway_protocol,provider,model,billing_path,
               prompt_version,source_snapshot,input_fingerprint,request_hash,
               config_json,started_at,completed_at
             ) VALUES('repository','completed',1,'test','test','custom','test',
                      'snapshot',?1,?1,'{}','now','now')",
            [format!("project-policy-{suffix}")],
        )?;
        let selector = serde_json::to_string(&crate::recon::SubjectSelector::Project {
            config: project_id.into(),
            membership_fingerprint: format!("members-{suffix}"),
            config_fingerprint: format!("config-{suffix}"),
        })?;
        conn.execute(
            "INSERT INTO repository_classifications(
               run_id,subject_key,subject_kind,selector_json,depth,role,confidence,
               explanation,citations_json,evidence_fingerprint,
               classification_fingerprint,source_snapshot,created_at
             ) VALUES(?1,?2,'project',?3,0,?4,'likely','test','[\"E001\"]',
                      ?2,?2,'snapshot','now')",
            rusqlite::params![
                conn.last_insert_rowid(),
                format!("project:{project_id}"),
                selector,
                role,
            ],
        )?;
    }
    let mut projects = vec![
        ProjectSummary {
            project_id: "tsconfig.runtime.json".into(),
            file_count: 1,
            purpose: "tooling".into(),
            purpose_reasons: vec!["filename".into()],
            membership_fingerprint: "members-runtime".into(),
            config_fingerprint: "config-runtime".into(),
        },
        ProjectSummary {
            project_id: "tsconfig.tool.json".into(),
            file_count: 2,
            purpose: "general".into(),
            purpose_reasons: Vec::new(),
            membership_fingerprint: "members-tooling".into(),
            config_fingerprint: "config-tooling".into(),
        },
    ];
    let mut ownership = vec![
        FileOwnership {
            file: "shared.ts".into(),
            project_ids: vec!["tsconfig.tool.json".into()],
            excluded_project_ids: vec!["tsconfig.runtime.json".into()],
            tooling_fallback: false,
        },
        FileOwnership {
            file: "tool-only.ts".into(),
            project_ids: vec!["tsconfig.tool.json".into()],
            excluded_project_ids: Vec::new(),
            tooling_fallback: false,
        },
    ];

    apply_repository_project_policy(&conn, &mut ownership, &mut projects)?;

    assert_eq!(ownership[0].project_ids, ["tsconfig.runtime.json"]);
    assert_eq!(ownership[0].excluded_project_ids, ["tsconfig.tool.json"]);
    assert!(!ownership[0].tooling_fallback);
    assert_eq!(ownership[1].project_ids, ["tsconfig.tool.json"]);
    assert!(ownership[1].excluded_project_ids.is_empty());
    assert!(ownership[1].tooling_fallback);
    assert_eq!(projects[0].purpose, "runtime");
    assert_eq!(projects[1].purpose, "tooling");
    assert!(projects.iter().all(|project| {
        project
            .purpose_reasons
            .iter()
            .any(|reason| reason.starts_with("repository-recon:"))
    }));

    conn.execute(
        "INSERT INTO scout_runs(
           scout_kind,status,gateway_protocol,provider,model,billing_path,
           prompt_version,source_snapshot,input_fingerprint,request_hash,
           config_json,started_at,completed_at
         ) VALUES('repository','completed',1,'test','test','custom','test',
                  'snapshot','project-policy-neutral','neutral','{}','now','now')",
        [],
    )?;
    let selector = serde_json::to_string(&crate::recon::SubjectSelector::Project {
        config: "tsconfig.runtime.json".into(),
        membership_fingerprint: "members-runtime".into(),
        config_fingerprint: "config-runtime".into(),
    })?;
    conn.execute(
        "INSERT INTO repository_classifications(
           run_id,subject_key,subject_kind,selector_json,depth,role,confidence,
           explanation,citations_json,evidence_fingerprint,
           classification_fingerprint,source_snapshot,created_at
         ) VALUES(?1,'project:tsconfig.runtime.json','project',?2,0,'unknown',
                  'possible','insufficient','[\"E001\"]','neutral','neutral',
                  'snapshot','now')",
        rusqlite::params![conn.last_insert_rowid(), selector],
    )?;
    let mut neutral_projects = vec![ProjectSummary {
        project_id: "tsconfig.runtime.json".into(),
        file_count: 1,
        purpose: "tooling".into(),
        purpose_reasons: vec!["filename".into()],
        membership_fingerprint: "members-runtime".into(),
        config_fingerprint: "config-runtime".into(),
    }];
    let mut neutral_ownership = vec![FileOwnership {
        file: "only.ts".into(),
        project_ids: Vec::new(),
        excluded_project_ids: vec!["tsconfig.runtime.json".into()],
        tooling_fallback: false,
    }];
    apply_repository_project_policy(&conn, &mut neutral_ownership, &mut neutral_projects)?;
    assert_eq!(neutral_projects[0].purpose, "tooling");
    assert_eq!(neutral_projects[0].purpose_reasons, ["filename"]);
    Ok(())
}

#[test]
fn checker_cancellation_is_an_operation_interrupt_not_a_failed_project() {
    let canceled = anyhow::Error::new(super::super::process::CheckerError::Canceled(
        "requested".into(),
    ));
    assert!(canceled_checker_error(&canceled));

    let failed = anyhow::Error::new(super::super::process::CheckerError::Remote {
        code: "checker_crash".into(),
        message: "worker failed".into(),
    });
    assert!(!canceled_checker_error(&failed));
}

#[test]
fn partial_failure_retry_policy_separates_project_state_from_transport_failure() {
    let mismatch = anyhow::Error::new(super::super::process::CheckerError::Remote {
        code: "project_mismatch".into(),
        message: "not an effective member".into(),
    });
    assert!(!project_failure_is_retryable(&mismatch));

    let crash = anyhow::Error::new(super::super::process::CheckerError::Remote {
        code: "checker_crash".into(),
        message: "worker failed".into(),
    });
    assert!(!project_failure_is_retryable(&crash));

    let worker_exit = anyhow::Error::new(super::super::process::CheckerError::Remote {
        code: "checker_exit".into(),
        message: "worker exited".into(),
    });
    assert!(!project_failure_is_retryable(&worker_exit));

    let process_exit = anyhow::Error::new(super::super::process::CheckerError::ChildExited(
        "checker aborted".into(),
    ));
    assert!(!project_failure_is_retryable(&process_exit));

    let spawn = anyhow::Error::new(super::super::process::CheckerError::Spawn(
        "temporarily unavailable".into(),
    ));
    assert!(project_failure_is_retryable(&spawn));

    let transport = anyhow::Error::new(super::super::process::CheckerError::Io(
        "temporary pipe failure".into(),
    ));
    assert!(project_failure_is_retryable(&transport));

    let exhausted = anyhow::Error::new(super::super::process::CheckerError::Remote {
        code: "EMFILE".into(),
        message: "too many open files".into(),
    });
    assert!(project_failure_is_retryable(&exhausted));

    let timeout = anyhow::Error::new(super::super::process::CheckerError::Timeout(
        Duration::from_secs(1),
    ));
    assert!(project_failure_is_retryable(&timeout));

    let protocol = anyhow::Error::new(super::super::process::CheckerError::Protocol(
        "invalid response".into(),
    ));
    assert!(!project_failure_is_retryable(&protocol));

    let unknown = anyhow::anyhow!("deterministic local failure");
    assert!(!project_failure_is_retryable(&unknown));

    let terminal_partial = anyhow::Error::new(PartialEnrichmentError {
        batch_id: 1,
        facts_published: 10,
        failures: vec![ProjectFailure {
            project_id: "tsconfig.json".into(),
            retryable: false,
        }],
    });
    assert!(is_terminal_partial_failure(&terminal_partial));

    let retryable_partial = anyhow::Error::new(PartialEnrichmentError {
        batch_id: 2,
        facts_published: 10,
        failures: vec![ProjectFailure {
            project_id: "tsconfig.json".into(),
            retryable: true,
        }],
    });
    assert!(!is_terminal_partial_failure(&retryable_partial));
}

#[test]
fn builtin_receiver_and_runtime_namesake_flags_come_from_the_index() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join("src"))?;
    fs::create_dir_all(repo.path().join("tests"))?;
    fs::write(
        repo.path().join("src/app.ts"),
        "import path from \"path\";\n\
         import { PathShim } from \"./path-shim\";\n\
         import { helper } from \"./helper\";\n\
         export class Service { run(): void {} }\n\
         export class Loader { ensureUserland(): void {} }\n\
         export function work(service: Service): void {\n\
           console.log(\"x\");\n\
           path.join(\"a\", \"b\");\n\
           service.run();\n\
           helper.probe();\n\
         }\n\
         export function ambient(): void { console.send(); }\n\
         export function shadowedImport(path: PathShim): void { path.join(); }\n\
         export function route(module: Loader): void {\n\
           module.ensureUserland();\n\
         }\n\
         export function hot(): void {\n\
           module.reload();\n\
         }\n",
    )?;
    fs::write(
        repo.path().join("src/globals.ts"),
        "class RepoConsole { send(): void {} }\n\
         declare const console: RepoConsole;\n",
    )?;
    fs::write(
        repo.path().join("src/path-shim.ts"),
        "export class PathShim { join(): void {} }\n\
         export default new PathShim();\n",
    )?;
    fs::write(
        repo.path().join("tsconfig.json"),
        "{\"compilerOptions\":{\"baseUrl\":\".\",\"paths\":{\"path\":[\"./src/path-shim.ts\"]}},\"include\":[\"src/**/*.ts\"]}",
    )?;
    fs::write(
        repo.path().join("src/shadow.ts"),
        "export const console = { log(_message: string): void {} };\n\
         export function shadowed(): void { console.log(\"y\"); }\n",
    )?;
    fs::write(
        repo.path().join("src/helper.ts"),
        "export const helper = { probe(): void {} };\n",
    )?;
    // Test-role namesakes satisfy the raw same-name floor for every
    // member below without making any of them runtime-anchorable.
    fs::write(
        repo.path().join("tests/namesakes.test.ts"),
        "export function log(): void {}\n\
         export function join(): void {}\n\
         export function probe(): void {}\n\
         export function reload(): void {}\n",
    )?;
    let conn = crate::store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;

    let calls = load_occurrences(&conn)?;
    let flags = |file: &str, member: &str| {
        let occurrence = calls
            .iter()
            .find(|occurrence| occurrence.file == file && occurrence.member == member)
            .unwrap_or_else(|| panic!("occurrence {file}:{member} discovered"));
        (occurrence.builtin_receiver, occurrence.runtime_namesake)
    };
    // Bare global receiver, unbound in the scope tree.
    assert_eq!(flags("src/app.ts", "log"), (true, false));
    // Request spelling alone labels a paths-mapped import and a lexically
    // shadowed import as builtin-looking. This is advisory only: both have
    // an indexed runtime namesake and must remain eligible.
    let joins = calls
        .iter()
        .filter(|occurrence| occurrence.file == "src/app.ts" && occurrence.member == "join")
        .collect::<Vec<_>>();
    assert_eq!(joins.len(), 2);
    assert!(joins.iter().all(|occurrence| occurrence.builtin_receiver));
    assert!(joins.iter().all(|occurrence| occurrence.runtime_namesake));
    // A project-wide ambient global is also unbound in the per-file Oxc
    // scope even though TypeScript can resolve its repository declaration.
    assert_eq!(flags("src/app.ts", "send"), (true, true));
    // Ordinary local receiver with a production-class namesake.
    assert_eq!(flags("src/app.ts", "run"), (false, true));
    // Namesake exists only in a test file.
    assert_eq!(flags("src/app.ts", "probe"), (false, false));
    // `module` as a function parameter is a binding, not the CommonJS
    // global: the gate must not fire (the Next.js app-route.ts case).
    assert_eq!(flags("src/app.ts", "ensureUserland"), (false, true));
    // `module` with no binding anywhere is the real global.
    assert_eq!(flags("src/app.ts", "reload"), (true, false));
    // A same-file symbol named after the global keeps the occurrence.
    assert!(!flags("src/shadow.ts", "log").0);

    let (eligible, skips) = select_eligible(calls, &options());
    let mut eligible_members = eligible
        .iter()
        .map(|occurrence| occurrence.member.as_str())
        .collect::<Vec<_>>();
    eligible_members.sort_unstable();
    assert_eq!(
        eligible_members,
        vec!["ensureUserland", "join", "join", "run", "send"]
    );
    assert_eq!(skips.builtin_receiver, 3);
    assert_eq!(skips.foreign_namesake, 4);
    Ok(())
}

#[test]
fn empty_filtered_plan_is_a_successful_noop_without_launching_the_checker() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::create_dir_all(repo.path().join("src"))?;
    fs::create_dir_all(repo.path().join("tests"))?;
    fs::write(
        repo.path().join("src/app.ts"),
        "export function run(): void { console.log('x'); }\n",
    )?;
    fs::write(
        repo.path().join("tests/log.test.ts"),
        "export function log(): void {}\n",
    )?;
    let conn = crate::store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    drop(conn);

    let report = enrich(repo.path(), &options())?;
    assert_eq!(report.occurrences_discovered, 1);
    assert_eq!(report.occurrences_eligible, 0);
    assert_eq!(report.occurrences_selected, 0);
    assert_eq!(report.occurrences_skipped_foreign_namesake, 1);
    assert_eq!(report.checker_source, "not-invoked");
    assert_eq!(report.batch_id, 0);
    Ok(())
}

#[test]
fn non_repo_declaration_contexts_skip_mapping_but_stay_unmapped() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("main.ts"),
        "export class CardTable { insert(): void {} }\n",
    )?;
    let conn = crate::store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let calls = load_occurrences(&conn)?;
    assert!(calls.is_empty(), "fixture has no member calls");
    let occurrence = planned_occurrence(1, "(root)", "main.ts", "production", 0);

    let answer = ProjectAnswer {
        project_id: "tsconfig.json".into(),
        status: "resolved".into(),
        receiver_type: None,
        declarations: vec![
            DeclarationSite {
                file: None,
                outside_root: true,
                start: 0,
                end: 4,
                source_hash: "lib".into(),
                context: Some("lib".into()),
            },
            DeclarationSite {
                file: Some("node_modules/@types/thing/index.d.ts".into()),
                outside_root: false,
                start: 0,
                end: 4,
                source_hash: "types".into(),
                context: Some("types".into()),
            },
        ],
        checker_input_fingerprint: "inputs".into(),
    };
    let outcome = map_occurrence(&conn, &occurrence, std::slice::from_ref(&answer))?;
    assert!(outcome.facts.is_empty());
    assert_eq!(outcome.unmapped_declarations, 2);
    assert_eq!(
        outcome.unmapped_declaration_contexts,
        BTreeMap::from([("lib".to_string(), 1), ("types".to_string(), 1)])
    );

    // An old sidecar sends no context; an outside declaration is still
    // attributed rather than mislabelled as a repository anchoring gap.
    let legacy = ProjectAnswer {
        project_id: "tsconfig.json".into(),
        status: "resolved".into(),
        receiver_type: None,
        declarations: vec![DeclarationSite {
            file: None,
            outside_root: true,
            start: 0,
            end: 4,
            source_hash: "lib".into(),
            context: None,
        }],
        checker_input_fingerprint: "inputs".into(),
    };
    let outcome = map_occurrence(&conn, &occurrence, std::slice::from_ref(&legacy))?;
    assert!(outcome.facts.is_empty());
    assert_eq!(outcome.unmapped_declarations, 1);
    assert_eq!(
        outcome.unmapped_declaration_contexts,
        BTreeMap::from([("outside".to_string(), 1)])
    );
    Ok(())
}

#[test]
fn namespace_member_resolved_by_the_structural_graph_is_not_requeried() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(
        repo.path().join("library.ts"),
        "export function run(): void {}\n",
    )?;
    fs::write(
        repo.path().join("main.ts"),
        "import * as library from './library';\nlibrary.run();\n",
    )?;
    let conn = crate::store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;

    let calls = load_occurrences(&conn)?;
    let call = calls
        .iter()
        .find(|occurrence| occurrence.member == "run")
        .expect("namespace member call");
    assert!(call.deterministically_resolved);
    let call_id = call.id;
    assert!(select_eligible(calls.clone(), &options()).0.is_empty());

    let mut all = options();
    all.include_all = true;
    assert_eq!(select_eligible(calls, &all).0.len(), 1);
    let bound_edges: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges
         WHERE confidence IN ('certain', 'likely')
           AND CAST(json_extract(detail_json, '$.memberCallId') AS INTEGER)=?1",
        [call_id],
        |row| row.get(0),
    )?;
    assert_eq!(bound_edges, 1);
    Ok(())
}

#[test]
fn manual_default_keeps_all_one_hundred_fifty_thousand_eligible_occurrences() {
    let occurrences = (0..150_000)
        .map(|id| {
            planned_occurrence(
                id,
                &format!("package-{}", id % 64),
                &format!("package-{}/file-{}.ts", id % 64, id % 1024),
                "production",
                id % 2,
            )
        })
        .collect::<Vec<_>>();
    let (eligible, _) = select_eligible(occurrences, &options());
    assert_eq!(eligible.len(), 150_000);
    assert_eq!(spread_occurrences(eligible).len(), 150_000);
}

#[test]
fn staged_batches_survive_connection_reopen_for_resume() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = "export class CardTable { insert(): void {} }\n\
                  declare const card: CardTable; card.insert();\n";
    let (conn, _) = indexed(repo.path(), source)?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let occurrence = occurrence(&conn)?;
    let identity = TypeScriptIdentity {
        version: "5.9.3".into(),
        source: "bundled".into(),
    };
    let projects = BTreeMap::from([("tsconfig.json".into(), vec![occurrence.clone()])]);
    let fingerprints = test_project_fingerprints(&projects);
    let batch_id = open_staging_batch(
        &conn,
        &StagingPlan {
            snapshot: &snapshot,
            plan_fingerprint: "plan",
            checker: &identity,
            protocol: 2,
            selected_occurrences: 1,
            projects: &projects,
            project_fingerprints: &fingerprints,
            force_new: false,
        },
    )?;
    let answer = ProjectAnswer {
        project_id: "tsconfig.json".into(),
        status: "unknown".into(),
        receiver_type: None,
        declarations: Vec::new(),
        checker_input_fingerprint: "inputs".into(),
    };
    let outcome = map_occurrence(&conn, &occurrence, &[answer])?;
    stage_batch(
        &conn,
        batch_id,
        "tsconfig.json",
        std::slice::from_ref(&occurrence),
        &outcome.facts,
        &outcome.projects,
    )?;
    drop(conn);

    let reopened = crate::store::open(repo.path())?;
    assert_eq!(
        completed_occurrences(&reopened, batch_id, "tsconfig.json")?,
        BTreeSet::from([occurrence.id])
    );
    let active: i64 = reopened.query_row(
        "SELECT active FROM checker_enrichment_batches WHERE id=?1",
        [batch_id],
        |row| row.get(0),
    )?;
    assert_eq!(active, 0, "staged progress must remain non-public");
    Ok(())
}

#[test]
fn failed_owner_activates_only_completed_projects_as_possible_and_remains_resumable() -> Result<()>
{
    let repo = tempfile::tempdir()?;
    let source = "export class CardTable { insert(): void {} }\n\
                  declare const card: CardTable; card.insert();\n";
    let (conn, hash) = indexed(repo.path(), source)?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let occurrence = occurrence(&conn)?;
    let identity = TypeScriptIdentity {
        version: "5.9.3".into(),
        source: "bundled".into(),
    };
    let projects = BTreeMap::from([
        ("tsconfig.good.json".into(), vec![occurrence.clone()]),
        ("tsconfig.failed.json".into(), vec![occurrence.clone()]),
    ]);
    let fingerprints = test_project_fingerprints(&projects);
    let batch_id = open_staging_batch(
        &conn,
        &StagingPlan {
            snapshot: &snapshot,
            plan_fingerprint: "partial",
            checker: &identity,
            protocol: 2,
            selected_occurrences: 1,
            projects: &projects,
            project_fingerprints: &fingerprints,
            force_new: false,
        },
    )?;

    let declaration = declaration_at(source, "insert(): void {}", "insert", &hash);
    let good = project_answer("tsconfig.good.json", vec![declaration]);
    let outcome = map_occurrence(&conn, &occurrence, &[good])?;
    stage_batch(
        &conn,
        batch_id,
        "tsconfig.good.json",
        std::slice::from_ref(&occurrence),
        &outcome.facts,
        &outcome.projects,
    )?;
    complete_project(
        repo.path(),
        &conn,
        batch_id,
        "tsconfig.good.json",
        "tsconfig.good.json-inputs",
        &[super::super::protocol::CheckerInputFile {
            path: repo.path().join("main.ts").to_string_lossy().into_owned(),
            source_hash: hash,
        }],
        1,
        1,
    )?;
    mark_project_failed(
        &conn,
        batch_id,
        "tsconfig.failed.json",
        std::slice::from_ref(&occurrence),
        "synthetic failure",
    )?;

    assert_eq!(
        activate_staging_batch(repo.path(), &conn, batch_id, &snapshot, true)?,
        1
    );
    crate::structural::rebuild_projection(&conn, &snapshot)?;
    let (confidence, detail): (String, String) = conn.query_row(
        "SELECT confidence, detail_json FROM resolved_edges
         WHERE provenance='checker' AND source_ref_id=?1",
        [occurrence.id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(confidence, "possible");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&detail)?["failedProjects"],
        serde_json::json!(["tsconfig.failed.json"])
    );
    assert_eq!(
        open_staging_batch(
            &conn,
            &StagingPlan {
                snapshot: &snapshot,
                plan_fingerprint: "partial",
                checker: &identity,
                protocol: 2,
                selected_occurrences: 1,
                projects: &projects,
                project_fingerprints: &fingerprints,
                force_new: false,
            },
        )?,
        batch_id,
        "the partial active batch must remain the resume target"
    );
    assert!(completed_occurrences(&conn, batch_id, "tsconfig.failed.json")?.is_empty());
    Ok(())
}

#[test]
fn all_failed_batch_cannot_replace_a_healthy_active_batch() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = "export class CardTable { insert(): void {} }\n\
                  declare const card: CardTable; card.insert();\n";
    let (conn, hash) = indexed(repo.path(), source)?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let occurrence = occurrence(&conn)?;
    let identity = TypeScriptIdentity {
        version: "5.9.3".into(),
        source: "bundled".into(),
    };

    let healthy_projects =
        BTreeMap::from([("tsconfig.healthy.json".into(), vec![occurrence.clone()])]);
    let healthy_fingerprints = test_project_fingerprints(&healthy_projects);
    let healthy_batch = open_staging_batch(
        &conn,
        &StagingPlan {
            snapshot: &snapshot,
            plan_fingerprint: "healthy-plan",
            checker: &identity,
            protocol: 2,
            selected_occurrences: 1,
            projects: &healthy_projects,
            project_fingerprints: &healthy_fingerprints,
            force_new: false,
        },
    )?;
    let declaration = declaration_at(source, "insert(): void {}", "insert", &hash);
    let answer = project_answer("tsconfig.healthy.json", vec![declaration]);
    let outcome = map_occurrence(&conn, &occurrence, &[answer])?;
    stage_batch(
        &conn,
        healthy_batch,
        "tsconfig.healthy.json",
        std::slice::from_ref(&occurrence),
        &outcome.facts,
        &outcome.projects,
    )?;
    complete_project(
        repo.path(),
        &conn,
        healthy_batch,
        "tsconfig.healthy.json",
        "tsconfig.healthy.json-inputs",
        &[super::super::protocol::CheckerInputFile {
            path: repo.path().join("main.ts").to_string_lossy().into_owned(),
            source_hash: hash,
        }],
        1,
        1,
    )?;
    assert_eq!(
        activate_staging_batch(repo.path(), &conn, healthy_batch, &snapshot, false)?,
        1
    );
    crate::structural::rebuild_projection(&conn, &snapshot)?;

    let failed_projects =
        BTreeMap::from([("tsconfig.failed.json".into(), vec![occurrence.clone()])]);
    let failed_fingerprints = test_project_fingerprints(&failed_projects);
    let failed_batch = open_staging_batch(
        &conn,
        &StagingPlan {
            snapshot: &snapshot,
            plan_fingerprint: "failed-plan",
            checker: &identity,
            protocol: 2,
            selected_occurrences: 1,
            projects: &failed_projects,
            project_fingerprints: &failed_fingerprints,
            force_new: false,
        },
    )?;
    mark_project_failed(
        &conn,
        failed_batch,
        "tsconfig.failed.json",
        std::slice::from_ref(&occurrence),
        "synthetic failure",
    )?;
    let error = activate_staging_batch(repo.path(), &conn, failed_batch, &snapshot, true)
        .expect_err("an all-failed batch must not activate");
    assert!(error.to_string().contains("no completed projects"));

    let active_batch: i64 = conn.query_row(
        "SELECT id FROM checker_enrichment_batches WHERE active=1",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(active_batch, healthy_batch);
    let live_edges: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges WHERE provenance='checker'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(live_edges, 1);
    Ok(())
}

/// Index one file and hand back its connection plus its indexed hash.
fn indexed(repo: &Path, source: &str) -> Result<(Connection, String)> {
    fs::write(repo.join("main.ts"), source)?;
    let conn = crate::store::open(repo)?;
    crate::indexer::index_repo(repo, &conn)?;
    let hash = conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
        row.get::<_, String>(0)
    })?;
    Ok((conn, hash))
}

/// The checker reports a declaration by its *name* span; `at` locates the
/// same name the same way the sidecar's `declarationResult` does.
fn declaration_at(source: &str, anchor: &str, name: &str, hash: &str) -> DeclarationSite {
    let start = source.find(anchor).expect("anchor present") as i64;
    DeclarationSite {
        file: Some("main.ts".into()),
        outside_root: false,
        start,
        end: start + name.len() as i64,
        source_hash: hash.to_string(),
        context: Some("repo".into()),
    }
}

fn answer(declarations: Vec<DeclarationSite>) -> ProjectAnswer {
    ProjectAnswer {
        project_id: "tsconfig.json".into(),
        status: "resolved".into(),
        receiver_type: Some("CardTable".into()),
        declarations,
        checker_input_fingerprint: "inputs".into(),
    }
}

fn project_answer(project_id: &str, declarations: Vec<DeclarationSite>) -> ProjectAnswer {
    ProjectAnswer {
        project_id: project_id.into(),
        checker_input_fingerprint: format!("{project_id}-inputs"),
        ..answer(declarations)
    }
}

fn occurrence(conn: &Connection) -> Result<Occurrence> {
    occurrence_in(conn, "main.ts")
}

fn occurrence_in(conn: &Connection, file: &str) -> Result<Occurrence> {
    Ok(load_occurrences(conn)?
        .into_iter()
        .find(|occurrence| occurrence.file == file && occurrence.member == "insert")
        .expect("indexed insert occurrence"))
}

fn seed_active_checker_batch(
    root: &Path,
    conn: &Connection,
    source_hash: &str,
    occurrence: &Occurrence,
    declaration: &DeclarationSite,
    project_fingerprints: &BTreeMap<String, String>,
) -> Result<i64> {
    let snapshot = crate::structural::current_snapshot(conn)?;
    let identity = TypeScriptIdentity {
        version: "5.9.3".into(),
        source: "bundled".into(),
    };
    let projects = project_fingerprints
        .keys()
        .map(|project| (project.clone(), vec![occurrence.clone()]))
        .collect::<BTreeMap<_, _>>();
    let batch_id = open_staging_batch(
        conn,
        &StagingPlan {
            snapshot: &snapshot,
            plan_fingerprint: "initial-plan",
            checker: &identity,
            protocol: 2,
            selected_occurrences: 1,
            projects: &projects,
            project_fingerprints,
            force_new: false,
        },
    )?;
    for project in project_fingerprints.keys() {
        let answer = project_answer(project, vec![declaration.clone()]);
        let outcome = map_occurrence(conn, occurrence, &[answer])?;
        stage_batch(
            conn,
            batch_id,
            project,
            std::slice::from_ref(occurrence),
            &outcome.facts,
            &outcome.projects,
        )?;
        complete_project(
            root,
            conn,
            batch_id,
            project,
            &format!("{project}-inputs"),
            &[super::super::protocol::CheckerInputFile {
                path: root.join("main.ts").to_string_lossy().into_owned(),
                source_hash: source_hash.to_string(),
            }],
            1,
            1,
        )?;
    }
    activate_staging_batch(root, conn, batch_id, &snapshot, false)?;
    crate::structural::rebuild_projection(conn, &snapshot)?;
    Ok(batch_id)
}

#[test]
fn input_freshness_cache_pins_first_digest_per_invocation() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("ambient.d.ts");
    let initial = b"declare const ambient: string;\n";
    let changed = b"declare const ambient: number;\n";
    fs::write(&path, initial)?;
    let initial_hash = blake3::hash(initial).to_hex().to_string();
    let changed_hash = blake3::hash(changed).to_hex().to_string();
    let repository_input = ValidatedInput {
        kind: "repository".into(),
        path: "ambient.d.ts".into(),
        source_hash: initial_hash.clone(),
    };
    let absolute_input = ValidatedInput {
        kind: "absolute".into(),
        path: path.to_string_lossy().into_owned(),
        source_hash: initial_hash.clone(),
    };

    let mut invocation = InputFreshnessCache::new(root.path());
    assert!(invocation.matches(&repository_input));
    assert!(invocation.matches(&absolute_input));
    assert_eq!(
        invocation.digests.len(),
        1,
        "equivalent resolved paths must share one read"
    );

    fs::write(&path, changed)?;
    assert!(
        invocation.matches(&repository_input),
        "one invocation consistently uses the first observed digest"
    );
    let mut next_invocation = InputFreshnessCache::new(root.path());
    assert!(!next_invocation.matches(&repository_input));
    assert!(next_invocation.matches(&ValidatedInput {
        source_hash: changed_hash,
        ..absolute_input
    }));
    Ok(())
}

#[test]
fn watch_carry_rebinds_unchanged_facts_to_current_member_call_rows() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = "export class CardTable { insert(): void {} }\n\
                  declare const card: CardTable; card.insert();\n";
    let (conn, hash) = indexed(repo.path(), source)?;
    let old_occurrence = occurrence(&conn)?;
    let identity = TypeScriptIdentity {
        version: "5.9.3".into(),
        source: "bundled".into(),
    };
    let project_fingerprints =
        BTreeMap::from([("tsconfig.json".to_string(), "stable-plan".to_string())]);
    let declaration = declaration_at(source, "insert(): void {}", "insert", &hash);
    let old_batch = seed_active_checker_batch(
        repo.path(),
        &conn,
        &hash,
        &old_occurrence,
        &declaration,
        &project_fingerprints,
    )?;
    let external = tempfile::tempdir()?;
    let external_input = external.path().join("ambient.d.ts");
    fs::write(&external_input, "declare const ambient: string;\n")?;
    let external_hash = blake3::hash(&fs::read(&external_input)?)
        .to_hex()
        .to_string();
    conn.execute(
        "INSERT INTO checker_project_inputs(
           batch_id, project_id, input_kind, input_path, source_hash
         ) VALUES(?1,'tsconfig.json','absolute',?2,?3)",
        params![
            old_batch,
            external_input.to_string_lossy().as_ref(),
            external_hash
        ],
    )?;

    fs::write(
        repo.path().join("a.ts"),
        "class Earlier { insert(): void {} }\n\
         declare const earlier: Earlier; earlier.insert();\n",
    )?;
    crate::indexer::watch_full_refresh_repo_with_options(
        repo.path(),
        &conn,
        &crate::indexer::IndexOptions::default(),
    )?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let current_occurrence = occurrence_in(&conn, "main.ts")?;
    assert_ne!(
        current_occurrence.id, old_occurrence.id,
        "fixture must exercise rowid rebinding"
    );
    let projects = BTreeMap::from([(
        "tsconfig.json".to_string(),
        vec![current_occurrence.clone()],
    )]);
    let batch_id = open_staging_batch(
        &conn,
        &StagingPlan {
            snapshot: &snapshot,
            plan_fingerprint: "next-plan",
            checker: &identity,
            protocol: 2,
            selected_occurrences: 1,
            projects: &projects,
            project_fingerprints: &project_fingerprints,
            force_new: false,
        },
    )?;
    let mut input_freshness = InputFreshnessCache::new(repo.path());
    let carried = carry_forward_projects(
        &conn,
        batch_id,
        &identity,
        2,
        &projects,
        &project_fingerprints,
        &mut input_freshness,
    )?;
    assert_eq!(carried.projects_carried, 1);
    assert_eq!(carried.occurrences_carried, 1);
    assert!(carried.projects_requiring_check.is_empty());
    assert!(project_complete_and_fresh(
        &conn,
        batch_id,
        "tsconfig.json",
        &mut input_freshness,
    )?);
    let rebound: i64 = conn.query_row(
        "SELECT member_call_id FROM checker_occurrence_projects
         WHERE batch_id=?1 AND project_id='tsconfig.json'",
        [batch_id],
        |row| row.get(0),
    )?;
    assert_eq!(rebound, current_occurrence.id);
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM checker_project_inputs
             WHERE batch_id=?1 AND project_id='tsconfig.json'
               AND input_kind='absolute' AND input_path=?2",
            params![batch_id, external_input.to_string_lossy().as_ref()],
            |row| row.get::<_, i64>(0),
        )?,
        1,
        "carried projects retain external checker-input watch coverage"
    );

    activate_staging_batch(repo.path(), &conn, batch_id, &snapshot, false)?;
    crate::structural::rebuild_projection(&conn, &snapshot)?;
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM resolved_edges
             WHERE provenance='checker' AND source_ref_id=?1",
            [current_occurrence.id],
            |row| row.get::<_, i64>(0),
        )?,
        1
    );
    Ok(())
}

#[test]
fn changed_external_input_prevents_project_carry() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = "export class CardTable { insert(): void {} }\n\
                  declare const card: CardTable; card.insert();\n";
    let (conn, hash) = indexed(repo.path(), source)?;
    let old_occurrence = occurrence(&conn)?;
    let identity = TypeScriptIdentity {
        version: "5.9.3".into(),
        source: "bundled".into(),
    };
    let project_fingerprints =
        BTreeMap::from([("tsconfig.json".to_string(), "stable-plan".to_string())]);
    let declaration = declaration_at(source, "insert(): void {}", "insert", &hash);
    let old_batch = seed_active_checker_batch(
        repo.path(),
        &conn,
        &hash,
        &old_occurrence,
        &declaration,
        &project_fingerprints,
    )?;
    let external = tempfile::tempdir()?;
    let external_input = external.path().join("ambient.d.ts");
    fs::write(&external_input, "declare const ambient: string;\n")?;
    let external_hash = blake3::hash(&fs::read(&external_input)?)
        .to_hex()
        .to_string();
    conn.execute(
        "INSERT INTO checker_project_inputs(
           batch_id, project_id, input_kind, input_path, source_hash
         ) VALUES(?1,'tsconfig.json','absolute',?2,?3)",
        params![
            old_batch,
            external_input.to_string_lossy().as_ref(),
            external_hash
        ],
    )?;
    fs::write(&external_input, "declare const ambient: number;\n")?;
    fs::write(
        repo.path().join("unrelated.ts"),
        "export const value = 1;\n",
    )?;
    crate::indexer::watch_full_refresh_repo_with_options(
        repo.path(),
        &conn,
        &crate::indexer::IndexOptions::default(),
    )?;

    let current_occurrence = occurrence(&conn)?;
    let projects = BTreeMap::from([("tsconfig.json".to_string(), vec![current_occurrence])]);
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let batch_id = open_staging_batch(
        &conn,
        &StagingPlan {
            snapshot: &snapshot,
            plan_fingerprint: "next-plan",
            checker: &identity,
            protocol: 2,
            selected_occurrences: 1,
            projects: &projects,
            project_fingerprints: &project_fingerprints,
            force_new: false,
        },
    )?;
    let carried = carry_forward_projects(
        &conn,
        batch_id,
        &identity,
        2,
        &projects,
        &project_fingerprints,
        &mut InputFreshnessCache::new(repo.path()),
    )?;
    assert_eq!(carried.projects_carried, 0);
    assert_eq!(carried.occurrences_carried, 0);
    assert_eq!(
        carried.projects_requiring_check,
        BTreeSet::from(["tsconfig.json".to_string()])
    );
    Ok(())
}

#[test]
fn one_changed_owner_prevents_every_owner_from_carrying_an_occurrence() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = "export class CardTable { insert(): void {} }\n\
                  declare const card: CardTable; card.insert();\n";
    let (conn, hash) = indexed(repo.path(), source)?;
    let old_occurrence = occurrence(&conn)?;
    let identity = TypeScriptIdentity {
        version: "5.9.3".into(),
        source: "bundled".into(),
    };
    let old_fingerprints = BTreeMap::from([
        ("tsconfig.a.json".to_string(), "stable-a".to_string()),
        ("tsconfig.b.json".to_string(), "old-b".to_string()),
    ]);
    let declaration = declaration_at(source, "insert(): void {}", "insert", &hash);
    seed_active_checker_batch(
        repo.path(),
        &conn,
        &hash,
        &old_occurrence,
        &declaration,
        &old_fingerprints,
    )?;
    fs::write(
        repo.path().join("unrelated.ts"),
        "export const value = 1;\n",
    )?;
    crate::indexer::watch_full_refresh_repo_with_options(
        repo.path(),
        &conn,
        &crate::indexer::IndexOptions::default(),
    )?;
    let current_occurrence = occurrence(&conn)?;
    let projects = BTreeMap::from([
        (
            "tsconfig.a.json".to_string(),
            vec![current_occurrence.clone()],
        ),
        ("tsconfig.b.json".to_string(), vec![current_occurrence]),
    ]);
    let current_fingerprints = BTreeMap::from([
        ("tsconfig.a.json".to_string(), "stable-a".to_string()),
        ("tsconfig.b.json".to_string(), "changed-b".to_string()),
    ]);
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let batch_id = open_staging_batch(
        &conn,
        &StagingPlan {
            snapshot: &snapshot,
            plan_fingerprint: "next-plan",
            checker: &identity,
            protocol: 2,
            selected_occurrences: 1,
            projects: &projects,
            project_fingerprints: &current_fingerprints,
            force_new: false,
        },
    )?;
    let carried = carry_forward_projects(
        &conn,
        batch_id,
        &identity,
        2,
        &projects,
        &current_fingerprints,
        &mut InputFreshnessCache::new(repo.path()),
    )?;
    assert_eq!(carried.projects_carried, 0);
    assert_eq!(carried.occurrences_carried, 0);
    assert_eq!(
        carried.projects_requiring_check,
        BTreeSet::from(["tsconfig.a.json".to_string(), "tsconfig.b.json".to_string()])
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM checker_occurrence_projects WHERE batch_id=?1",
            [batch_id],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );
    Ok(())
}

#[test]
fn changed_target_content_prevents_fact_carry_even_when_project_scope_is_stable() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let main_source = "import { CardTable } from './target';\n\
                       declare const card: CardTable; card.insert();\n";
    let target_source = "export class CardTable { insert(): void {} }\n";
    fs::write(repo.path().join("main.ts"), main_source)?;
    fs::write(repo.path().join("target.ts"), target_source)?;
    let conn = crate::store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let main_hash = conn.query_row("SELECT hash FROM files WHERE path='main.ts'", [], |row| {
        row.get::<_, String>(0)
    })?;
    let target_hash =
        conn.query_row("SELECT hash FROM files WHERE path='target.ts'", [], |row| {
            row.get::<_, String>(0)
        })?;
    let initial_occurrence = occurrence(&conn)?;
    let declaration_start = target_source.find("insert").expect("declaration") as i64;
    let declaration = DeclarationSite {
        file: Some("target.ts".into()),
        outside_root: false,
        start: declaration_start,
        end: declaration_start + "insert".len() as i64,
        source_hash: target_hash,
        context: Some("repo".into()),
    };
    let fingerprints = BTreeMap::from([("tsconfig.json".to_string(), "stable-plan".to_string())]);
    seed_active_checker_batch(
        repo.path(),
        &conn,
        &main_hash,
        &initial_occurrence,
        &declaration,
        &fingerprints,
    )?;

    fs::write(
        repo.path().join("target.ts"),
        "export class CardTable { insert(): void { return; } }\n",
    )?;
    crate::indexer::watch_full_refresh_repo_with_options(
        repo.path(),
        &conn,
        &crate::indexer::IndexOptions::default(),
    )?;
    let current_occurrence = occurrence(&conn)?;
    let projects = BTreeMap::from([("tsconfig.json".to_string(), vec![current_occurrence])]);
    let identity = TypeScriptIdentity {
        version: "5.9.3".into(),
        source: "bundled".into(),
    };
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let batch_id = open_staging_batch(
        &conn,
        &StagingPlan {
            snapshot: &snapshot,
            plan_fingerprint: "next-plan",
            checker: &identity,
            protocol: 2,
            selected_occurrences: 1,
            projects: &projects,
            project_fingerprints: &fingerprints,
            force_new: false,
        },
    )?;
    let carried = carry_forward_projects(
        &conn,
        batch_id,
        &identity,
        2,
        &projects,
        &fingerprints,
        &mut InputFreshnessCache::new(repo.path()),
    )?;
    assert_eq!(carried.occurrences_carried, 0);
    assert_eq!(
        carried.projects_requiring_check,
        BTreeSet::from(["tsconfig.json".to_string()])
    );
    Ok(())
}

/// Anchoring by span containment alone let any declaration nested inside an
/// indexed symbol's body claim that symbol: an object-literal method inside
/// a function published a fabricated `likely` self-edge on the function.
/// Mapping must require the symbol to BE the member's declaration.
#[test]
fn an_object_literal_method_inside_a_function_maps_to_nothing_instead_of_its_container()
-> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = "export class CardTable { insert(): void {} }\n\
                  export function caller(): void {\n\
                  \x20 const rows = { insert(): void {} };\n\
                  \x20 rows.insert();\n\
                  }\n";
    let (conn, hash) = indexed(repo.path(), source)?;
    let literal_method_start = source.rfind("insert(): void {}").expect("literal method") as i64;
    let declaration = DeclarationSite {
        file: Some("main.ts".into()),
        outside_root: false,
        start: literal_method_start,
        end: literal_method_start + "insert".len() as i64,
        source_hash: hash,
        context: Some("repo".into()),
    };

    // The containment trap is live: indexed symbols really do enclose this
    // declaration, so refusing it is a decision and not an accident.
    let containers: i64 = conn.query_row(
        "SELECT count(*) FROM symbols symbol JOIN files file ON file.id=symbol.file_id
         WHERE file.path='main.ts'
           AND symbol.decl_start<=?1 AND symbol.decl_end>=?2",
        params![declaration.start, declaration.end],
        |row| row.get(0),
    )?;
    assert!(containers > 0, "the enclosing symbol must exist");

    assert!(map_declaration(&conn, "insert", &declaration)?.is_none());
    let outcome = map_occurrence(&conn, &occurrence(&conn)?, &[answer(vec![declaration])])?;
    assert!(
        outcome.facts.is_empty(),
        "an unindexed declaration must publish no edge, least of all a self-edge"
    );
    assert_eq!(outcome.unmapped_declarations, 1);
    Ok(())
}

/// Ambiguity is judged over the checker's whole answer. Two valid targets
/// where only one maps (jscout indexes no members for erased interfaces)
/// must not collapse into a lone arbitrary `likely` edge.
#[test]
fn a_declaration_that_cannot_map_keeps_every_surviving_edge_possible() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = "export class CardTable { insert(): void {} }\n\
                  export interface Vendor { insert(): void }\n\
                  declare const target: CardTable | Vendor;\n\
                  target.insert();\n";
    let (conn, hash) = indexed(repo.path(), source)?;
    let class_member = declaration_at(source, "insert(): void {}", "insert", &hash);
    let interface_member = declaration_at(source, "insert(): void }", "insert", &hash);
    assert!(
        map_declaration(&conn, "insert", &class_member)?.is_some(),
        "the class method is the mappable target"
    );
    assert!(
        map_declaration(&conn, "insert", &interface_member)?.is_none(),
        "the erased interface member has no indexed anchor"
    );
    let occurrence = occurrence(&conn)?;

    let ambiguous = map_occurrence(
        &conn,
        &occurrence,
        &[answer(vec![class_member.clone(), interface_member])],
    )?;
    assert_eq!(ambiguous.facts.len(), 1);
    assert_eq!(ambiguous.unmapped_declarations, 1);
    assert_eq!(
        ambiguous.facts[0].confidence, "possible",
        "a target the checker saw but jscout could not map still means ambiguity"
    );

    // Control: the same lone survivor is `likely` only when the checker
    // itself named exactly one target.
    let unambiguous = map_occurrence(&conn, &occurrence, &[answer(vec![class_member])])?;
    assert_eq!(unambiguous.facts.len(), 1);
    assert_eq!(unambiguous.unmapped_declarations, 0);
    assert_eq!(unambiguous.facts[0].confidence, "likely");
    Ok(())
}

#[test]
fn later_project_cannot_upgrade_an_earlier_ambiguous_answer() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = "export class CardTable { insert(): void {} }\n\
                  export interface Vendor { insert(): void }\n\
                  declare const target: CardTable | Vendor;\n\
                  target.insert();\n";
    let (conn, hash) = indexed(repo.path(), source)?;
    let class_member = declaration_at(source, "insert(): void {}", "insert", &hash);
    let interface_member = declaration_at(source, "insert(): void }", "insert", &hash);
    let occurrence = occurrence(&conn)?;

    for answers in [
        vec![
            project_answer(
                "tsconfig.a.json",
                vec![class_member.clone(), interface_member.clone()],
            ),
            project_answer("tsconfig.b.json", vec![class_member.clone()]),
        ],
        vec![
            project_answer("tsconfig.b.json", vec![class_member.clone()]),
            project_answer(
                "tsconfig.a.json",
                vec![class_member.clone(), interface_member.clone()],
            ),
        ],
    ] {
        let outcome = map_occurrence(&conn, &occurrence, &answers)?;
        assert_eq!(outcome.facts.len(), 2);
        assert_eq!(outcome.unmapped_declarations, 1);
        assert!(
            outcome
                .facts
                .iter()
                .all(|fact| fact.confidence == "possible"),
            "confidence must be independent of project answer order"
        );
    }
    Ok(())
}

#[test]
fn an_unknown_owning_project_is_visible_without_demoting_a_clean_resolution() -> Result<()> {
    let repo = tempfile::tempdir()?;
    let source = "export class CardTable { insert(): void {} }\n\
                  declare const target: CardTable;\n\
                  target.insert();\n";
    let (conn, hash) = indexed(repo.path(), source)?;
    let class_member = declaration_at(source, "insert(): void {}", "insert", &hash);
    let occurrence = occurrence(&conn)?;
    let unknown = ProjectAnswer {
        project_id: "tsconfig.unknown.json".into(),
        status: "unknown".into(),
        receiver_type: None,
        declarations: Vec::new(),
        checker_input_fingerprint: "unknown-inputs".into(),
    };
    let outcome = map_occurrence(
        &conn,
        &occurrence,
        &[
            project_answer("tsconfig.resolved.json", vec![class_member]),
            unknown,
        ],
    )?;
    assert_eq!(outcome.unknown_answers, 1);
    assert_eq!(outcome.facts.len(), 1);
    assert_eq!(outcome.facts[0].confidence, "likely");
    assert_eq!(outcome.projects.len(), 2);
    assert!(outcome.projects.iter().any(|project| {
        project.project_id == "tsconfig.unknown.json" && project.status == "unknown"
    }));
    Ok(())
}

#[test]
fn raced_snapshot_or_checker_input_publishes_nothing() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.ts"), "export const value = 1;\n")?;
    let conn = crate::store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    conn.execute(
        "INSERT INTO checker_enrichment_batches(
           source_snapshot, checker_version, checker_source,
           checker_input_fingerprint, sidecar_protocol, created_at, active
         ) VALUES(?1,'5.9.3','bundled','previous',1,datetime('now'),1)",
        [&snapshot],
    )?;
    let previous_batch = conn.last_insert_rowid();
    let identity = TypeScriptIdentity {
        version: "5.9.3".into(),
        source: "bundled".into(),
    };

    let stale_snapshot = publish(
        repo.path(),
        &conn,
        &PublishPlan {
            snapshot: "stale",
            checker: &identity,
            protocol: 1,
            input_fingerprint: "new",
            facts: &[],
            projects: &[],
            inputs: &[],
        },
    )
    .expect_err("snapshot race");
    assert!(stale_snapshot.to_string().contains("snapshot changed"));

    let ambient = repo.path().join("ambient.d.ts");
    fs::write(&ambient, "declare const version: 1;\n")?;
    let expected = blake3::hash(&fs::read(&ambient)?).to_hex().to_string();
    fs::write(&ambient, "declare const version: 2;\n")?;
    let raced_input = publish(
        repo.path(),
        &conn,
        &PublishPlan {
            snapshot: &snapshot,
            checker: &identity,
            protocol: 1,
            input_fingerprint: "new",
            facts: &[],
            projects: &[],
            inputs: &[ValidatedInput {
                kind: "absolute".into(),
                path: ambient.to_string_lossy().into_owned(),
                source_hash: expected,
            }],
        },
    )
    .expect_err("input race");
    assert!(raced_input.to_string().contains("changed since"));

    let batches: (i64, i64) = conn.query_row(
        "SELECT count(*), sum(active) FROM checker_enrichment_batches",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let active: i64 = conn.query_row(
        "SELECT active FROM checker_enrichment_batches WHERE id=?1",
        [previous_batch],
        |row| row.get(0),
    )?;
    assert_eq!(batches, (1, 1));
    assert_eq!(active, 1);
    Ok(())
}

/// Nothing reads a superseded batch, so a repeatedly enriched repository
/// must not accumulate one dead batch and its facts per pass.
#[test]
fn publishing_a_batch_prunes_the_one_it_supersedes() -> Result<()> {
    let repo = tempfile::tempdir()?;
    fs::write(repo.path().join("main.ts"), "export const value = 1;\n")?;
    let conn = crate::store::open(repo.path())?;
    crate::indexer::index_repo(repo.path(), &conn)?;
    let snapshot = crate::structural::current_snapshot(&conn)?;
    let identity = TypeScriptIdentity {
        version: "5.9.3".into(),
        source: "bundled".into(),
    };
    let plan = |fingerprint: &'static str| PublishPlan {
        snapshot: &snapshot,
        checker: &identity,
        protocol: 1,
        input_fingerprint: fingerprint,
        facts: &[],
        projects: &[],
        inputs: &[],
    };

    let first = publish(repo.path(), &conn, &plan("one"))?;
    let second = publish(repo.path(), &conn, &plan("two"))?;
    let third = publish(repo.path(), &conn, &plan("three"))?;
    assert_ne!(first, second);

    let surviving: Vec<(i64, i64)> = conn
        .prepare("SELECT id, active FROM checker_enrichment_batches ORDER BY id")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;
    assert_eq!(surviving, vec![(third, 1)]);
    Ok(())
}
