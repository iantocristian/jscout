use std::fs;
use std::path::Path;

use anyhow::Result;

use super::{
    NeighborhoodOptions, compute_snapshot, neighborhood, rebuild_projection, workflow_neighborhood,
};
use crate::{indexer, origin, store};

fn write(root: &Path, path: &str, source: &str) -> Result<()> {
    let path = root.join(path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, source)?;
    Ok(())
}

#[test]
fn projects_resolved_calls_and_returns_snapshot() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "a.ts",
        "export function greet(name) { return name; }\n",
    )?;
    write(
        repo.path(),
        "b.ts",
        "import { greet } from './a';\nexport function run() { return greet('x'); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let result = neighborhood(
        &conn,
        "a.ts:greet",
        &NeighborhoodOptions {
            depth: 2,
            ..Default::default()
        },
    )?;
    assert_eq!(result.snapshot.len(), 64);
    assert!(result.resolved_anchor.contains("a.ts#::greet@1"));
    assert!(result.edges.iter().any(|edge| {
        edge.kind == "call"
            && edge.source.contains("b.ts#::run@1")
            && edge.target.contains("a.ts#::greet@1")
            && edge.confidence == "certain"
    }));
    Ok(())
}

#[test]
fn snapshot_hashes_file_corpus_and_parser_format() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(repo.path(), "main.ts", "export const value = 1;\n")?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let baseline = compute_snapshot(&conn)?;
    assert_eq!(baseline, super::current_snapshot(&conn)?);

    conn.execute(
        "UPDATE files SET format='javascript' WHERE path='main.ts'",
        [],
    )?;
    let changed_format = compute_snapshot(&conn)?;
    assert_ne!(changed_format, baseline);

    conn.execute(
        "UPDATE files SET format='typescript', corpus='docs' WHERE path='main.ts'",
        [],
    )?;
    let changed_corpus = compute_snapshot(&conn)?;
    assert_ne!(changed_corpus, baseline);
    assert_ne!(changed_corpus, changed_format);
    Ok(())
}

#[test]
fn snapshot_hashes_stable_docs_provenance_not_git_cache_mechanics() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(repo.path(), "main.ts", "export const value = 1;\n")?;
    write(repo.path(), "README.md", "# Guide\n\nCurrent guidance.\n")?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    conn.execute(
        "INSERT INTO meta(key,value)
         VALUES('documentation_provenance_format_version',?1)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [crate::docs::PROVENANCE_FORMAT_VERSION],
    )?;
    let (file_id, chunk_id): (i64, i64) = conn.query_row(
        "SELECT f.id, c.id
         FROM files f JOIN chunks c ON c.file_id=f.id
         WHERE f.path='README.md'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    conn.execute(
        "INSERT INTO doc_file_provenance(file_id,projection_hash,status,detail)
         VALUES(?1,'projection-a','resolved','diagnostic-a')
         ON CONFLICT(file_id) DO UPDATE SET
           projection_hash=excluded.projection_hash,
           status=excluded.status,
           detail=excluded.detail",
        [file_id],
    )?;
    conn.execute(
        "UPDATE doc_chunk_meta
         SET freshness_basis='git', freshness_author_time=100,
             freshness_committer_time=200, freshness_detail='diagnostic-a'
         WHERE chunk_id=?1",
        [chunk_id],
    )?;
    let stable = compute_snapshot(&conn)?;
    assert_eq!(compute_snapshot(&conn)?, stable);

    conn.execute(
        "UPDATE doc_file_provenance
         SET projection_hash='projection-b', detail='diagnostic-b'
         WHERE file_id=?1",
        [file_id],
    )?;
    conn.execute(
        "UPDATE doc_chunk_meta
         SET freshness_committer_time=201, freshness_detail='diagnostic-b'
         WHERE chunk_id=?1",
        [chunk_id],
    )?;
    conn.execute(
        "INSERT INTO doc_blame_cache(
           path_scope,path,bytes_hash,converted_blob_oid,path_tip,
           shallow_fingerprint,attribution_json,format_version
         ) VALUES(
           'scope-a','README.md','bytes-a','converted-a','tip-a','shallow-a','[]','cache-v1'
         )",
        [],
    )?;
    assert_eq!(
        compute_snapshot(&conn)?,
        stable,
        "diagnostics, committer time, projection caches, and blame cache identity are not search snapshot state"
    );

    conn.execute(
        "UPDATE doc_blame_cache
         SET bytes_hash='bytes-b', converted_blob_oid='converted-b', path_tip='tip-b',
             shallow_fingerprint='shallow-b',
             attribution_json='[{\"basis\":\"working_tree\"}]'",
        [],
    )?;
    assert_eq!(compute_snapshot(&conn)?, stable);

    conn.execute(
        "UPDATE doc_chunk_meta SET freshness_author_time=101 WHERE chunk_id=?1",
        [chunk_id],
    )?;
    let newer_author = compute_snapshot(&conn)?;
    assert_ne!(newer_author, stable);

    conn.execute(
        "UPDATE doc_chunk_meta
         SET freshness_basis='working_tree', freshness_author_time=100
         WHERE chunk_id=?1",
        [chunk_id],
    )?;
    let working_tree = compute_snapshot(&conn)?;
    assert_ne!(working_tree, stable);
    assert_ne!(working_tree, newer_author);

    conn.execute(
        "UPDATE doc_chunk_meta
         SET freshness_basis='git', freshness_author_time=100
         WHERE chunk_id=?1",
        [chunk_id],
    )?;
    conn.execute(
        "UPDATE doc_file_provenance SET status='blame_failed' WHERE file_id=?1",
        [file_id],
    )?;
    assert_ne!(compute_snapshot(&conn)?, stable);

    conn.execute(
        "UPDATE doc_file_provenance SET status='resolved' WHERE file_id=?1",
        [file_id],
    )?;
    conn.execute(
        "UPDATE meta SET value='documentation-provenance-v0'
         WHERE key='documentation_provenance_format_version'",
        [],
    )?;
    assert_ne!(compute_snapshot(&conn)?, stable);
    Ok(())
}

#[test]
fn docs_provenance_snapshot_orders_by_source_identity_not_row_id() -> Result<()> {
    let snapshot_for_order = |paths: &[&str]| -> Result<String> {
        let repo = tempfile::tempdir()?;
        let conn = store::open(repo.path())?;
        conn.execute_batch(&format!(
            "INSERT INTO meta(key,value) VALUES
               ('extraction_version','{}'),
               ('documentation_chunk_format_version','{}'),
               ('documentation_provenance_format_version','{}');",
            crate::entity::EXTRACTION_VERSION,
            crate::docs::CHUNK_FORMAT_VERSION,
            crate::docs::PROVENANCE_FORMAT_VERSION,
        ))?;
        for (offset, path) in paths.iter().enumerate() {
            let id = offset as i64 + 1;
            let (status, basis, author_time) = if *path == "a.md" {
                ("resolved", "git", Some(10_i64))
            } else {
                ("untracked_or_new", "unknown", None)
            };
            conn.execute(
                "INSERT INTO files(id,path,hash,corpus,format,role,origin)
                 VALUES(?1,?2,?3,'docs','markdown','documentation','repository')",
                rusqlite::params![id, path, format!("file-{path}")],
            )?;
            conn.execute(
                "INSERT INTO chunks(
                   id,file_id,kind,name,scope_chain,symbols,start,end,
                   start_line,end_line,hash,content
                 ) VALUES(?1,?1,'markdown_section',NULL,'','',0,4,1,1,?2,'body')",
                rusqlite::params![id, format!("chunk-{path}")],
            )?;
            conn.execute(
                "INSERT INTO doc_chunk_meta(
                   chunk_id,title,breadcrumb,ordinal,front_matter_state,
                   freshness_basis,freshness_author_time
                 ) VALUES(?1,?2,'',0,'absent',?3,?4)",
                rusqlite::params![id, path, basis, author_time],
            )?;
            conn.execute(
                "INSERT INTO doc_file_provenance(
                   file_id,projection_hash,status,detail
                 ) VALUES(?1,?2,?3,NULL)",
                rusqlite::params![id, format!("projection-{path}"), status],
            )?;
        }
        compute_snapshot(&conn)
    };

    assert_eq!(
        snapshot_for_order(&["a.md", "b.md"])?,
        snapshot_for_order(&["b.md", "a.md"])?
    );
    Ok(())
}

#[test]
fn neighborhood_orders_parallel_edges_deterministically() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "parallel.ts",
        "export function source() {}\nexport function target() {}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let (source, target, file_id): (String, String, i64) = conn.query_row(
        "SELECT source.node_key, target.node_key, source.file_id
         FROM graph_nodes source
         JOIN graph_nodes target ON target.file_id=source.file_id
         WHERE source.display_name='source' AND target.display_name='target'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    for line in [80, 20, 60, 10, 70, 30, 50, 40] {
        conn.execute(
            "INSERT INTO resolved_edges(
               src_key, dst_key, kind, confidence, provenance,
               source_file_id, line, detail_json
             ) VALUES(?1, ?2, 'call', 'certain', 'parallel-test', ?3, ?4, ?5)",
            rusqlite::params![
                source,
                target,
                file_id,
                line,
                serde_json::json!({ "line": line }).to_string()
            ],
        )?;
    }

    for _ in 0..16 {
        let result = neighborhood(
            &conn,
            &source,
            &NeighborhoodOptions {
                direction: "out".into(),
                node_limit: 10,
                edge_limit: 20,
                ..Default::default()
            },
        )?;
        let lines = result
            .edges
            .iter()
            .map(|edge| edge.line.expect("parallel edge line"))
            .collect::<Vec<_>>();
        assert_eq!(lines, vec![10, 20, 30, 40, 50, 60, 70, 80]);

        let truncated = neighborhood(
            &conn,
            &source,
            &NeighborhoodOptions {
                direction: "out".into(),
                node_limit: 10,
                edge_limit: 4,
                ..Default::default()
            },
        )?;
        let retained_lines = truncated
            .edges
            .iter()
            .map(|edge| edge.line.expect("parallel edge line"))
            .collect::<Vec<_>>();
        assert_eq!(retained_lines, vec![10, 20, 30, 40]);
        assert!(truncated.truncated);
    }
    Ok(())
}

#[test]
fn paths_returns_ranked_bounded_composed_routes() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "flow.ts",
        "export function finish() {}\n\
         export function middle() { finish(); }\n\
         export function start() { middle(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let result = super::paths(
        &conn,
        "flow.ts:start",
        "flow.ts:finish",
        &super::PathOptions {
            direction: "out".into(),
            max_depth: 3,
            kinds: vec!["call".into()],
            ..Default::default()
        },
    )?;
    assert_eq!(result.paths.len(), 1);
    assert_eq!(result.paths[0].steps.len(), 2);
    assert!(result.paths[0].score > 0.0);
    assert_eq!(result.paths[0].nodes[0].display_name, "start");
    assert_eq!(result.paths[0].nodes[2].display_name, "finish");
    Ok(())
}

#[test]
fn paths_marks_reverse_traversal_explicitly() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "flow.ts",
        "export function finish() {}\n\
         export function middle() { finish(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let result = super::paths(
        &conn,
        "flow.ts:finish",
        "flow.ts:middle",
        &super::PathOptions {
            direction: "both".into(),
            max_depth: 1,
            kinds: vec!["call".into()],
            ..Default::default()
        },
    )?;
    let step = &result.paths[0].steps[0];
    assert!(step.reversed);
    assert_eq!(step.from, step.edge.target);
    assert_eq!(step.to, step.edge.source);
    Ok(())
}

#[test]
fn paths_caps_explored_prefix_states() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "flow.ts",
        "export function root() {}\nexport function target() {}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let root: String = conn.query_row(
        "SELECT node_key FROM graph_nodes WHERE node_kind='symbol' AND display_name='root'",
        [],
        |row| row.get(0),
    )?;
    let target: String = conn.query_row(
        "SELECT node_key FROM graph_nodes WHERE node_kind='symbol' AND display_name='target'",
        [],
        |row| row.get(0),
    )?;

    for layer in 0..4 {
        for index in 0..15 {
            let key = format!("dense:{layer}:{index}");
            conn.execute(
                "INSERT INTO graph_nodes(node_key, node_kind, display_name, meta_json)
                 VALUES(?1, 'candidate', ?1, '{}')",
                [&key],
            )?;
            if layer == 0 {
                conn.execute(
                    "INSERT INTO resolved_edges(
                       src_key, dst_key, kind, confidence, provenance, detail_json
                     ) VALUES(?1, ?2, 'call', 'certain', 'test', '{}')",
                    rusqlite::params![root, key],
                )?;
            } else {
                for parent in 0..15 {
                    let parent_key = format!("dense:{}:{parent}", layer - 1);
                    conn.execute(
                        "INSERT INTO resolved_edges(
                           src_key, dst_key, kind, confidence, provenance, detail_json
                         ) VALUES(?1, ?2, 'call', 'certain', 'test', '{}')",
                        rusqlite::params![parent_key, key],
                    )?;
                }
            }
        }
    }

    let result = super::paths(
        &conn,
        &root,
        &target,
        &super::PathOptions {
            direction: "out".into(),
            max_depth: 4,
            path_limit: 1,
            kinds: vec!["call".into()],
            ..Default::default()
        },
    )?;
    assert!(result.paths.is_empty());
    assert_eq!(result.searched_states, super::MAX_PATH_SEARCH_STATES);
    assert!(result.truncated);
    Ok(())
}

#[test]
fn registry_entity_connects_dispatch_to_imported_registered_handler() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "identifier.ts",
        "export const TARGET_HANDLER_ID = 'handler-id';\n",
    )?;
    write(
        repo.path(),
        "handler.ts",
        "import { TARGET_HANDLER_ID } from './identifier';\n\
         export const continueWorkflow = () => 'continued';\n\
         export const processHandler = () => continueWorkflow();\n\
         export default defineLogicFunction({\n\
           universalIdentifier: TARGET_HANDLER_ID,\n\
           handler: processHandler,\n\
         });\n",
    )?;
    write(
        repo.path(),
        "route.ts",
        "import { TARGET_HANDLER_ID } from './identifier';\n\
         export const routeHandler = () => ({\n\
           targetLogicFunctionUniversalIdentifier: TARGET_HANDLER_ID,\n\
         });\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let registry_entities: i64 = conn.query_row(
        "SELECT count(*) FROM entities WHERE entity_type='registry'",
        [],
        |row| row.get(0),
    )?;
    let occurrences: i64 = conn.query_row(
        "SELECT count(*) FROM entity_occurrences occurrence
         JOIN entities entity ON entity.id=occurrence.entity_id
         WHERE entity.entity_type='registry'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!((registry_entities, occurrences), (1, 2));

    let result = neighborhood(
        &conn,
        "routeHandler",
        &NeighborhoodOptions {
            depth: 2,
            direction: "out".into(),
            ..Default::default()
        },
    )?;
    assert!(result.edges.iter().any(|edge| {
        edge.kind == "dispatches"
            && edge.source.contains("route.ts#::routeHandler@1")
            && edge.target.starts_with("entity:registry:ref-")
            && edge.confidence == "likely"
    }));
    assert!(result.edges.iter().any(|edge| {
        edge.kind == "registered_handler"
            && edge.source.starts_with("entity:registry:ref-")
            && edge.target.contains("handler.ts#::processHandler@1")
    }));
    let workflow = workflow_neighborhood(
        &conn,
        "sym:route.ts#::routeHandler@1",
        2,
        20,
        40,
        &origin::defaults(),
    )?;
    assert!(
        workflow
            .nodes
            .iter()
            .any(|node| node.display_name == "processHandler")
    );
    assert!(
        workflow
            .nodes
            .iter()
            .any(|node| node.display_name == "continueWorkflow")
    );
    assert!(!workflow.truncated);
    Ok(())
}

#[test]
fn lifecycle_entity_collapses_producer_helper_for_two_hop_worker_recall() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "worker.ts",
        "const OBJECT = 'slackAssistantRequest';\n\
         export const workerHandler = () => 'worked';\n\
         export default defineLogicFunction({\n\
           universalIdentifier: 'worker-id',\n\
           handler: workerHandler,\n\
           databaseEventTriggerSettings: { eventName: `${OBJECT}.created` },\n\
         });\n",
    )?;
    write(
        repo.path(),
        "create.ts",
        "export const createRequest = async (client) => {\n\
           await client.mutation({ createSlackAssistantRequest: { id: true } });\n\
           await client.mutation({ createSlackAssistantRequest: { id: true } });\n\
         };\n",
    )?;
    write(
        repo.path(),
        "enqueue.ts",
        "import { createRequest } from './create';\n\
         export const enqueueRequest = async (client) => createRequest(client);\n",
    )?;
    write(
        repo.path(),
        "entry.ts",
        "import { enqueueRequest } from './enqueue';\n\
         export const start = async (client) => enqueueRequest(client);\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let roles: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT occurrence.role FROM entity_occurrences occurrence
             JOIN entities entity ON entity.id=occurrence.entity_id
             WHERE entity.entity_key='entity:data_lifecycle:slackAssistantRequest.created'
             ORDER BY occurrence.role",
        )?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    assert_eq!(
        roles,
        [
            "lifecycle_listener",
            "lifecycle_producer",
            "lifecycle_producer"
        ]
    );
    let producer_edges: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges
         WHERE src_key LIKE '%create.ts#::createRequest@1'
           AND dst_key='entity:data_lifecycle:slackAssistantRequest.created'
           AND kind='produces_lifecycle'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        producer_edges, 1,
        "occurrences must not duplicate traversal edges"
    );
    let collapsed_edges: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges
         WHERE src_key LIKE '%enqueue.ts#::enqueueRequest@1'
           AND dst_key='entity:data_lifecycle:slackAssistantRequest.created'
           AND kind='produces_lifecycle_via'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        collapsed_edges, 1,
        "caller collapse must also be deduplicated"
    );

    let result = neighborhood(
        &conn,
        "enqueueRequest",
        &NeighborhoodOptions {
            depth: 2,
            direction: "out".into(),
            ..Default::default()
        },
    )?;
    assert!(result.edges.iter().any(|edge| {
        edge.kind == "produces_lifecycle_via"
            && edge.source.contains("enqueue.ts#::enqueueRequest@1")
            && edge.target == "entity:data_lifecycle:slackAssistantRequest.created"
    }));
    assert!(result.edges.iter().any(|edge| {
        edge.kind == "lifecycle_listener"
            && edge.source == "entity:data_lifecycle:slackAssistantRequest.created"
            && edge.target.contains("worker.ts#::workerHandler@1")
    }));
    let workflow = workflow_neighborhood(
        &conn,
        "sym:entry.ts#::start@1",
        2,
        20,
        40,
        &origin::defaults(),
    )?;
    assert!(
        workflow
            .nodes
            .iter()
            .any(|node| node.display_name == "enqueueRequest")
    );
    assert!(
        workflow
            .nodes
            .iter()
            .any(|node| node.display_name == "workerHandler")
    );
    assert!(!workflow.truncated);
    Ok(())
}

#[test]
fn workflow_neighborhood_stops_high_degree_code_hubs_without_truncation() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(repo.path(), "shared.ts", "export const shared = () => 1;\n")?;
    write(
        repo.path(),
        "entry.ts",
        "import { shared } from './shared';\n\
         export const entry = () => shared();\n",
    )?;
    for index in 0..13 {
        write(
            repo.path(),
            &format!("caller-{index}.ts"),
            &format!(
                "import {{ shared }} from './shared';\n\
                 export const caller{index} = () => shared();\n"
            ),
        )?;
    }
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let workflow = workflow_neighborhood(
        &conn,
        "sym:entry.ts#::entry@1",
        2,
        100,
        400,
        &origin::defaults(),
    )?;
    assert!(
        workflow
            .nodes
            .iter()
            .any(|node| node.display_name == "shared")
    );
    assert!(
        !workflow
            .nodes
            .iter()
            .any(|node| node.display_name == "caller0"),
        "a high-degree helper is evidence but must not bridge to every caller"
    );
    assert!(!workflow.truncated);
    Ok(())
}

#[test]
fn workflow_neighborhood_rejects_only_high_degree_general_entity_hubs() -> Result<()> {
    let build = |reader_count: usize| -> Result<(i64, super::WorkflowNeighborhood)> {
        let repo = tempfile::tempdir()?;
        for index in 0..reader_count {
            write(
                repo.path(),
                &format!("reader-{index}.ts"),
                &format!("export function reader{index}() {{ return process.env.SHARED_KEY; }}\n"),
            )?;
        }
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let degree: i64 = conn.query_row(
            "SELECT count(*) FROM resolved_edges edge
             JOIN entities entity
               ON entity.entity_key=edge.src_key OR entity.entity_key=edge.dst_key
             WHERE entity.entity_type='environment_variable'
               AND entity.name='SHARED_KEY'",
            [],
            |row| row.get(0),
        )?;
        let workflow = workflow_neighborhood(
            &conn,
            "sym:reader-0.ts#::reader0@1",
            1,
            100,
            400,
            &origin::defaults(),
        )?;
        Ok((degree, workflow))
    };

    let (low_degree, low) = build(5)?;
    assert_eq!(low_degree, 5);
    assert_eq!(low.nodes.len(), 5, "four low-degree peers should be clues");
    assert!(!low.truncated);

    let (high_degree, high) = build(14)?;
    assert_eq!(high_degree, 14);
    assert_eq!(
        high.nodes.len(),
        1,
        "the shared hub must not associate peers"
    );
    assert!(!high.truncated);
    Ok(())
}

#[test]
fn job_and_di_entities_join_producers_handlers_tokens_and_implementations() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "job-name.ts",
        "export class EmailJob {}\nexport class CleanupJob {}\n",
    )?;
    write(
        repo.path(),
        "jobs.ts",
        "import { CleanupJob, EmailJob } from './job-name';\n\
         export class JobConsumer {\n\
           @Process(EmailJob.name) emailHandler(job) { return job.data; }\n\
           @Process(CleanupJob.name) cleanupHandler(job) { return job.data; }\n\
         }\n",
    )?;
    write(
        repo.path(),
        "producer.ts",
        "import { CleanupJob, EmailJob } from './job-name';\n\
         export class Producer {\n\
           enqueueEmail(payload) {\n\
             return this.messageQueueService.add(EmailJob.name, payload);\n\
           }\n\
           scheduleCleanup() {\n\
             return this.messageQueueService.addCron({\n\
               jobName: CleanupJob.name,\n\
               data: undefined,\n\
               options: { repeat: { pattern: '0 0 * * *' } },\n\
             });\n\
           }\n\
         }\n",
    )?;
    write(
        repo.path(),
        "token.ts",
        "export const MAILER = Symbol('mailer');\n",
    )?;
    write(repo.path(), "mailer.ts", "export class MailerService {}\n")?;
    write(
        repo.path(),
        "module.ts",
        "import { MAILER } from './token';\n\
         import { MailerService } from './mailer';\n\
         export const providers = [{ provide: MAILER, useClass: MailerService }];\n",
    )?;
    write(
        repo.path(),
        "consumer.ts",
        "import { MAILER } from './token';\n\
         export class Consumer { constructor(@Inject(MAILER) mailer) {} }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let job = neighborhood(
        &conn,
        "enqueueEmail",
        &NeighborhoodOptions {
            depth: 2,
            direction: "out".into(),
            ..Default::default()
        },
    )?;
    assert!(job.edges.iter().any(|edge| {
        edge.kind == "produces_job"
            && edge.source.contains("producer.ts#Producer::enqueueEmail@1")
            && edge.target.starts_with("entity:job:ref-")
    }));
    assert!(job.edges.iter().any(|edge| {
        edge.kind == "job_handler"
            && edge.source.starts_with("entity:job:ref-")
            && edge.target.contains("jobs.ts#JobConsumer::emailHandler@1")
            && edge.provenance == "registration-site-fallback"
    }));

    let cron = neighborhood(
        &conn,
        "scheduleCleanup",
        &NeighborhoodOptions {
            depth: 2,
            direction: "out".into(),
            ..Default::default()
        },
    )?;
    assert!(cron.edges.iter().any(|edge| {
        edge.kind == "produces_job"
            && edge
                .source
                .contains("producer.ts#Producer::scheduleCleanup@1")
    }));
    assert!(cron.edges.iter().any(|edge| {
        edge.kind == "job_handler"
            && edge
                .target
                .contains("jobs.ts#JobConsumer::cleanupHandler@1")
    }));

    let provider: (String, String) = conn.query_row(
        "SELECT source.node_key, target.node_key
         FROM resolved_edges edge
         JOIN graph_nodes source ON source.node_key=edge.src_key
         JOIN graph_nodes target ON target.node_key=edge.dst_key
         WHERE edge.kind='provides' AND source.node_kind='entity'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert!(provider.0.starts_with("entity:di_token:ref-"));
    assert!(provider.1.contains("mailer.ts#::MailerService@1"));
    let injections: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges
         WHERE kind='injects' AND dst_key=?1",
        [&provider.0],
        |row| row.get(0),
    )?;
    assert_eq!(injections, 1);
    Ok(())
}

#[test]
fn workflow_neighborhood_suppresses_only_inverse_high_degree_di_fanout() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "token.ts",
        "export const MAILER = Symbol('mailer');\n",
    )?;
    write(repo.path(), "mailer.ts", "export class MailerService {}\n")?;
    write(
        repo.path(),
        "module.ts",
        "import { MAILER } from './token';\n\
         import { MailerService } from './mailer';\n\
         export const providers = [{ provide: MAILER, useClass: MailerService }];\n",
    )?;
    for index in 0..15 {
        write(
            repo.path(),
            &format!("consumer-{index}.ts"),
            &format!(
                "import {{ MAILER }} from './token';\n\
                 export class Consumer{index} {{ constructor(@Inject(MAILER) mailer) {{}} }}\n"
            ),
        )?;
    }
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let (entity, provider): (String, String) = conn.query_row(
        "SELECT edge.src_key, edge.dst_key FROM resolved_edges edge
         WHERE edge.kind='provides'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(super::graph_degree(&conn, &entity)?, 16);
    let injector: String = conn.query_row(
        "SELECT edge.src_key FROM resolved_edges edge
         JOIN graph_nodes node ON node.node_key=edge.src_key
         JOIN files file ON file.id=node.file_id
         WHERE edge.kind='injects' AND edge.dst_key=?1
           AND file.path='consumer-0.ts'",
        [&entity],
        |row| row.get(0),
    )?;

    let from_provider = workflow_neighborhood(&conn, &provider, 1, 100, 400, &origin::defaults())?;
    assert_eq!(
        from_provider.nodes.len(),
        1,
        "a common provider must not fan out to every injection site"
    );
    assert!(!from_provider.truncated);

    let from_consumer = workflow_neighborhood(&conn, &injector, 1, 100, 400, &origin::defaults())?;
    assert!(
        from_consumer.nodes.iter().any(|node| node.key == provider),
        "a consumer must still resolve its concrete provider"
    );
    assert_eq!(from_consumer.nodes.len(), 2);
    assert!(!from_consumer.truncated);
    Ok(())
}

#[test]
fn contract_plane_resolves_type_only_barrels_without_runtime_edges() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "contracts.ts",
        "export interface User { id: string }\n\
         export type UserResult = User | null;\n\
         export enum UserState { Active }\n",
    )?;
    write(
        repo.path(),
        "barrel.ts",
        "export type { User, UserResult } from './contracts';\n\
         export { UserState } from './contracts';\n",
    )?;
    write(
        repo.path(),
        "api.ts",
        "import type { User, UserResult } from './barrel';\n\
         export function load(input: User): Promise<UserResult> { throw Error(); }\n\
         export const save = (input: User): UserResult => input;\n\
         export class UserApi { get(input: User): UserResult { return input; } }\n",
    )?;
    write(
        repo.path(),
        "unresolved.ts",
        "import type { Ghost } from './missing-barrel';\n\
         export function haunted(input: Ghost): Ghost { return input; }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let interface = "contract:interface:contracts.ts#User";
    let alias = "contract:type_alias:contracts.ts#UserResult";
    let result = neighborhood(
        &conn,
        "api.ts:load",
        &NeighborhoodOptions {
            depth: 1,
            kinds: vec!["accepts_contract".into(), "returns_contract".into()],
            ..Default::default()
        },
    )?;
    assert!(result.edges.iter().any(|edge| {
        edge.kind == "accepts_contract"
            && edge.target == interface
            && edge.confidence == "certain"
            && edge.detail["documentary"] == true
    }));
    assert!(result.edges.iter().any(|edge| {
        edge.kind == "returns_contract" && edge.target == alias && edge.confidence == "certain"
    }));
    let workflow =
        workflow_neighborhood(&conn, "sym:api.ts#::load@1", 2, 20, 40, &origin::defaults())?;
    assert!(
        !workflow
            .nodes
            .iter()
            .any(|node| node.display_name == "save"),
        "shared contracts are documentary and must not create workflow candidates"
    );
    let contract_file: String = conn.query_row(
        "SELECT files.path FROM graph_nodes
         JOIN files ON files.id=graph_nodes.file_id
         WHERE graph_nodes.node_key=?1",
        [interface],
        |row| row.get(0),
    )?;
    assert_eq!(contract_file, "contracts.ts");
    let alias_reference: i64 = conn.query_row(
        "SELECT EXISTS(
           SELECT 1 FROM resolved_edges
           WHERE src_key=?1 AND dst_key=?2 AND kind='references_contract'
         )",
        rusqlite::params![alias, interface],
        |row| row.get(0),
    )?;
    assert_eq!(alias_reference, 1);
    let runtime_type_edges: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges
         WHERE source_ref_id IN (
           SELECT id FROM refs WHERE file_id=(SELECT id FROM files WHERE path='api.ts')
         ) AND dst_key IN (?1, ?2)",
        rusqlite::params![interface, alias],
        |row| row.get(0),
    )?;
    assert_eq!(runtime_type_edges, 0);
    let type_only_module: (i64, i64, i64) = conn.query_row(
        "SELECT edge.type_only,
                EXISTS(
                  SELECT 1 FROM resolved_edges projected
                  WHERE projected.src_key='file:api.ts'
                    AND projected.dst_key='file:barrel.ts'
                    AND projected.kind='imports_types'
                    AND projected.provenance='type-resolver'
                ),
                EXISTS(
                  SELECT 1 FROM resolved_edges projected
                  WHERE projected.src_key='file:api.ts'
                    AND projected.dst_key='file:barrel.ts'
                    AND projected.kind='import'
                )
         FROM module_edges edge
         JOIN files source ON source.id=edge.from_file
         WHERE source.path='api.ts' AND edge.request='./barrel'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(type_only_module, (1, 1, 0));
    let unresolved_contract: (String, Option<String>) = conn.query_row(
        "SELECT entity_key, identity_anchor FROM entities
         WHERE plane='contract' AND name='Ghost'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(
        unresolved_contract,
        (
            "contract:reference:unresolved:./missing-barrel#Ghost".into(),
            None
        )
    );
    let enum_count: i64 = conn.query_row(
        "SELECT count(*) FROM entities
         WHERE plane='contract' AND entity_type='enum' AND name='UserState'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(enum_count, 1);
    Ok(())
}

#[test]
fn general_entities_project_endpoint_configuration_data_and_host_edges() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "app.ts",
        "export function createJob() {}\n\
         router.post('/jobs', createJob);\n\
         @Controller('/users')\n\
         export class UsersController {\n\
           @Get(':id') getUser() {}\n\
           @Query('user') user() {}\n\
         }\n\
         export function run() {\n\
           process.env.API_KEY;\n\
           config.get('database.host');\n\
           prisma.user.findMany();\n\
           prisma.user.create({ data: {} });\n\
           getRepository(User);\n\
           flags.isEnabled('new-ui');\n\
           fetch('https://api.example.com/v1');\n\
           client.query({ currentUser: { id: true } });\n\
         }\n\
         export function followup() {\n\
           client.query({ currentUser: { id: true } });\n\
         }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    for (entity_type, name, kind) in [
        ("route", "POST /jobs", "handles_route"),
        ("route", "GET /users/:id", "handles_route"),
        ("graphql_operation", "query:user", "handles_graphql"),
        ("graphql_operation", "query:currentUser", "invokes_graphql"),
        ("environment_variable", "API_KEY", "reads_env"),
        ("config_key", "database.host", "reads_config"),
        ("database_resource", "user", "reads_resource"),
        ("database_resource", "user", "writes_resource"),
        ("database_resource", "User", "acquires_resource"),
        ("feature_flag", "new-ui", "checks_flag"),
        ("external_host", "api.example.com", "calls_host"),
    ] {
        let edge_count: i64 = conn.query_row(
            "SELECT count(*) FROM resolved_edges edge
             JOIN entities entity
               ON entity.entity_key=edge.src_key OR entity.entity_key=edge.dst_key
             WHERE entity.plane='general' AND entity.entity_type=?1
               AND entity.name=?2 AND edge.kind=?3",
            rusqlite::params![entity_type, name, kind],
            |row| row.get(0),
        )?;
        assert!(edge_count > 0, "missing {entity_type}/{name}/{kind}");
    }
    let handler_target: String = conn.query_row(
        "SELECT target.display_name FROM resolved_edges edge
         JOIN entities route ON route.entity_key=edge.src_key
         JOIN graph_nodes target ON target.node_key=edge.dst_key
         WHERE route.plane='general' AND route.entity_type='route'
           AND route.name='GET /users/:id' AND edge.kind='handles_route'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(handler_target, "getUser");
    let workflow =
        workflow_neighborhood(&conn, "sym:app.ts#::run@1", 1, 20, 40, &origin::defaults())?;
    assert!(
        workflow
            .nodes
            .iter()
            .any(|node| node.display_name == "followup")
    );
    Ok(())
}

#[test]
fn inline_route_handlers_do_not_attach_to_the_next_declaration() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "app.ts",
        "export function listUsers() {}\n\
         usersRouter.get('/users', listUsers);\n\
         router.post('/webhooks', async (request) => request.body);\n\
         export function unrelatedNearbyFunction() {}\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let named_handler: String = conn.query_row(
        "SELECT target.display_name FROM resolved_edges edge
         JOIN entities route ON route.entity_key=edge.src_key
         JOIN graph_nodes target ON target.node_key=edge.dst_key
         WHERE route.plane='general' AND route.entity_type='route'
           AND route.name='GET /users' AND edge.kind='handles_route'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(named_handler, "listUsers");
    let inline_occurrences: i64 = conn.query_row(
        "SELECT count(*) FROM entity_occurrences occurrence
         JOIN entities route ON route.id=occurrence.entity_id
         WHERE route.plane='general' AND route.entity_type='route'
           AND route.name='POST /webhooks'",
        [],
        |row| row.get(0),
    )?;
    let inline_handler_edges: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges edge
         JOIN entities route ON route.entity_key=edge.src_key
         WHERE route.plane='general' AND route.entity_type='route'
           AND route.name='POST /webhooks' AND edge.kind='handles_route'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(inline_occurrences, 1);
    assert_eq!(inline_handler_edges, 0);
    Ok(())
}

#[test]
fn scopes_same_named_methods_by_class() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "models.ts",
        "class Alpha { ping() {} }\nclass Beta { ping() {} }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let keys: Vec<String> = {
        let mut stmt = conn.prepare(
            "SELECT node_key FROM graph_nodes
             WHERE node_kind='symbol' AND display_name='ping' ORDER BY node_key",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        rows.collect::<std::result::Result<_, _>>()?
    };
    assert_eq!(keys.len(), 2);
    assert!(keys.iter().any(|key| key.contains("#Alpha::ping@1")));
    assert!(keys.iter().any(|key| key.contains("#Beta::ping@1")));
    assert!(neighborhood(&conn, "ping", &NeighborhoodOptions::default()).is_err());
    Ok(())
}

#[test]
fn truncation_keeps_higher_ranked_candidates_not_sql_order() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(repo.path(), "root.ts", "export const root = 1;\n")?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    for (key, name) in [("candidate:a-low", "a-low"), ("candidate:z-high", "z-high")] {
        conn.execute(
            "INSERT INTO graph_nodes(node_key, node_kind, display_name, meta_json)
             VALUES(?1, 'candidate', ?2, '{}')",
            rusqlite::params![key, name],
        )?;
    }
    conn.execute(
        "INSERT INTO resolved_edges(
           src_key, dst_key, kind, confidence, provenance, detail_json
         ) VALUES('file:root.ts', 'candidate:a-low', 'call', 'possible', 'test', '{}')",
        [],
    )?;
    conn.execute(
        "INSERT INTO resolved_edges(
           src_key, dst_key, kind, confidence, provenance, detail_json
         ) VALUES('file:root.ts', 'candidate:z-high', 'import', 'certain', 'test', '{}')",
        [],
    )?;

    let result = neighborhood(
        &conn,
        "file:root.ts",
        &NeighborhoodOptions {
            depth: 1,
            direction: "out".into(),
            node_limit: 2,
            edge_limit: 2,
            min_confidence: "possible".into(),
            kinds: Vec::new(),
            expected_snapshot: None,
            file_roles: Vec::new(),
            file_origins: origin::defaults(),
            penalize_file_roles: false,
        },
    )?;
    assert!(result.truncated);
    assert!(
        result
            .nodes
            .iter()
            .any(|node| node.key == "candidate:z-high")
    );
    assert!(
        !result
            .nodes
            .iter()
            .any(|node| node.key == "candidate:a-low")
    );
    assert_eq!(result.edges.len(), 1);
    assert_eq!(result.edges[0].kind, "import");
    assert!(result.edges[0].relevance > 0.0);
    Ok(())
}

#[test]
fn ambiguous_root_reference_projects_every_candidate_as_possible() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "ambiguous.js",
        "function target() {}\nfunction run() { target(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let file_id: i64 =
        conn.query_row("SELECT id FROM files WHERE path='ambiguous.js'", [], |r| {
            r.get(0)
        })?;
    conn.execute(
        "INSERT INTO symbols(
           file_id, name, kind, start, end, decl_start, decl_end,
           scope_chain, line, exported
         ) VALUES(?1, 'target', 'function', 0, 20, 0, 20, '', 1, 0)",
        [file_id],
    )?;
    let snapshot = compute_snapshot(&conn)?;
    rebuild_projection(&conn, &snapshot)?;

    let result = neighborhood(
        &conn,
        "ambiguous.js:run",
        &NeighborhoodOptions {
            direction: "out".into(),
            min_confidence: "possible".into(),
            ..Default::default()
        },
    )?;
    let candidates: Vec<_> = result
        .edges
        .iter()
        .filter(|edge| edge.kind == "call" && edge.target.contains("::target@"))
        .collect();
    assert_eq!(candidates.len(), 2);
    assert!(candidates.iter().all(|edge| edge.confidence == "possible"));
    assert!(candidates.iter().all(|edge| {
        edge.detail["ambiguousTarget"] == true && edge.detail["candidateCount"] == 2
    }));
    Ok(())
}

#[test]
fn possible_member_calls_traverse_through_candidate_hubs() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "service.ts",
        "class Service { load() {} }\nfunction run(client) { client.load(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let default_result = neighborhood(
        &conn,
        "service.ts:load",
        &NeighborhoodOptions {
            depth: 2,
            direction: "both".into(),
            ..Default::default()
        },
    )?;
    assert!(
        !default_result
            .edges
            .iter()
            .any(|edge| { edge.kind == "member_call" || edge.kind == "member_candidate" })
    );

    let result = neighborhood(
        &conn,
        "service.ts:load",
        &NeighborhoodOptions {
            depth: 2,
            direction: "both".into(),
            min_confidence: "possible".into(),
            ..Default::default()
        },
    )?;
    assert!(result.edges.iter().any(|edge| {
        edge.kind == "member_candidate"
            && edge.source == "member:unknown:load"
            && edge.target.contains("#Service::load@1")
    }));
    assert!(result.edges.iter().any(|edge| {
        edge.kind == "member_call"
            && edge.source.contains("#::run@1")
            && edge.target == "member:unknown:load"
            && edge.confidence == "possible"
    }));
    Ok(())
}

#[test]
fn checker_facts_project_per_occurrence_without_replacing_member_hubs() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "service.ts",
        "class Service { load() {} }\nfunction run(client: Service) { client.load(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = super::current_snapshot(&conn)?;
    let (
        member_call_id,
        source_file_id,
        source_hash,
        call_start,
        call_end,
        receiver_start,
        receiver_end,
        property_start,
        property_end,
    ) = conn.query_row(
        "SELECT call.rowid, file.id, file.hash, call.start, call.end,
                call.receiver_start, call.receiver_end,
                call.property_start, call.property_end
         FROM member_calls call JOIN files file ON file.id=call.file_id
         WHERE call.prop='load'",
        [],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
            ))
        },
    )?;
    let (target, target_start, target_end): (String, i64, i64) = conn.query_row(
        "SELECT node.node_key, symbol.decl_start, symbol.decl_end
         FROM graph_nodes node JOIN symbols symbol
           ON node.native_table='symbols' AND node.native_id=symbol.id
         WHERE node.display_name='load'",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let target_fingerprint =
        crate::checker::target_fingerprint(&target, &source_hash, target_start, target_end);
    conn.execute(
        "INSERT INTO checker_enrichment_batches(
           source_snapshot, checker_version, checker_source,
           checker_input_fingerprint, sidecar_protocol, created_at, active
         ) VALUES(?1,'5.9.3','bundled','inputs',1,datetime('now'),1)",
        [&snapshot],
    )?;
    let batch_id = conn.last_insert_rowid();
    for (project, fingerprint) in [
        ("tsconfig.json", "inputs"),
        ("tsconfig.stray.json", "stray-inputs"),
    ] {
        conn.execute(
            "INSERT INTO checker_project_runs(
               batch_id, project_id, status, selected_occurrences,
               completed_occurrences, checker_input_fingerprint, updated_at
             ) VALUES(?1,?2,'completed',1,1,?3,datetime('now'))",
            rusqlite::params![batch_id, project, fingerprint],
        )?;
    }
    conn.execute(
        "INSERT INTO checker_enrichments(
           batch_id, member_call_id, source_file_id, source_file, source_hash,
           call_start, call_end, receiver_start, receiver_end,
           property_start, property_end, project_id, receiver_type,
           target_anchor, target_fingerprint, confidence, provenance,
           checker_input_fingerprint
         ) VALUES(
           ?1,?2,?3,'service.ts',?4,?5,?6,?7,?8,?9,?10,
           'tsconfig.json','Service',?11,?12,'likely','checker','inputs'
         )",
        rusqlite::params![
            batch_id,
            member_call_id,
            source_file_id,
            source_hash,
            call_start,
            call_end,
            receiver_start,
            receiver_end,
            property_start,
            property_end,
            target,
            target_fingerprint,
        ],
    )?;
    conn.execute(
        "INSERT INTO checker_occurrence_projects(
           batch_id, member_call_id, source_file, source_hash,
           call_start, call_end, receiver_start, receiver_end,
           property_start, property_end, project_id,
           checker_input_fingerprint, status
         ) VALUES(?1,?2,'service.ts',?3,?4,?5,?6,?7,?8,?9,
                  'tsconfig.json','inputs','resolved')",
        rusqlite::params![
            batch_id,
            member_call_id,
            source_hash,
            call_start,
            call_end,
            receiver_start,
            receiver_end,
            property_start,
            property_end,
        ],
    )?;
    conn.execute(
        "INSERT INTO checker_occurrence_projects(
           batch_id, member_call_id, source_file, source_hash,
           call_start, call_end, receiver_start, receiver_end,
           property_start, property_end, project_id,
           checker_input_fingerprint, status
         ) VALUES(?1,?2,'service.ts',?3,?4,?5,?6,?7,?8,?9,
                  'tsconfig.stray.json','stray-inputs','unknown')",
        rusqlite::params![
            batch_id,
            member_call_id,
            source_hash,
            call_start,
            call_end,
            receiver_start,
            receiver_end,
            property_start,
            property_end,
        ],
    )?;
    rebuild_projection(&conn, &snapshot)?;

    let checker_edges: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges
         WHERE kind='member_call' AND provenance='checker'
           AND confidence='likely' AND dst_key=?1",
        [&target],
        |row| row.get(0),
    )?;
    let hub_edges: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges
         WHERE kind='member_call' AND provenance='member-name-match'
           AND dst_key='member:unknown:load'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(checker_edges, 1);
    assert_eq!(hub_edges, 1);

    // Canonical facts must be explicitly rebound to the current
    // member_calls row before projection. Source identity and spans remain
    // defense in depth, but cannot authorize a stale rowid by themselves.
    let retained_member_call_id = member_call_id + 10_000;
    conn.execute(
        "UPDATE checker_enrichments SET member_call_id=?1 WHERE batch_id=?2",
        rusqlite::params![retained_member_call_id, batch_id],
    )?;
    conn.execute(
        "UPDATE checker_occurrence_projects SET member_call_id=?1 WHERE batch_id=?2",
        rusqlite::params![retained_member_call_id, batch_id],
    )?;
    rebuild_projection(&conn, &snapshot)?;
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM resolved_edges
             WHERE kind='member_call' AND provenance='checker' AND dst_key=?1",
            [&target],
            |row| row.get::<_, i64>(0),
        )?,
        0
    );

    conn.execute(
        "UPDATE checker_enrichments SET member_call_id=?1 WHERE batch_id=?2",
        rusqlite::params![member_call_id, batch_id],
    )?;
    conn.execute(
        "UPDATE checker_occurrence_projects SET member_call_id=?1 WHERE batch_id=?2",
        rusqlite::params![member_call_id, batch_id],
    )?;
    rebuild_projection(&conn, &snapshot)?;

    let (checker_source, checker_detail): (String, String) = conn.query_row(
        "SELECT src_key, detail_json FROM resolved_edges
         WHERE kind='member_call' AND provenance='checker' AND dst_key=?1",
        [&target],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let checker_detail: serde_json::Value = serde_json::from_str(&checker_detail)?;
    assert_eq!(
        checker_detail["unknownProjects"],
        serde_json::json!(["tsconfig.stray.json"]),
        "unknown owning projects stay visible without demoting the clean resolution"
    );
    assert_eq!(checker_detail["candidateCount"], 1);
    conn.execute(
        "UPDATE checker_project_runs SET status='failed'
         WHERE batch_id=?1 AND project_id='tsconfig.stray.json'",
        [batch_id],
    )?;
    rebuild_projection(&conn, &snapshot)?;
    let (failed_confidence, failed_detail): (String, String) = conn.query_row(
        "SELECT confidence, detail_json FROM resolved_edges
         WHERE kind='member_call' AND provenance='checker' AND dst_key=?1",
        [&target],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!(failed_confidence, "possible");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&failed_detail)?["failedProjects"],
        serde_json::json!(["tsconfig.stray.json"])
    );
    conn.execute(
        "UPDATE checker_project_runs SET status='completed'
         WHERE batch_id=?1 AND project_id='tsconfig.stray.json'",
        [batch_id],
    )?;
    rebuild_projection(&conn, &snapshot)?;
    let workflow = workflow_neighborhood(&conn, &checker_source, 1, 20, 40, &origin::defaults())?;
    assert!(
        workflow.nodes.iter().any(|node| node.key == target),
        "a likely checker-resolved member call must participate in workflow discovery"
    );
    super::clear_checker_plane(&conn)?;
    let cleared: (i64, i64, i64) = conn.query_row(
        "SELECT
           (SELECT count(*) FROM checker_enrichment_batches),
           (SELECT count(*) FROM resolved_edges WHERE provenance='checker'),
           (SELECT count(*) FROM resolved_edges
              WHERE provenance='member-name-match'
                AND dst_key='member:unknown:load')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    assert_eq!(cleared, (0, 0, 1));
    Ok(())
}

/// Checker facts are one disposable plane, not independently freshened
/// project fragments. A structural snapshot change suppresses the entire
/// old batch until enrichment publishes a batch for the new snapshot.
#[test]
fn checker_batch_is_removed_after_snapshot_changes() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "tables.ts",
        "export class CardTable { insert() {} }\n\
         export class OtherTable { insert() {} }\n",
    )?;
    for side in ["left", "right"] {
        write(
            repo.path(),
            &format!("{side}-types.ts"),
            "import { CardTable } from './tables';\n\
             export type Selected = CardTable;\n",
        )?;
        write(
            repo.path(),
            &format!("{side}.ts"),
            &format!(
                "import {{ Selected }} from './{side}-types';\n\
                 export function run(table: Selected) {{ table.insert(); }}\n"
            ),
        )?;
    }
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let snapshot = super::current_snapshot(&conn)?;
    let (target, target_hash, target_start, target_end): (String, String, i64, i64) = conn
        .query_row(
            "SELECT node.node_key, file.hash, symbol.decl_start, symbol.decl_end
             FROM graph_nodes node
             JOIN symbols symbol
               ON node.native_table='symbols' AND node.native_id=symbol.id
             JOIN files file ON file.id=symbol.file_id
             WHERE node.node_key LIKE '%CardTable::insert@%'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
    let fingerprint =
        crate::checker::target_fingerprint(&target, &target_hash, target_start, target_end);
    conn.execute(
        "INSERT INTO checker_enrichment_batches(
           source_snapshot, checker_version, checker_source,
           checker_input_fingerprint, sidecar_protocol, created_at, active
         ) VALUES(?1,'5.9.3','bundled','inputs',1,datetime('now'),1)",
        [&snapshot],
    )?;
    let batch_id = conn.last_insert_rowid();
    for side in ["left", "right"] {
        let project = format!("tsconfig.{side}.json");
        let query_file = format!("{side}.ts");
        let input_fingerprint = format!("{side}-inputs");
        conn.execute(
            "INSERT INTO checker_project_runs(
               batch_id, project_id, status, selected_occurrences,
               completed_occurrences, checker_input_fingerprint, updated_at
             ) VALUES(?1,?2,'completed',1,1,?3,datetime('now'))",
            rusqlite::params![batch_id, &project, &input_fingerprint],
        )?;
        let (call, file_id, hash, spans): (i64, i64, String, [i64; 6]) = conn.query_row(
            "SELECT call.rowid, file.id, file.hash, call.start, call.end,
                        call.receiver_start, call.receiver_end,
                        call.property_start, call.property_end
                 FROM member_calls call JOIN files file ON file.id=call.file_id
                 WHERE file.path=?1 AND call.prop='insert'",
            [&query_file],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    [
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                        row.get(7)?,
                        row.get(8)?,
                    ],
                ))
            },
        )?;
        conn.execute(
            "INSERT INTO checker_occurrence_projects(
               batch_id, member_call_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id,
               checker_input_fingerprint, status
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,'resolved')",
            rusqlite::params![
                batch_id,
                call,
                &query_file,
                &hash,
                spans[0],
                spans[1],
                spans[2],
                spans[3],
                spans[4],
                spans[5],
                &project,
                &input_fingerprint,
            ],
        )?;
        conn.execute(
            "INSERT INTO checker_enrichments(
               batch_id, member_call_id, source_file_id, source_file, source_hash,
               call_start, call_end, receiver_start, receiver_end,
               property_start, property_end, project_id, receiver_type,
               target_anchor, target_fingerprint, confidence, provenance,
               checker_input_fingerprint
             ) VALUES(
               ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,
               ?12,'CardTable',?13,?14,'likely','checker',?15
             )",
            rusqlite::params![
                batch_id,
                call,
                file_id,
                query_file,
                hash,
                spans[0],
                spans[1],
                spans[2],
                spans[3],
                spans[4],
                spans[5],
                project,
                target,
                fingerprint,
                input_fingerprint,
            ],
        )?;
    }
    rebuild_projection(&conn, &snapshot)?;
    let projected = |conn: &rusqlite::Connection| -> Result<Vec<String>> {
        let mut statement = conn.prepare(
            "SELECT file.path FROM resolved_edges edge
             JOIN files file ON file.id=edge.source_file_id
             WHERE edge.kind='member_call' AND edge.provenance='checker'
             ORDER BY file.path",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<_, _>>()?)
    };
    assert_eq!(projected(&conn)?, vec!["left.ts", "right.ts"]);

    write(
        repo.path(),
        "left-types.ts",
        "import { OtherTable } from './tables';\n\
         export type Selected = OtherTable;\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    assert!(
        projected(&conn)?.is_empty(),
        "an old-snapshot checker batch must not project partial survivors"
    );
    let retained: i64 = conn.query_row(
        "SELECT count(*) FROM checker_enrichments WHERE batch_id=?1",
        [batch_id],
        |row| row.get(0),
    )?;
    assert_eq!(
        retained, 0,
        "every refresh mode retires checker facts from an older snapshot"
    );
    Ok(())
}

#[test]
fn receiver_value_flow_resolves_this_new_and_one_hop_inheritance() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "base.ts",
        "export class Base { inherited() {} staticOnly() {} shadowed() {} field() {} }\n",
    )?;
    write(
        repo.path(),
        "main.ts",
        "import { Base } from './base';\n\
         class Child extends Base {\n\
           own() {}\n\
           static staticOnly() {}\n\
           get accessor() { return () => {}; }\n\
           get shadowed() { return () => {}; }\n\
           field = null;\n\
           run() {\n\
             this.own();\n\
             this.inherited();\n\
             const arrow = () => this.own();\n\
             function nested() { this.own(); }\n\
             arrow(); nested();\n\
           }\n\
         }\n\
         const child = new Child();\n\
         child.own();\n\
         new Child().own();\n\
         child.staticOnly();\n\
         child.accessor();\n\
         child.shadowed();\n\
         child.field();\n\
         let mutable = new Child();\n\
         mutable.own();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let mut statement = conn.prepare(
        "SELECT target.display_name, target.meta_json, edge.detail_json
         FROM resolved_edges edge
         JOIN graph_nodes target ON target.node_key=edge.dst_key
         WHERE edge.provenance='receiver-value-flow'
         ORDER BY edge.line, edge.id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        rows.len(),
        6,
        "nested-function `this`, non-instance members, and `let` must give up"
    );
    assert_eq!(
        rows.iter().map(|row| row.0.as_str()).collect::<Vec<_>>(),
        ["own", "inherited", "own", "own", "own", "staticOnly"]
    );
    let inherited_meta: serde_json::Value = serde_json::from_str(&rows[1].1)?;
    assert_eq!(inherited_meta["path"], "base.ts");
    let inherited_static_name_meta: serde_json::Value = serde_json::from_str(&rows[5].1)?;
    assert_eq!(inherited_static_name_meta["path"], "base.ts");
    for (_, _, detail) in rows {
        let detail: serde_json::Value = serde_json::from_str(&detail)?;
        assert_eq!(detail["candidateCount"], 1);
        assert_eq!(detail["occurrenceSpecific"], true);
    }
    Ok(())
}

#[test]
fn receiver_value_flow_closes_factory_returns_through_depth_two() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "adapters.ts",
        "export class PgAdapter { save() {} }\n\
         export class SqliteAdapter { save() {} }\n",
    )?;
    write(
        repo.path(),
        "factories.ts",
        "import { PgAdapter, SqliteAdapter } from './adapters';\n\
         export const createPg = () => new PgAdapter();\n\
         export function createSqlite() { const db = new SqliteAdapter(); return db; }\n\
         export function openDatabase(kind) {\n\
           if (kind) return createPg();\n\
           return createSqlite();\n\
         }\n\
         export function layerTwo() { return openDatabase('sqlite'); }\n\
         export function layerThree() { return layerTwo(); }\n\
         export async function freshDb() { return openDatabase('sqlite'); }\n\
         export function conditional(kind) {\n\
           return kind ? new PgAdapter() : new SqliteAdapter();\n\
         }\n\
         export function incomplete(kind, value) {\n\
           if (kind) return new PgAdapter();\n\
           return value;\n\
         }\n\
         export function fallthrough(kind) {\n\
           if (kind) return new PgAdapter();\n\
         }\n",
    )?;
    write(
        repo.path(),
        "main.ts",
        "import { openDatabase, layerTwo, layerThree, freshDb, conditional, incomplete, fallthrough } from './factories';\n\
         const direct = openDatabase('pg'); direct.save();\n\
         const bounded = layerTwo(); bounded.save();\n\
         const awaited = await freshDb(); awaited.save();\n\
         const promise = freshDb(); promise.save();\n\
         const deep = layerThree(); deep.save();\n\
         const dynamic = conditional(true); dynamic.save();\n\
         const partial = incomplete(true, null); partial.save();\n\
         const missing = fallthrough(true); missing.save();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let mut statement = conn.prepare(
        "SELECT edge.line, target.display_name, edge.confidence, edge.detail_json
         FROM resolved_edges edge
         JOIN graph_nodes target ON target.node_key=edge.dst_key
         WHERE edge.provenance='receiver-value-flow'
         ORDER BY edge.line, target.display_name",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        rows.len(),
        4,
        "only the two synchronous bounded calls have two targets each"
    );
    assert_eq!(
        rows.iter().map(|row| row.1.as_str()).collect::<Vec<_>>(),
        ["save", "save", "save", "save"]
    );
    assert!(rows.iter().all(|row| row.2 == "likely"));
    let mut flows = Vec::new();
    for (_, _, _, detail) in rows {
        let detail: serde_json::Value = serde_json::from_str(&detail)?;
        flows.push(detail["flow"].as_str().unwrap_or_default().to_string());
        assert_eq!(detail["candidateCount"], 2);
        assert_eq!(detail["receiverClasses"].as_array().map(Vec::len), Some(2));
    }
    assert_eq!(flows, ["factory", "factory", "factory", "factory"]);
    Ok(())
}

#[test]
fn receiver_value_flow_rejects_awaited_thenable_assimilation() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "main.ts",
        "class Replacement { run() {} }\n\
         class Thenable {\n\
           run() {}\n\
           then(resolve: (value: Replacement) => void) { resolve(new Replacement()); }\n\
         }\n\
         async function make() { return new Thenable(); }\n\
         (await new Thenable()).run();\n\
         (await make()).run();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resolved_edges WHERE provenance='receiver-value-flow'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn receiver_value_flow_rejects_dynamic_with_scope() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "main.js",
        "class A { run() {} }\n\
         class B { run() {} }\n\
         with ({ A: B }) { new A().run(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resolved_edges WHERE provenance='receiver-value-flow'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn receiver_value_flow_rejects_eval_scope_mutation() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "main.js",
        "class A { run() {} }\n\
         class B { run() {} }\n\
         function make() { return new A(); }\n\
         (0, eval)(\"make = function() { return new B(); }\");\n\
         make().run();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resolved_edges WHERE provenance='receiver-value-flow'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn receiver_value_flow_rejects_sloppy_block_factory_redeclaration() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "main.js",
        "class A { run() {} }\n\
         class B { run() {} }\n\
         function make() { return new A(); }\n\
         if (true) { function make() { return new B(); } }\n\
         make().run();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM resolved_edges WHERE provenance='receiver-value-flow'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    Ok(())
}

#[test]
fn receiver_value_flow_follows_exported_const_values() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "adapter.ts",
        "export class Adapter { save() {} }\n",
    )?;
    write(
        repo.path(),
        "singleton.ts",
        "import { Adapter } from './adapter';\n\
         export const singleton = new Adapter();\n\
         export function current() { return singleton; }\n",
    )?;
    write(
        repo.path(),
        "main.ts",
        "import { singleton, current } from './singleton';\n\
         singleton.save();\n\
         const alias = singleton; alias.save();\n\
         const fromFactory = current(); fromFactory.save();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let rows = conn
        .prepare(
            "SELECT edge.confidence, edge.detail_json
             FROM resolved_edges edge
             WHERE edge.provenance='receiver-value-flow'
             ORDER BY edge.line, edge.id",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(rows.len(), 3);
    assert!(rows.iter().all(|row| row.0 == "likely"));
    let flows = rows
        .iter()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(&row.1)
                .map(|detail| detail["flow"].as_str().unwrap_or_default().to_string())
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(flows, ["binding", "binding", "factory"]);
    Ok(())
}

#[test]
fn receiver_value_flow_accepts_three_classes_and_rejects_four() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "classes.ts",
        "export class A { run() {} }\n\
         export class B { run() {} }\n\
         export class C { run() {} }\n\
         export class D { run() {} }\n",
    )?;
    write(
        repo.path(),
        "factories.ts",
        "import { A, B, C, D } from './classes';\n\
         export function three(value) {\n\
           if (value === 1) return new A();\n\
           if (value === 2) return new B();\n\
           return new C();\n\
         }\n\
         export function four(value) {\n\
           if (value === 1) return new A();\n\
           if (value === 2) return new B();\n\
           if (value === 3) return new C();\n\
           return new D();\n\
         }\n",
    )?;
    write(
        repo.path(),
        "main.ts",
        "import { three, four } from './factories';\n\
         const accepted = three(1); accepted.run();\n\
         const rejected = four(1); rejected.run();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let rows = conn
        .prepare(
            "SELECT target.display_name, edge.detail_json
             FROM resolved_edges edge
             JOIN graph_nodes target ON target.node_key=edge.dst_key
             WHERE edge.provenance='receiver-value-flow'
             ORDER BY target.node_key",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(rows.len(), 3, "the four-class receiver must give up");
    assert!(rows.iter().all(|row| row.0 == "run"));
    for (_, detail) in rows {
        let detail: serde_json::Value = serde_json::from_str(&detail)?;
        assert_eq!(detail["candidateCount"], 3);
        assert_eq!(detail["receiverClasses"].as_array().map(Vec::len), Some(3));
    }
    Ok(())
}

#[test]
fn receiver_value_flow_rejects_unstable_or_non_identity_preserving_bindings() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "main.ts",
        "class RootService { run() {} }\n\
         class Replacement { run() {} }\n\
         const service = new RootService();\n\
         function parameter(service) { return service; }\n\
         function localLet() { let service = new Replacement(); return service; }\n\
         function localClass() { class RootService { run() {} } return new RootService(); }\n\
         function make() { return new RootService(); }\n\
         function nestedFactory() { function make() { return new Replacement(); } return make(); }\n\
         function mutableFactory() { return new RootService(); }\n\
         mutableFactory = () => new Replacement();\n\
         class MutableClass { run() {} }\n\
         MutableClass = Replacement;\n\
         function makeContainer() { return new RootService(); }\n\
         const { picked } = makeContainer();\n\
         const [first] = makeContainer();\n\
         function optionalFactory() { return new RootService(); }\n\
         const optional = optionalFactory?.();\n\
         const overwritten = new RootService();\n\
         overwritten.run = () => {};\n\
         parameter(service).run();\n\
         localLet().run();\n\
         localClass().run();\n\
         nestedFactory().run();\n\
         mutableFactory().run();\n\
         new MutableClass().run();\n\
         picked.run();\n\
         first.run();\n\
         optional.run();\n\
         overwritten.run();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let projected: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges WHERE provenance='receiver-value-flow'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        projected, 0,
        "shadowing, reassignment, destructuring, and optional calls must remain on hubs"
    );
    Ok(())
}

#[test]
fn receiver_value_flow_rejects_constructor_decorator_and_static_property_mutation() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "main.ts",
        "function decorate(value: unknown) { return value; }\n\
         class Safe { run() {} }\n\
         class Replacement { run() {} }\n\
         class Base { run() {} }\n\
         class Returning { constructor() { return new Replacement(); } run() {} }\n\
         class ReturningBase { constructor() { return new Replacement(); } }\n\
         class ReturningChild extends ReturningBase { run() {} }\n\
         function dynamicBase() { return class { constructor() { return new Replacement(); } }; }\n\
         class DynamicChild extends dynamicBase() { run() {} }\n\
         @decorate class DecoratedClass { run() {} }\n\
         class DecoratedMethod { @decorate run() {} }\n\
         class DecoratedElsewhere { run() {} @decorate other() {} }\n\
         class DecoratedParameter { run() {} constructor(@decorate value: unknown) {} }\n\
         class DecoratedProperty { run() {} @decorate other = 1; }\n\
         class ComputedWritten { run() {} constructor() { this['run'] = () => {}; } }\n\
         class DynamicWritten { run() {} constructor(key: string) { this[key] = () => {}; } }\n\
         class ParameterProperty extends Base { constructor(public run: () => void) { super(); } }\n\
         class InitializingBase { constructor() { this.run = () => {}; } }\n\
         class DerivedOwn extends InitializingBase { run() {} }\n\
         class FieldBase { run = () => {}; }\n\
         class DerivedFieldOwn extends FieldBase { run() {} }\n\
         const computedKey = 'run'; class ComputedOverride extends Base { [computedKey]() {} }\n\
         function duplicateFactory() { return new Safe(); }\n\
         function duplicateFactory() { return new Replacement(); }\n\
         function mixedFactory() { return new Safe(); }\n\
         var mixedFactory = function() { return new Replacement(); };\n\
         function destructuredFactory() { return new Safe(); }\n\
         var { destructuredFactory } = { destructuredFactory: function() { return new Replacement(); } };\n\
         const computed = new Safe(); computed['run'] = () => {};\n\
         const dynamic = new Safe(); const key = 'run'; dynamic[key] = () => {};\n\
         const deleted = new Safe(); delete deleted.run;\n\
         const updated = new Safe(); updated.run++;\n\
         const iterated = new Safe(); for (iterated.run of [() => {}]) {}\n\
         const destructured = new Safe(); [destructured.run] = [() => {}];\n\
         function overwrittenFactory() { const value = new Safe(); value.run = () => {}; return value; }\n\
         new Safe().run();\n\
         new Returning().run();\n\
         new ReturningChild().run();\n\
         new DynamicChild().run();\n\
         new DecoratedClass().run();\n\
         new DecoratedMethod().run();\n\
         new DecoratedElsewhere().run();\n\
         new DecoratedParameter(1).run();\n\
         new DecoratedProperty().run();\n\
         new ComputedWritten().run();\n\
         new DynamicWritten('run').run();\n\
         new ParameterProperty(() => {}).run();\n\
         new DerivedOwn().run();\n\
         new DerivedFieldOwn().run();\n\
         new ComputedOverride().run();\n\
         duplicateFactory().run();\n\
         mixedFactory().run();\n\
         destructuredFactory().run();\n\
         computed.run();\n\
         dynamic.run();\n\
         deleted.run();\n\
         updated.run();\n\
         iterated.run();\n\
         destructured.run();\n\
         overwrittenFactory().run();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let rows = conn
        .prepare(
            "SELECT target.display_name, edge.detail_json
             FROM resolved_edges edge
             JOIN graph_nodes target ON target.node_key=edge.dst_key
             WHERE edge.provenance='receiver-value-flow'
             ORDER BY edge.line, edge.id",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0, "run");
    let detail: serde_json::Value = serde_json::from_str(&rows[0].1)?;
    assert_eq!(
        detail["receiverClasses"],
        serde_json::json!(["main.ts#Safe"])
    );
    Ok(())
}

#[test]
fn receiver_value_flow_accepts_namespace_factories_but_not_object_members() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "factory.ts",
        "export class A { run() {} }\n\
         export class B { run() {} }\n\
         export function make() { return new A(); }\n",
    )?;
    write(
        repo.path(),
        "main.ts",
        "import * as ns from './factory';\n\
         import { A, B } from './factory';\n\
         function holder() { return new A(); }\n\
         holder.make = () => new B();\n\
         const exact = ns.make();\n\
         exact.run?.();\n\
         const property = holder.make();\n\
         property.run();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let lines = conn
        .prepare(
            "SELECT line FROM resolved_edges
             WHERE provenance='receiver-value-flow' ORDER BY line, id",
        )?
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(lines, [6]);
    Ok(())
}

#[test]
fn receiver_value_flow_handles_instance_fields_and_respects_method_blockers() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "main.ts",
        "class Service {\n\
           run() {}\n\
           direct = this.run();\n\
           arrow = () => this.run();\n\
           static bad = this.run();\n\
           ['computed'] = this.run();\n\
           nested = function () { this.run(); };\n\
         }\n\
         class Shadow { run() {} run = () => {}; }\n\
         new Shadow().run();\n\
         class Base { inherited() {} inherited = () => {}; }\n\
         class Child extends Base {}\n\
         new Child().inherited();\n\
         class Written { run() {} constructor() { this.run = () => {}; } }\n\
         new Written().run();\n\
         class Overloaded {\n\
           run(value: string): void;\n\
           run(value: number): void;\n\
           run(_value: string | number): void {}\n\
         }\n\
         new Overloaded().run('x');\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let rows = conn
        .prepare(
            "SELECT line, json_extract(detail_json, '$.flow')
             FROM resolved_edges WHERE provenance='receiver-value-flow'
             ORDER BY line, id",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        rows,
        [
            (3, "this".into()),
            (4, "this".into()),
            (21, "construct".into())
        ]
    );
    Ok(())
}

#[test]
fn receiver_value_flow_requires_exact_nonheuristic_export_resolution() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(repo.path(), "a.ts", "export class Service { run() {} }\n")?;
    write(repo.path(), "b.ts", "export class Service { run() {} }\n")?;
    write(repo.path(), "named.ts", "export { Service } from './a';\n")?;
    write(
        repo.path(),
        "default.ts",
        "export default class DefaultService { run() {} }\n",
    )?;
    write(
        repo.path(),
        "default-barrel.ts",
        "export * from './default';\n",
    )?;
    write(
        repo.path(),
        "ambiguous.ts",
        "export * from './a';\nexport * from './b';\n",
    )?;
    write(
        repo.path(),
        "main.ts",
        "import { Service as Exact } from './named';\n\
         import { Service as Ambiguous } from './ambiguous';\n\
         import DefaultThroughStar from './default-barrel';\n\
         new Exact().run();\n\
         new Ambiguous().run();\n\
         new DefaultThroughStar().run();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let targets = conn
        .prepare(
            "SELECT target.display_name, json_extract(target.meta_json, '$.path')
             FROM resolved_edges edge
             JOIN graph_nodes target ON target.node_key=edge.dst_key
             WHERE edge.provenance='receiver-value-flow'
             ORDER BY edge.line, edge.id",
        )?
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(targets, [("run".into(), "a.ts".into())]);
    Ok(())
}

#[test]
fn receiver_value_flow_rejects_workspace_inferred_module_edges() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "pnpm-workspace.yaml",
        "packages:\n  - packages/*\n",
    )?;
    write(
        repo.path(),
        "packages/lib/package.json",
        r#"{"name":"@acme/lib"}"#,
    )?;
    write(
        repo.path(),
        "packages/lib/src/service.ts",
        "export class Service { run() {} }\n",
    )?;
    write(
        repo.path(),
        "packages/app/package.json",
        r#"{"name":"@acme/app"}"#,
    )?;
    write(
        repo.path(),
        "packages/app/src/main.ts",
        "import { Service } from '@acme/lib/service';\nnew Service().run();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;

    let resolution: String = conn.query_row(
        "SELECT resolution FROM module_edges edge
         JOIN files file ON file.id=edge.from_file
         WHERE file.path='packages/app/src/main.ts'
           AND edge.request='@acme/lib/service'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(resolution, "workspace-inferred");
    let projected: i64 = conn.query_row(
        "SELECT count(*) FROM resolved_edges WHERE provenance='receiver-value-flow'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(projected, 0);
    Ok(())
}

#[test]
fn receiver_value_flow_is_removed_when_a_const_becomes_unsupported() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(
        repo.path(),
        "main.ts",
        "class Service { run() {} }\nconst service = new Service();\nservice.run();\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let count = |conn: &rusqlite::Connection| -> Result<i64> {
        Ok(conn.query_row(
            "SELECT count(*) FROM resolved_edges WHERE provenance='receiver-value-flow'",
            [],
            |row| row.get(0),
        )?)
    };
    assert_eq!(count(&conn)?, 1);

    write(
        repo.path(),
        "main.ts",
        "class Service { run() {} }\n\
         const service = Math.random() ? new Service() : null;\n\
         service.run();\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    assert_eq!(count(&conn)?, 0);
    Ok(())
}

#[test]
fn rebuild_reroutes_barrel_reexports() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(repo.path(), "a.ts", "export function target() {}\n")?;
    write(repo.path(), "b.ts", "export function target() {}\n")?;
    write(repo.path(), "barrel.ts", "export { target } from './a';\n")?;
    write(
        repo.path(),
        "use.ts",
        "import { target } from './barrel';\nexport function run() { target(); }\n",
    )?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let first = neighborhood(
        &conn,
        "use.ts:run",
        &NeighborhoodOptions {
            direction: "out".into(),
            ..Default::default()
        },
    )?;
    assert!(
        first
            .edges
            .iter()
            .any(|edge| edge.target.contains("a.ts#::target@1"))
    );

    write(repo.path(), "barrel.ts", "export { target } from './b';\n")?;
    indexer::index_repo(repo.path(), &conn)?;
    let second = neighborhood(
        &conn,
        "use.ts:run",
        &NeighborhoodOptions {
            direction: "out".into(),
            ..Default::default()
        },
    )?;
    assert!(
        second
            .edges
            .iter()
            .any(|edge| edge.target.contains("b.ts#::target@1"))
    );
    assert!(
        !second
            .edges
            .iter()
            .any(|edge| edge.target.contains("a.ts#::target@1"))
    );
    Ok(())
}

#[test]
fn stale_symbol_anchor_is_explicitly_reresolved() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(repo.path(), "mod.ts", "export function target() {}\n")?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let first = neighborhood(&conn, "mod.ts:target", &NeighborhoodOptions::default())?;

    write(
        repo.path(),
        "mod.ts",
        "// moved\n\nexport function target() {}\n",
    )?;
    indexer::index_repo(repo.path(), &conn)?;
    let second = neighborhood(
        &conn,
        &first.resolved_anchor,
        &NeighborhoodOptions {
            expected_snapshot: Some(first.snapshot.clone()),
            ..Default::default()
        },
    )?;
    assert_ne!(first.snapshot, second.snapshot);
    assert_eq!(second.anchor_status, "re-resolved");
    Ok(())
}

#[test]
fn events_use_hubs_instead_of_direct_emit_listener_edges() -> Result<()> {
    let repo = tempfile::tempdir()?;
    write(repo.path(), "emit.ts", "bus.emit('ready');\n")?;
    write(repo.path(), "listen.ts", "bus.on('ready', start);\n")?;
    let conn = store::open(repo.path())?;
    indexer::index_repo(repo.path(), &conn)?;
    let result = neighborhood(
        &conn,
        "event:unknown:ready",
        &NeighborhoodOptions {
            direction: "both".into(),
            min_confidence: "possible".into(),
            ..Default::default()
        },
    )?;
    assert!(result.edges.iter().any(|edge| edge.kind == "emits"));
    assert!(result.edges.iter().any(|edge| edge.kind == "listens"));
    assert!(!result.edges.iter().any(|edge| {
        edge.source.starts_with("event-site:") && edge.target.starts_with("event-site:")
    }));
    Ok(())
}
