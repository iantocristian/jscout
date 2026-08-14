use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;

use crate::{file_role, origin, semantic, store, structural};

const OVERVIEW_CITATIONS_PER_CLASSIFICATION: usize = 3;
const OVERVIEW_CITATION_CHARS: usize = 320;

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
    pub areas_matched: usize,
    pub areas: Vec<AreaOverview>,
    pub areas_truncated: bool,
    pub entity_inventory_matched: usize,
    pub entity_inventory: Vec<InventoryCount>,
    pub entity_inventory_truncated: bool,
    pub relations_matched: usize,
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
    pub reconnaissance_limit: usize,
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
            reconnaissance_limit: 12,
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

#[derive(Debug, Clone, Serialize)]
pub struct ReconnaissanceCitation {
    pub id: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<[usize; 2]>,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReconnaissanceClassification {
    pub subject: String,
    pub kind: String,
    pub role: String,
    pub confidence: String,
    pub policy: String,
    pub explanation: String,
    pub citation_ids: Vec<String>,
    pub cited_evidence: Vec<ReconnaissanceCitation>,
    pub cited_evidence_truncated: bool,
    pub member_count: usize,
    pub deterministic_file_roles: BTreeMap<String, usize>,
    pub effective_file_roles: BTreeMap<String, usize>,
    pub conflict_files: usize,
    pub depth: usize,
    pub prompt_version: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReconnaissanceOverlay {
    pub trust: &'static str,
    pub matched: usize,
    pub returned: usize,
    pub truncated: bool,
    pub roles: BTreeMap<String, usize>,
    pub effective_file_roles: BTreeMap<String, usize>,
    pub classifications: Vec<ReconnaissanceClassification>,
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
    pub omitted_reconnaissance_classifications: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RepositoryOverviewResponse {
    #[serde(flatten)]
    pub overview: RepositoryOverview,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_overlay: Option<SemanticOverlay>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconnaissance: Option<ReconnaissanceOverlay>,
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
    let areas_matched = areas.len();
    let areas_truncated = areas_matched > area_limit;
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
    let entity_inventory_matched = entity_inventory.len();

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
    let relations_matched = relations.len();
    let relations_truncated = relations_matched > relation_limit;
    relations.truncate(relation_limit);

    Ok(RepositoryOverview {
        snapshot: structural::current_snapshot(conn)?,
        totals,
        files_by_origin,
        files_by_role,
        areas_matched,
        areas,
        areas_truncated,
        entity_inventory_matched,
        entity_inventory,
        entity_inventory_truncated: false,
        relations_matched,
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
    if options.reconnaissance_limit == 0 || options.reconnaissance_limit > 100 {
        bail!("reconnaissance overview limit must be between 1 and 100");
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
        let reconnaissance =
            reconnaissance_overlay(conn, &options.file_origins, options.reconnaissance_limit)?;
        let omitted_semantic_artifacts = semantic_overlay.as_ref().map_or(0, |overlay| {
            overlay.fresh_matched.saturating_sub(overlay.returned)
        });
        let omitted_relations = overview
            .relations_matched
            .saturating_sub(overview.relations.len());
        let omitted_areas = overview.areas_matched.saturating_sub(overview.areas.len());
        let omitted_entity_inventory = overview
            .entity_inventory_matched
            .saturating_sub(overview.entity_inventory.len());
        let omitted_reconnaissance_classifications = reconnaissance.as_ref().map_or(0, |overlay| {
            overlay.matched.saturating_sub(overlay.returned)
        });
        let initially_truncated = omitted_semantic_artifacts > 0
            || omitted_relations > 0
            || omitted_areas > 0
            || omitted_entity_inventory > 0
            || omitted_reconnaissance_classifications > 0;
        let mut response = RepositoryOverviewResponse {
            overview,
            semantic_overlay,
            reconnaissance,
            response_budget: OverviewResponseBudget {
                byte_limit: options.response_byte_limit,
                truncated: initially_truncated,
                omitted_semantic_artifacts,
                omitted_relations,
                omitted_areas,
                omitted_entity_inventory,
                omitted_reconnaissance_classifications,
                ..Default::default()
            },
        };
        apply_overview_budget(&mut response)?;
        Ok(response)
    })
}

fn reconnaissance_overlay(
    conn: &Connection,
    file_origins: &[String],
    limit: usize,
) -> Result<Option<ReconnaissanceOverlay>> {
    let matched: usize = conn.query_row(
        "SELECT count(*) FROM repository_current_classifications",
        [],
        |row| Ok(row.get::<_, i64>(0)? as usize),
    )?;
    if matched == 0 {
        return Ok(None);
    }
    let mut roles = BTreeMap::new();
    let mut statement = conn.prepare(
        "SELECT role, count(*) FROM repository_current_classifications
         GROUP BY role ORDER BY role",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
    })?;
    for row in rows {
        let (role, count) = row?;
        roles.insert(role, count);
    }

    let mut effective_file_roles = BTreeMap::<String, usize>::new();
    let origins_json = serde_json::to_string(file_origins)?;
    let mut statement = conn.prepare(
        "SELECT COALESCE(policy.effective_role, file.role), count(*)
         FROM files file
         LEFT JOIN repository_file_policy policy ON policy.file_id=file.id
         WHERE file.origin IN (SELECT value FROM json_each(?1))
         GROUP BY COALESCE(policy.effective_role, file.role)
         ORDER BY COALESCE(policy.effective_role, file.role)",
    )?;
    let rows = statement.query_map([origins_json], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
    })?;
    for row in rows {
        let (role, count) = row?;
        effective_file_roles.insert(role, count);
    }

    let mut statement = conn.prepare(
        "SELECT subject_key,subject_kind,role,confidence,explanation,
                citations_json,cited_evidence_json,member_count,
                deterministic_roles_json,effective_roles_json,conflict_files,
                depth,prompt_version
         FROM repository_current_classifications
         ORDER BY
           CASE role WHEN 'mixed' THEN 0 WHEN 'unknown' THEN 1 ELSE 2 END,
           CASE confidence WHEN 'possible' THEN 0 ELSE 1 END,
           (conflict_files > 0) DESC,
           CASE role WHEN 'runtime' THEN 1 ELSE 0 END,
           conflict_files DESC, subject_key
         LIMIT ?1",
    )?;
    let rows = statement.query_map([limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, String>(6)?,
            row.get::<_, i64>(7)? as usize,
            row.get::<_, String>(8)?,
            row.get::<_, String>(9)?,
            row.get::<_, i64>(10)? as usize,
            row.get::<_, i64>(11)? as usize,
            row.get::<_, String>(12)?,
        ))
    })?;
    let mut classifications = Vec::new();
    for row in rows {
        let (
            subject,
            kind,
            role,
            confidence,
            explanation,
            citations_json,
            cited_evidence_json,
            member_count,
            deterministic_roles_json,
            effective_roles_json,
            conflict_files,
            depth,
            prompt_version,
        ) = row?;
        let citation_ids = serde_json::from_str::<Vec<String>>(&citations_json)?;
        let cited_evidence_values = serde_json::from_str::<Vec<Value>>(&cited_evidence_json)?;
        let cited_evidence_matched = cited_evidence_values.len();
        let cited_evidence = cited_evidence_values
            .into_iter()
            .take(OVERVIEW_CITATIONS_PER_CLASSIFICATION)
            .map(reconnaissance_citation)
            .collect::<Result<Vec<_>>>()?;
        let policy = if confidence == "likely"
            && matches!(
                role.as_str(),
                "runtime" | "tooling" | "documentation" | "test" | "generated"
            ) {
            "active"
        } else {
            "neutral"
        };
        classifications.push(ReconnaissanceClassification {
            subject,
            kind,
            role,
            confidence,
            policy: policy.into(),
            explanation,
            citation_ids,
            cited_evidence_truncated: cited_evidence_matched > cited_evidence.len(),
            cited_evidence,
            member_count,
            deterministic_file_roles: parse_count_map(&deterministic_roles_json)?,
            effective_file_roles: parse_count_map(&effective_roles_json)?,
            conflict_files,
            depth,
            prompt_version,
        });
    }
    let returned = classifications.len();
    Ok(Some(ReconnaissanceOverlay {
        trust: "untrusted_semantic_policy",
        matched,
        returned,
        truncated: returned < matched,
        roles,
        effective_file_roles,
        classifications,
    }))
}

fn parse_count_map(value: &str) -> Result<BTreeMap<String, usize>> {
    Ok(serde_json::from_str(value)?)
}

fn reconnaissance_citation(value: Value) -> Result<ReconnaissanceCitation> {
    let id = value
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let source = value
        .get("source")
        .and_then(Value::as_str)
        .map(str::to_string);
    let start = value.get("start_line").and_then(Value::as_u64);
    let end = value.get("end_line").and_then(Value::as_u64);
    let lines = start
        .zip(end)
        .map(|(start, end)| [start as usize, end as usize]);
    let content = value
        .get("content")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let excerpt = if content.chars().count() <= OVERVIEW_CITATION_CHARS {
        content.to_string()
    } else {
        content
            .chars()
            .take(OVERVIEW_CITATION_CHARS)
            .collect::<String>()
            + "…"
    };
    Ok(ReconnaissanceCitation {
        id,
        kind,
        source,
        lines,
        excerpt,
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
        "SELECT artifact.id, artifact.artifact_type FROM semantic_artifacts artifact
         WHERE NOT EXISTS(
           SELECT 1 FROM semantic_artifacts successor
           WHERE successor.supersedes_artifact_id=artifact.id
         )
         ORDER BY artifact.id DESC",
    )?;
    let ids = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .filter_map(|row| match row {
            Ok((id, artifact_type)) if types.contains(artifact_type.as_str()) => Some(Ok(id)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let artifacts = semantic::load_artifacts(conn, &ids)?;
    let mut excluded_non_fresh = 0;
    let mut artifacts = artifacts
        .into_iter()
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
    settle_overview_unbudgeted_bytes(response)?;
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
        if let Some(overlay) = response.reconnaissance.as_mut()
            && overlay.classifications.pop().is_some()
        {
            overlay.returned = overlay.classifications.len();
            overlay.truncated = true;
            response
                .response_budget
                .omitted_reconnaissance_classifications += 1;
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

fn settle_overview_unbudgeted_bytes(response: &mut RepositoryOverviewResponse) -> Result<()> {
    for _ in 0..8 {
        let rendered = settle_overview_bytes(response)?;
        if response.response_budget.unbudgeted_bytes == rendered {
            return Ok(());
        }
        response.response_budget.unbudgeted_bytes = rendered;
    }
    settle_overview_bytes(response)?;
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
    use crate::{indexer, recon, semantic, store, structural};

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
    fn overview_surfaces_current_cited_reconnaissance_and_effective_roles() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join("docs"))?;
        fs::write(
            repo.path().join("docs/runtime.ts"),
            "export function renderDocument() { return 1; }\n",
        )?;
        fs::write(
            repo.path().join("docs/runtime.test.ts"),
            "test('render', () => renderDocument());\n",
        )?;
        let conn = store::open(repo.path())?;
        indexer::index_repo(repo.path(), &conn)?;
        let selector = recon::SubjectSelector::RepositoryArea {
            scope: "docs".into(),
            direct_only: false,
        };
        let state = recon::build_scope_state(
            repo.path(),
            &conn,
            "area:repository:docs".into(),
            selector.clone(),
        )?;
        let snapshot = structural::current_snapshot(&conn)?;
        conn.execute(
            "INSERT INTO scout_runs(
               scout_kind,status,gateway_protocol,provider,model,billing_path,
               prompt_version,source_snapshot,input_fingerprint,request_hash,
               config_json,started_at,completed_at
             ) VALUES('repository','completed',1,'openai-codex','gpt-5.6-terra',
                      'plan','repository-recon/v2',?1,'overview-recon',
                      'overview-recon','{}','now','now')",
            [&snapshot],
        )?;
        let run_id = conn.last_insert_rowid();
        let cited = json!([{
            "id": "E001",
            "kind": "outline",
            "source": "docs/runtime.ts",
            "start_line": 1,
            "end_line": 1,
            "content": "exported function `renderDocument`"
        }]);
        recon::persist_classification(
            &conn,
            &recon::NewClassification {
                run_id,
                subject_key: &state.subject_key,
                subject_kind: "area",
                selector: &selector,
                parent_subject_key: None,
                depth: 0,
                role: "runtime",
                confidence: "likely",
                explanation: "document-domain runtime implementation",
                citations_json: "[\"E001\"]",
                cited_evidence_json: &cited.to_string(),
                evidence_fingerprint: &state.evidence_fingerprint,
                classification_fingerprint: "overview-recon",
                source_snapshot: &snapshot,
            },
        )?;
        recon::reconcile_file_policy(repo.path(), &conn)?;

        let response = overview_response(&conn, &OverviewOptions::default())?;
        let overlay = response
            .reconnaissance
            .expect("current reconnaissance overlay");
        assert_eq!(overlay.trust, "untrusted_semantic_policy");
        assert_eq!(overlay.roles["runtime"], 1);
        assert_eq!(overlay.effective_file_roles["runtime"], 1);
        assert_eq!(overlay.effective_file_roles["test"], 1);
        assert_eq!(overlay.classifications[0].conflict_files, 1);
        assert_eq!(overlay.classifications[0].citation_ids, ["E001"]);
        assert_eq!(
            overlay.classifications[0].cited_evidence[0]
                .source
                .as_deref(),
            Some("docs/runtime.ts")
        );
        Ok(())
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
                body: json!({ "purpose": "starts the flow ".repeat(256) }),
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
        conn.execute(
            "INSERT INTO semantic_artifacts(
               artifact_type, canonical_name, body_json, model, prompt_version,
               confidence, source_snapshot, created_at, artifact_fingerprint
             ) VALUES('summary','module:excluded',?1,'test','summary-scout/v1',
                      'likely',?2,'now','excluded-summary')",
            rusqlite::params![
                json!({
                    "level": "module",
                    "scope": "module:excluded",
                    "overview": "must not be freshness-loaded by a card-only overlay",
                })
                .to_string(),
                structural::current_snapshot(&conn)?,
            ],
        )?;
        conn.execute(
            "UPDATE meta SET value='/definitely/missing/jscout/root' WHERE key='root'",
            [],
        )?;

        let options = OverviewOptions {
            include_semantic: true,
            semantic_types: vec!["card".into()],
            ..Default::default()
        };
        let fresh = overview_response(&conn, &options)?;
        let deterministic_areas = fresh.overview.areas.len();
        let overlay = fresh.semantic_overlay.as_ref().expect("overlay requested");
        assert_eq!(overlay.returned, 1);
        assert_eq!(overlay.excluded_non_fresh, 0);
        assert_eq!(overlay.artifacts[0].freshness, "fresh");
        assert_eq!(
            fresh.response_budget.unbudgeted_bytes,
            fresh.response_budget.rendered_bytes
        );

        let deterministic = overview_response(
            &conn,
            &OverviewOptions {
                include_semantic: false,
                ..options.clone()
            },
        )?;
        let bounded_limit = deterministic.response_budget.rendered_bytes + 512;
        assert!(fresh.response_budget.rendered_bytes > bounded_limit);

        let bounded = overview_response(
            &conn,
            &OverviewOptions {
                response_byte_limit: bounded_limit,
                ..options.clone()
            },
        )?;
        assert!(bounded.response_budget.truncated);
        assert_eq!(bounded.response_budget.omitted_semantic_artifacts, 1);
        assert!(
            bounded
                .semantic_overlay
                .as_ref()
                .is_some_and(|overlay| overlay.artifacts.is_empty())
        );
        assert_eq!(bounded.overview.areas.len(), deterministic_areas);
        assert!(bounded.response_budget.rendered_bytes <= bounded_limit);

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
