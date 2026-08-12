use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;

use crate::{file_role, origin, semantic, store, structural};

#[derive(Debug, Clone)]
pub struct EntityLookupOptions {
    pub query: String,
    pub planes: Vec<String>,
    pub entity_types: Vec<String>,
    pub roles: Vec<String>,
    pub file_roles: Vec<String>,
    pub file_origins: Vec<String>,
    pub limit: usize,
    pub occurrences_per_entity: usize,
}

impl Default for EntityLookupOptions {
    fn default() -> Self {
        Self {
            query: String::new(),
            planes: Vec::new(),
            entity_types: Vec::new(),
            roles: Vec::new(),
            file_roles: file_role::DEFAULT_EXPANSION
                .iter()
                .map(|role| (*role).to_string())
                .collect(),
            file_origins: origin::defaults(),
            limit: 20,
            occurrences_per_entity: 8,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityOccurrence {
    pub file: String,
    pub file_role: String,
    pub file_origin: String,
    pub lines: [i64; 2],
    pub span: [i64; 2],
    pub role: String,
    pub confidence: String,
    pub extractor: String,
    pub provenance: String,
    pub detail: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityRecord {
    pub anchor: String,
    pub plane: String,
    pub entity_type: String,
    pub name: String,
    pub identity_anchor: Option<String>,
    pub occurrence_count: usize,
    pub occurrences: Vec<EntityOccurrence>,
    pub occurrences_truncated: bool,
    pub meta: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityLookup {
    pub snapshot: String,
    pub entities: Vec<EntityRecord>,
    pub matched_entities: usize,
    pub truncated: bool,
}

pub fn entities(conn: &Connection, options: &EntityLookupOptions) -> Result<EntityLookup> {
    if options.limit == 0 || options.occurrences_per_entity == 0 {
        bail!("entity and occurrence limits must be greater than zero");
    }
    origin::validate_all(&options.file_origins)?;
    file_role::validate_all(&options.file_roles)?;
    let allowed_planes: HashSet<&str> = options.planes.iter().map(String::as_str).collect();
    if let Some(invalid) = allowed_planes
        .iter()
        .find(|plane| !matches!(**plane, "runtime" | "contract" | "general"))
    {
        bail!("unknown entity plane `{invalid}`");
    }
    let planes = serde_json::to_string(&options.planes)?;
    let entity_types = serde_json::to_string(&options.entity_types)?;
    let roles = serde_json::to_string(&options.roles)?;
    let file_roles = serde_json::to_string(&options.file_roles)?;
    let file_origins = serde_json::to_string(&options.file_origins)?;

    let mut stmt = conn.prepare(
        "WITH ranked AS (
           SELECT entity.id, entity.entity_key, entity.plane, entity.entity_type,
                  entity.name, entity.identity_anchor, entity.meta_json,
                  count(occurrence.id) AS occurrence_count,
                  CASE WHEN ?1 <> '' AND lower(entity.name)=lower(?1) THEN 1 ELSE 0 END AS exact
           FROM entities entity
           JOIN entity_occurrences occurrence ON occurrence.entity_id=entity.id
           JOIN files file ON file.id=occurrence.file_id
           WHERE (?2 OR entity.plane IN (SELECT value FROM json_each(?3)))
             AND (?4 OR entity.entity_type IN (SELECT value FROM json_each(?5)))
             AND (?1 = '' OR instr(lower(entity.name), lower(?1)) > 0
                          OR instr(lower(entity.entity_key), lower(?1)) > 0)
             AND (?6 OR occurrence.role IN (SELECT value FROM json_each(?7)))
             AND (?8 OR file.role IN (SELECT value FROM json_each(?9)))
             AND file.origin IN (SELECT value FROM json_each(?10))
           GROUP BY entity.id, entity.entity_key, entity.plane, entity.entity_type,
                    entity.name, entity.identity_anchor, entity.meta_json
         )
         SELECT id, entity_key, plane, entity_type, name, identity_anchor, meta_json,
                occurrence_count, exact, count(*) OVER () AS matched_entities
         FROM ranked
         ORDER BY exact DESC, occurrence_count DESC,
                  plane, entity_type, name, entity_key
         LIMIT ?11",
    )?;
    let rows = stmt.query_map(
        params![
            options.query,
            options.planes.is_empty(),
            planes,
            options.entity_types.is_empty(),
            entity_types,
            options.roles.is_empty(),
            roles,
            options.file_roles.is_empty(),
            file_roles,
            file_origins,
            options.limit as i64,
        ],
        |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
            ))
        },
    )?;
    let mut entities = Vec::new();
    let mut matched_entities = 0;
    for row in rows {
        let (id, key, plane, entity_type, name, identity_anchor, meta, occurrence_count, _, total) =
            row?;
        matched_entities = total as usize;
        let mut occurrences = load_occurrences(conn, id, options)?;
        let occurrences_truncated = occurrence_count as usize > options.occurrences_per_entity;
        occurrences.truncate(options.occurrences_per_entity);
        entities.push(EntityRecord {
            anchor: key,
            plane,
            entity_type,
            name,
            identity_anchor,
            occurrence_count: occurrence_count as usize,
            occurrences,
            occurrences_truncated,
            meta: serde_json::from_str(&meta).unwrap_or(Value::Null),
        });
    }
    Ok(EntityLookup {
        snapshot: structural::current_snapshot(conn)?,
        entities,
        matched_entities,
        truncated: matched_entities > options.limit,
    })
}

fn load_occurrences(
    conn: &Connection,
    entity_id: i64,
    options: &EntityLookupOptions,
) -> Result<Vec<EntityOccurrence>> {
    let roles = serde_json::to_string(&options.roles)?;
    let file_roles = serde_json::to_string(&options.file_roles)?;
    let file_origins = serde_json::to_string(&options.file_origins)?;
    let mut stmt = conn.prepare(
        "SELECT file.path, file.role, file.origin,
                occurrence.line, occurrence.end_line,
                occurrence.start, occurrence.end, occurrence.role,
                occurrence.confidence, occurrence.extractor,
                occurrence.provenance, occurrence.detail_json
         FROM entity_occurrences occurrence
         JOIN files file ON file.id=occurrence.file_id
         WHERE occurrence.entity_id=?1
           AND (?2 OR occurrence.role IN (SELECT value FROM json_each(?3)))
           AND (?4 OR file.role IN (SELECT value FROM json_each(?5)))
           AND file.origin IN (SELECT value FROM json_each(?6))
         ORDER BY occurrence.confidence='certain' DESC,
                  occurrence.confidence='likely' DESC,
                  file.path, occurrence.start, occurrence.id
         LIMIT ?7",
    )?;
    let rows = stmt.query_map(
        params![
            entity_id,
            options.roles.is_empty(),
            roles,
            options.file_roles.is_empty(),
            file_roles,
            file_origins,
            options.occurrences_per_entity.saturating_add(1) as i64,
        ],
        |row| {
            let detail: String = row.get(11)?;
            Ok(EntityOccurrence {
                file: row.get(0)?,
                file_role: row.get(1)?,
                file_origin: row.get(2)?,
                lines: [row.get(3)?, row.get(4)?],
                span: [row.get(5)?, row.get(6)?],
                role: row.get(7)?,
                confidence: row.get(8)?,
                extractor: row.get(9)?,
                provenance: row.get(10)?,
                detail: serde_json::from_str(&detail).unwrap_or(Value::Null),
            })
        },
    )?;
    let mut occurrences = Vec::new();
    for row in rows {
        occurrences.push(row?);
    }
    Ok(occurrences)
}

#[derive(Debug, Clone, Serialize)]
pub struct AreaOverview {
    pub path: String,
    pub files: usize,
    pub chunks: usize,
    pub symbols: usize,
    pub entity_occurrences: usize,
    pub roles: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InventoryCount {
    pub plane: String,
    pub kind: String,
    pub entities: usize,
    pub occurrences: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationCount {
    pub kind: String,
    pub edges: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryOverview {
    pub snapshot: String,
    pub totals: BTreeMap<String, usize>,
    pub files_by_origin: BTreeMap<String, usize>,
    pub files_by_role: BTreeMap<String, usize>,
    pub areas: Vec<AreaOverview>,
    pub areas_truncated: bool,
    pub entity_inventory: Vec<InventoryCount>,
    pub entity_inventory_truncated: bool,
    pub relations: Vec<RelationCount>,
    pub relations_truncated: bool,
}

#[derive(Debug, Clone)]
pub struct OverviewOptions {
    pub file_origins: Vec<String>,
    pub area_limit: usize,
    pub relation_limit: usize,
    pub include_semantic: bool,
    pub semantic_limit: usize,
    pub semantic_types: Vec<String>,
    pub response_byte_limit: usize,
}

impl Default for OverviewOptions {
    fn default() -> Self {
        Self {
            file_origins: origin::defaults(),
            area_limit: 20,
            relation_limit: 30,
            include_semantic: false,
            semantic_limit: 8,
            semantic_types: Vec::new(),
            response_byte_limit: 24_000,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticOverlayArtifact {
    pub id: i64,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub name: Option<String>,
    pub trust: String,
    pub body: Value,
    pub confidence: String,
    pub model: String,
    pub prompt_version: String,
    pub created_at: String,
    pub freshness: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SemanticOverlay {
    pub fresh_matched: usize,
    pub excluded_non_fresh: usize,
    pub returned: usize,
    pub truncated: bool,
    pub artifacts: Vec<SemanticOverlayArtifact>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OverviewResponseBudget {
    pub byte_limit: usize,
    pub rendered_bytes: usize,
    pub unbudgeted_bytes: usize,
    pub truncated: bool,
    pub omitted_semantic_artifacts: usize,
    pub omitted_relations: usize,
    pub omitted_areas: usize,
    pub omitted_entity_inventory: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryOverviewResponse {
    #[serde(flatten)]
    pub overview: RepositoryOverview,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_overlay: Option<SemanticOverlay>,
    pub response_budget: OverviewResponseBudget,
}

fn overview_unpinned(
    conn: &Connection,
    file_origins: &[String],
    area_limit: usize,
    relation_limit: usize,
) -> Result<RepositoryOverview> {
    if area_limit == 0 || relation_limit == 0 {
        bail!("overview limits must be greater than zero");
    }
    origin::validate_all(file_origins)?;
    let allowed_origins: HashSet<&str> = file_origins.iter().map(String::as_str).collect();
    let repository = allowed_origins.contains("repository");
    let workspace = allowed_origins.contains("workspace");
    let dependency = allowed_origins.contains("dependency");
    let mut areas: HashMap<String, AreaOverview> = HashMap::new();
    let mut files_by_origin = BTreeMap::new();
    let mut files_by_role = BTreeMap::new();
    let mut totals = BTreeMap::from([
        ("files".to_string(), 0),
        ("chunks".to_string(), 0),
        ("symbols".to_string(), 0),
        ("entity_occurrences".to_string(), 0),
        ("graph_edges".to_string(), 0),
    ]);
    let mut stmt = conn.prepare(
        "SELECT file.path, file.origin, file.role,
                COALESCE(chunk.count, 0), COALESCE(symbol.count, 0),
                COALESCE(site.count, 0)
         FROM files file
         LEFT JOIN (SELECT file_id, count(*) AS count FROM chunks GROUP BY file_id) chunk
           ON chunk.file_id=file.id
         LEFT JOIN (SELECT file_id, count(*) AS count FROM symbols GROUP BY file_id) symbol
           ON symbol.file_id=file.id
         LEFT JOIN (SELECT file_id, count(*) AS count FROM entity_sites GROUP BY file_id) site
           ON site.file_id=file.id
         ORDER BY file.path",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)? as usize,
            row.get::<_, i64>(4)? as usize,
            row.get::<_, i64>(5)? as usize,
        ))
    })?;
    for row in rows {
        let (path, file_origin, role, chunks, symbols, occurrences) = row?;
        if !allowed_origins.contains(file_origin.as_str()) {
            continue;
        }
        *files_by_origin.entry(file_origin).or_default() += 1;
        *files_by_role.entry(role.clone()).or_default() += 1;
        *totals.get_mut("files").expect("total exists") += 1;
        *totals.get_mut("chunks").expect("total exists") += chunks;
        *totals.get_mut("symbols").expect("total exists") += symbols;
        *totals.get_mut("entity_occurrences").expect("total exists") += occurrences;
        let area_path = repository_area(&path);
        let area = areas
            .entry(area_path.clone())
            .or_insert_with(|| AreaOverview {
                path: area_path,
                files: 0,
                chunks: 0,
                symbols: 0,
                entity_occurrences: 0,
                roles: BTreeMap::new(),
            });
        area.files += 1;
        area.chunks += chunks;
        area.symbols += symbols;
        area.entity_occurrences += occurrences;
        *area.roles.entry(role).or_default() += 1;
    }
    let mut areas: Vec<_> = areas.into_values().collect();
    areas.sort_by(|left, right| {
        right
            .files
            .cmp(&left.files)
            .then_with(|| right.symbols.cmp(&left.symbols))
            .then_with(|| left.path.cmp(&right.path))
    });
    let areas_truncated = areas.len() > area_limit;
    areas.truncate(area_limit);

    let mut entity_inventory = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT entity.plane, entity.entity_type,
                count(DISTINCT entity.id), count(DISTINCT occurrence.id)
         FROM entities entity
         JOIN entity_occurrences occurrence ON occurrence.entity_id=entity.id
         JOIN files file ON file.id=occurrence.file_id
         WHERE (?1 AND file.origin='repository')
            OR (?2 AND file.origin='workspace')
            OR (?3 AND file.origin='dependency')
         GROUP BY entity.plane, entity.entity_type
         ORDER BY entity.plane, entity.entity_type",
    )?;
    let rows = stmt.query_map(params![repository, workspace, dependency], |row| {
        Ok(InventoryCount {
            plane: row.get(0)?,
            kind: row.get(1)?,
            entities: row.get::<_, i64>(2)? as usize,
            occurrences: row.get::<_, i64>(3)? as usize,
        })
    })?;
    for row in rows {
        entity_inventory.push(row?);
    }

    let mut relations = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT edge.kind, count(*)
         FROM resolved_edges edge
         LEFT JOIN files file ON file.id=edge.source_file_id
         WHERE file.origin IS NULL
            OR (?1 AND file.origin='repository')
            OR (?2 AND file.origin='workspace')
            OR (?3 AND file.origin='dependency')
         GROUP BY edge.kind ORDER BY count(*) DESC, edge.kind",
    )?;
    let rows = stmt.query_map(params![repository, workspace, dependency], |row| {
        Ok(RelationCount {
            kind: row.get(0)?,
            edges: row.get::<_, i64>(1)? as usize,
        })
    })?;
    for row in rows {
        let relation = row?;
        relations.push(relation);
    }
    *totals.get_mut("graph_edges").expect("total exists") =
        relations.iter().map(|relation| relation.edges).sum();
    let relations_truncated = relations.len() > relation_limit;
    relations.truncate(relation_limit);

    Ok(RepositoryOverview {
        snapshot: structural::current_snapshot(conn)?,
        totals,
        files_by_origin,
        files_by_role,
        areas,
        areas_truncated,
        entity_inventory,
        entity_inventory_truncated: false,
        relations,
        relations_truncated,
    })
}

pub fn overview_response(
    conn: &Connection,
    options: &OverviewOptions,
) -> Result<RepositoryOverviewResponse> {
    if options.semantic_limit == 0 || options.semantic_limit > 100 {
        bail!("semantic overview limit must be between 1 and 100");
    }
    if options.response_byte_limit == 0 {
        bail!("overview response byte limit must be greater than zero");
    }
    validate_semantic_types(&options.semantic_types)?;
    store::with_read_snapshot(conn, "jscout_repository_overview_pack", || {
        let overview = overview_unpinned(
            conn,
            &options.file_origins,
            options.area_limit,
            options.relation_limit,
        )?;
        let semantic_overlay = options
            .include_semantic
            .then(|| semantic_overlay(conn, &options.semantic_types, options.semantic_limit))
            .transpose()?;
        let mut response = RepositoryOverviewResponse {
            overview,
            semantic_overlay,
            response_budget: OverviewResponseBudget {
                byte_limit: options.response_byte_limit,
                ..Default::default()
            },
        };
        apply_overview_budget(&mut response)?;
        Ok(response)
    })
}

fn validate_semantic_types(types: &[String]) -> Result<()> {
    if let Some(invalid) = types.iter().find(|artifact_type| {
        !matches!(
            artifact_type.as_str(),
            "workflow" | "card" | "concept" | "summary" | "annotation"
        )
    }) {
        bail!("unknown semantic artifact type `{invalid}`");
    }
    Ok(())
}

fn semantic_overlay(
    conn: &Connection,
    configured_types: &[String],
    limit: usize,
) -> Result<SemanticOverlay> {
    let default_types = ["summary", "concept", "workflow", "annotation"];
    let types = if configured_types.is_empty() {
        default_types.iter().copied().collect::<HashSet<_>>()
    } else {
        configured_types.iter().map(String::as_str).collect()
    };
    let mut statement = conn.prepare(
        "SELECT artifact.id FROM semantic_artifacts artifact
         WHERE NOT EXISTS(
           SELECT 1 FROM semantic_artifacts successor
           WHERE successor.supersedes_artifact_id=artifact.id
         )
         ORDER BY artifact.id DESC",
    )?;
    let ids = statement
        .query_map([], |row| row.get::<_, i64>(0))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let artifacts = semantic::load_artifacts(conn, &ids)?;
    let mut excluded_non_fresh = 0;
    let mut artifacts = artifacts
        .into_iter()
        .filter(|artifact| types.contains(artifact.artifact_type.as_str()))
        .filter_map(|artifact| {
            if artifact.freshness != "fresh" {
                excluded_non_fresh += 1;
                return None;
            }
            Some(artifact)
        })
        .collect::<Vec<_>>();
    artifacts.sort_by(|left, right| {
        semantic_overlay_priority(left)
            .cmp(&semantic_overlay_priority(right))
            .then_with(|| right.id.cmp(&left.id))
    });
    let fresh_matched = artifacts.len();
    artifacts.truncate(limit);
    let artifacts = artifacts
        .into_iter()
        .map(|artifact| SemanticOverlayArtifact {
            id: artifact.id,
            artifact_type: artifact.artifact_type,
            name: artifact.name,
            trust: artifact.trust,
            body: artifact.body,
            confidence: artifact.confidence,
            model: artifact.model,
            prompt_version: artifact.prompt_version,
            created_at: artifact.created_at,
            freshness: artifact.freshness,
        })
        .collect::<Vec<_>>();
    let returned = artifacts.len();
    Ok(SemanticOverlay {
        fresh_matched,
        excluded_non_fresh,
        returned,
        truncated: returned < fresh_matched,
        artifacts,
    })
}

fn semantic_overlay_priority(artifact: &semantic::SemanticArtifact) -> u8 {
    match (artifact.artifact_type.as_str(), artifact.name.as_deref()) {
        ("summary", Some("repo")) => 0,
        ("concept", _) => 1,
        ("workflow", _) => 2,
        ("annotation", _) => 3,
        ("summary", Some(name)) if name.starts_with("module:") => 4,
        ("summary", _) => 5,
        ("card", _) => 6,
        _ => 7,
    }
}

fn apply_overview_budget(response: &mut RepositoryOverviewResponse) -> Result<()> {
    let byte_limit = response.response_budget.byte_limit;
    let unbudgeted = settle_overview_bytes(response)?;
    response.response_budget.unbudgeted_bytes = unbudgeted;
    settle_overview_bytes(response)?;
    while response.response_budget.rendered_bytes > byte_limit {
        response.response_budget.truncated = true;
        if let Some(overlay) = response.semantic_overlay.as_mut()
            && overlay.artifacts.pop().is_some()
        {
            overlay.returned = overlay.artifacts.len();
            overlay.truncated = true;
            response.response_budget.omitted_semantic_artifacts += 1;
            settle_overview_bytes(response)?;
            continue;
        }
        if response.overview.relations.pop().is_some() {
            response.overview.relations_truncated = true;
            response.response_budget.omitted_relations += 1;
            settle_overview_bytes(response)?;
            continue;
        }
        if response.overview.areas.pop().is_some() {
            response.overview.areas_truncated = true;
            response.response_budget.omitted_areas += 1;
            settle_overview_bytes(response)?;
            continue;
        }
        if response.overview.entity_inventory.pop().is_some() {
            response.overview.entity_inventory_truncated = true;
            response.response_budget.omitted_entity_inventory += 1;
            settle_overview_bytes(response)?;
            continue;
        }
        let minimum = settle_overview_bytes(response)?;
        bail!(
            "overview response byte limit {byte_limit} is below the minimum envelope ({minimum} bytes)"
        );
    }
    Ok(())
}

fn settle_overview_bytes(response: &mut RepositoryOverviewResponse) -> Result<usize> {
    for _ in 0..8 {
        let rendered = serde_json::to_string_pretty(response)?.len();
        if response.response_budget.rendered_bytes == rendered {
            return Ok(rendered);
        }
        response.response_budget.rendered_bytes = rendered;
    }
    Ok(serde_json::to_string_pretty(response)?.len())
}

fn repository_area(path: &str) -> String {
    if let Some(dependency_path) = path.strip_prefix("dependency:") {
        let mut parts = dependency_path.split('/');
        let first = parts.next().unwrap_or(dependency_path);
        if first.starts_with('@')
            && let Some(package) = parts.next()
        {
            return format!("dependency:{first}/{package}");
        }
        return format!("dependency:{first}");
    }
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [root @ ("packages" | "apps" | "services"), scope, name, ..] if scope.starts_with('@') => {
            format!("{root}/{scope}/{name}")
        }
        [root @ ("packages" | "apps" | "services"), name, ..] => {
            format!("{root}/{name}")
        }
        ["src", name, ..] => format!("src/{name}"),
        [root, ..] => (*root).to_string(),
        [] => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use anyhow::Result;

    use serde_json::json;

    use super::{
        EntityLookupOptions, OverviewOptions, entities, overview_response, repository_area,
    };
    use crate::{indexer, semantic, store, structural};

    #[test]
    fn entity_lookup_filters_evidence_and_overview_is_bounded() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join("packages/api/src"))?;
        fs::write(
            repo.path().join("packages/api/src/main.ts"),
            "export function run() { return process.env.API_KEY + process.env.OTHER_KEY; }\n",
        )?;
        fs::create_dir_all(repo.path().join("packages/api/test"))?;
        fs::write(
            repo.path().join("packages/api/test/main.test.ts"),
            "test('env', () => process.env.API_KEY);\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;

        let result = entities(
            &conn,
            &EntityLookupOptions {
                query: "API_KEY".into(),
                planes: vec!["general".into()],
                ..Default::default()
            },
        )?;
        assert_eq!(result.entities.len(), 1);
        assert_eq!(result.entities[0].occurrence_count, 1);
        assert_eq!(result.entities[0].occurrences[0].file_role, "production");

        let bounded = entities(
            &conn,
            &EntityLookupOptions {
                planes: vec!["general".into()],
                limit: 1,
                ..Default::default()
            },
        )?;
        assert_eq!(bounded.entities.len(), 1);
        assert_eq!(bounded.matched_entities, 2);
        assert!(bounded.truncated);

        let overview = overview_response(
            &conn,
            &OverviewOptions {
                area_limit: 1,
                relation_limit: 2,
                ..Default::default()
            },
        )?
        .overview;
        assert_eq!(overview.areas.len(), 1);
        assert_eq!(overview.areas[0].path, "packages/api");
        assert!(overview.relations.len() <= 2);
        assert_eq!(overview.totals["files"], 2);
        Ok(())
    }

    #[test]
    fn dependency_areas_preserve_the_package_instance_prefix() {
        assert_eq!(
            repository_area("dependency:lodash@4.17.21#abc123/lodash.js"),
            "dependency:lodash@4.17.21#abc123"
        );
        assert_eq!(
            repository_area("dependency:@scope/pkg@2.0.0#def456/src/index.ts"),
            "dependency:@scope/pkg@2.0.0#def456"
        );
    }

    #[test]
    fn semantic_overview_includes_only_current_fresh_memory() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::write(
            repo.path().join("flow.ts"),
            "export function start() { return 1; }\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let anchor = structural::resolve_current_anchor(&conn, "flow.ts:start")?;
        let snapshot = structural::current_snapshot(&conn)?;
        semantic::annotate(
            repo.path(),
            &conn,
            &semantic::AnnotateInput {
                artifact_type: "card".into(),
                name: Some(anchor.clone()),
                body: json!({ "purpose": "starts the flow" }),
                supports: vec![semantic::SupportInput {
                    claim_path: "/purpose".into(),
                    anchor,
                    role: None,
                    evidence_file: "flow.ts".into(),
                    evidence_start_line: 1,
                    evidence_end_line: 1,
                    confidence: "likely".into(),
                }],
                confidence: "likely".into(),
                snapshot,
                supersedes: None,
            },
        )?;

        let options = OverviewOptions {
            include_semantic: true,
            semantic_types: vec!["card".into()],
            ..Default::default()
        };
        let fresh = overview_response(&conn, &options)?;
        let overlay = fresh.semantic_overlay.expect("overlay requested");
        assert_eq!(overlay.returned, 1);
        assert_eq!(overlay.excluded_non_fresh, 0);
        assert_eq!(overlay.artifacts[0].freshness, "fresh");

        fs::write(
            repo.path().join("flow.ts"),
            "export function start() { return 2; }\n",
        )?;
        indexer::index_repo(repo.path(), &conn)?;
        let drifted = overview_response(&conn, &options)?;
        let overlay = drifted.semantic_overlay.expect("overlay requested");
        assert!(overlay.artifacts.is_empty());
        assert_eq!(overlay.excluded_non_fresh, 1);
        Ok(())
    }
}
