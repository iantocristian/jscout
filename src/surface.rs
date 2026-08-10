use std::collections::{BTreeMap, HashMap, HashSet};

use anyhow::{Result, bail};
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;

use crate::{file_role, origin, structural};

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
    let allowed_types: HashSet<&str> = options.entity_types.iter().map(String::as_str).collect();
    let allowed_roles: HashSet<&str> = options.roles.iter().map(String::as_str).collect();
    let allowed_file_roles: HashSet<&str> =
        options.file_roles.iter().map(String::as_str).collect();
    let allowed_origins: HashSet<&str> =
        options.file_origins.iter().map(String::as_str).collect();
    let query = options.query.to_ascii_lowercase();

    let mut stmt = conn.prepare(
        "SELECT id, entity_key, plane, entity_type, name, identity_anchor, meta_json
         FROM entities ORDER BY plane, entity_type, name, entity_key",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
            row.get::<_, String>(6)?,
        ))
    })?;
    let mut candidates = Vec::new();
    for row in rows {
        let (id, key, plane, entity_type, name, identity_anchor, meta) = row?;
        if (!allowed_planes.is_empty() && !allowed_planes.contains(plane.as_str()))
            || (!allowed_types.is_empty() && !allowed_types.contains(entity_type.as_str()))
            || (!query.is_empty()
                && !name.to_ascii_lowercase().contains(&query)
                && !key.to_ascii_lowercase().contains(&query))
        {
            continue;
        }
        let occurrences = load_occurrences(
            conn,
            id,
            &allowed_roles,
            &allowed_file_roles,
            &allowed_origins,
        )?;
        if occurrences.is_empty() {
            continue;
        }
        let exact = !query.is_empty() && name.eq_ignore_ascii_case(&options.query);
        candidates.push((
            exact,
            EntityRecord {
                anchor: key,
                plane,
                entity_type,
                name,
                identity_anchor,
                occurrence_count: occurrences.len(),
                occurrences: occurrences
                    .iter()
                    .take(options.occurrences_per_entity)
                    .cloned()
                    .collect(),
                occurrences_truncated: occurrences.len() > options.occurrences_per_entity,
                meta: serde_json::from_str(&meta).unwrap_or(Value::Null),
            },
        ));
    }
    candidates.sort_by(|(left_exact, left), (right_exact, right)| {
        right_exact
            .cmp(left_exact)
            .then_with(|| right.occurrence_count.cmp(&left.occurrence_count))
            .then_with(|| {
                (&left.plane, &left.entity_type, &left.name, &left.anchor).cmp(&(
                    &right.plane,
                    &right.entity_type,
                    &right.name,
                    &right.anchor,
                ))
            })
    });
    let matched_entities = candidates.len();
    let entities = candidates
        .into_iter()
        .take(options.limit)
        .map(|(_, entity)| entity)
        .collect();
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
    allowed_roles: &HashSet<&str>,
    allowed_file_roles: &HashSet<&str>,
    allowed_origins: &HashSet<&str>,
) -> Result<Vec<EntityOccurrence>> {
    let mut stmt = conn.prepare(
        "SELECT file.path, file.role, file.origin,
                occurrence.line, occurrence.end_line,
                occurrence.start, occurrence.end, occurrence.role,
                occurrence.confidence, occurrence.extractor,
                occurrence.provenance, occurrence.detail_json
         FROM entity_occurrences occurrence
         JOIN files file ON file.id=occurrence.file_id
         WHERE occurrence.entity_id=?1
         ORDER BY occurrence.confidence='certain' DESC,
                  occurrence.confidence='likely' DESC,
                  file.path, occurrence.start, occurrence.id",
    )?;
    let rows = stmt.query_map([entity_id], |row| {
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
    })?;
    let mut occurrences = Vec::new();
    for row in rows {
        let occurrence = row?;
        if (!allowed_roles.is_empty() && !allowed_roles.contains(occurrence.role.as_str()))
            || (!allowed_file_roles.is_empty()
                && !allowed_file_roles.contains(occurrence.file_role.as_str()))
            || !allowed_origins.contains(occurrence.file_origin.as_str())
        {
            continue;
        }
        occurrences.push(occurrence);
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
    pub relations: Vec<RelationCount>,
    pub relations_truncated: bool,
}

pub fn overview(
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
        let area = areas.entry(area_path.clone()).or_insert_with(|| AreaOverview {
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
        relations,
        relations_truncated,
    })
}

fn repository_area(path: &str) -> String {
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [root @ ("packages" | "apps" | "services"), scope, name, ..]
            if scope.starts_with('@') => format!("{root}/{scope}/{name}"),
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

    use super::{EntityLookupOptions, entities, overview};
    use crate::{indexer, store};

    #[test]
    fn entity_lookup_filters_evidence_and_overview_is_bounded() -> Result<()> {
        let repo = tempfile::tempdir()?;
        fs::create_dir_all(repo.path().join("packages/api/src"))?;
        fs::write(
            repo.path().join("packages/api/src/main.ts"),
            "export function run() { return process.env.API_KEY; }\n",
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

        let overview = overview(&conn, &crate::origin::defaults(), 1, 2)?;
        assert_eq!(overview.areas.len(), 1);
        assert_eq!(overview.areas[0].path, "packages/api");
        assert!(overview.relations.len() <= 2);
        assert_eq!(overview.totals["files"], 2);
        Ok(())
    }
}
