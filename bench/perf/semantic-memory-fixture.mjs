export const SEMANTIC_MEMORY_FIXTURE_VERSION = 'semantic-memory-ai-pipe-v1';

export const DEFAULT_SEMANTIC_SCALES = Object.freeze([1_000, 5_000, 25_000]);
export const DEFAULT_SEMANTIC_SAMPLES = 20;
export const DEFAULT_SEMANTIC_WARMUPS = 3;
export const SEMANTIC_HISTORY_DIVISOR = 5;
export const SEMANTIC_SUPPORTS_PER_GENERATED_ARTIFACT = 4;
export const SEMANTIC_RELATION_COUNT = 40;
export const SEMANTIC_RUN_INDEX_NAME = 'candidate_semantic_artifacts_scout_run';

const keywordSql = (lineage) => `CASE (${lineage} % 8)
    WHEN 0 THEN 'quartz'
    WHEN 1 THEN 'zephyr'
    WHEN 2 THEN 'topaz'
    WHEN 3 THEN 'cobalt'
    WHEN 4 THEN 'saffron'
    WHEN 5 THEN 'indigo'
    WHEN 6 THEN 'willow'
    ELSE 'ember'
  END`;

function positiveInteger(value, label) {
  if (!Number.isSafeInteger(value) || value < 1) {
    throw new Error(`${label} must be a positive safe integer`);
  }
  return value;
}

function sqlValue(value) {
  if (value === null || value === undefined) return 'NULL';
  if (typeof value === 'number') return String(value);
  return `'${String(value).replaceAll("'", "''")}'`;
}

export function parseSemanticScales(value) {
  const scales = String(value)
    .split(',')
    .map((item) => Number(item.trim()))
    .filter((item) => !Number.isNaN(item));
  if (scales.length === 0) throw new Error('semantic scales must not be empty');
  for (const scale of scales) {
    positiveInteger(scale, 'semantic scale');
    if (scale < 256) throw new Error('semantic scales must be at least 256 artifacts');
  }
  return [...new Set(scales)].toSorted((left, right) => left - right);
}

export function semanticFixtureShape({
  currentArtifacts,
  templateArtifactId,
  templateRunId,
  templateSupports,
  supportTemplates,
}) {
  positiveInteger(currentArtifacts, 'currentArtifacts');
  positiveInteger(templateArtifactId, 'templateArtifactId');
  positiveInteger(templateRunId, 'templateRunId');
  positiveInteger(templateSupports, 'templateSupports');
  positiveInteger(supportTemplates, 'supportTemplates');
  if (supportTemplates < SEMANTIC_SUPPORTS_PER_GENERATED_ARTIFACT) {
    throw new Error('supportTemplates must cover every generated support slot');
  }
  if (supportTemplates % SEMANTIC_SUPPORTS_PER_GENERATED_ARTIFACT !== 0) {
    throw new Error('supportTemplates must be divisible by generated supports per artifact');
  }
  if (currentArtifacts < 256) throw new Error('currentArtifacts must be at least 256');
  const generatedLineages = currentArtifacts - 1;
  const historyArtifacts = Math.floor(generatedLineages / SEMANTIC_HISTORY_DIVISOR);
  const generatedRows = generatedLineages + historyArtifacts;
  const totalArtifacts = currentArtifacts + historyArtifacts;
  const detailArtifactId = templateArtifactId + generatedRows;
  return Object.freeze({
    currentArtifacts,
    generatedLineages,
    historyArtifacts,
    totalArtifacts,
    totalRuns: totalArtifacts,
    totalSupports:
      templateSupports + generatedRows * SEMANTIC_SUPPORTS_PER_GENERATED_ARTIFACT,
    totalRelations: SEMANTIC_RELATION_COUNT,
    quartzArtifacts: Math.floor(generatedLineages / 8),
    templateArtifactId,
    templateRunId,
    templateSupports,
    supportTemplates,
    anchorScopedArtifacts: 1 + Math.ceil(
      generatedLineages / (supportTemplates / SEMANTIC_SUPPORTS_PER_GENERATED_ARTIFACT),
    ),
    detailArtifactId,
    lastGeneratedRunId: templateRunId + generatedRows,
  });
}

export function semanticFixtureSql(shape, supportTemplates) {
  const {
    generatedLineages,
    historyArtifacts,
    templateArtifactId,
    templateRunId,
  } = shape;
  if (supportTemplates.length !== shape.supportTemplates) {
    throw new Error(
      `expected ${shape.supportTemplates} support templates, got ${supportTemplates.length}`,
    );
  }
  const supportTemplateValues = supportTemplates.map((support, index) => `(
    ${index + 1}, ${sqlValue(support.anchor_key)}, ${sqlValue(support.role)},
    ${sqlValue(support.evidence_file)}, ${sqlValue(support.evidence_start_line)},
    ${sqlValue(support.evidence_end_line)}, ${sqlValue(support.source_hash)},
    ${sqlValue(support.context_hash)}, ${sqlValue(support.confidence)}
  )`).join(',\n');
  const artifactId = `(n + ${templateArtifactId})`;
  const successorArtifactId = `(n + ${templateArtifactId + generatedLineages})`;
  const runId = `(n + ${templateRunId})`;
  const successorRunId = `(n + ${templateRunId + generatedLineages})`;
  const baseKeyword = keywordSql('n');

  return `PRAGMA foreign_keys=ON;
BEGIN IMMEDIATE;
CREATE TEMP TABLE semantic_fixture_support(
  slot INTEGER PRIMARY KEY,
  anchor_key TEXT NOT NULL,
  role TEXT,
  evidence_file TEXT NOT NULL,
  evidence_start_line INTEGER NOT NULL,
  evidence_end_line INTEGER NOT NULL,
  source_hash TEXT NOT NULL,
  context_hash TEXT NOT NULL,
  confidence TEXT NOT NULL
);
INSERT INTO semantic_fixture_support VALUES
${supportTemplateValues};

WITH RECURSIVE sequence(n) AS (
  VALUES(1)
  UNION ALL SELECT n + 1 FROM sequence WHERE n < ${generatedLineages}
)
INSERT INTO scout_runs(
  id, scout_kind, status, gateway_protocol, provider, model, billing_path,
  reasoning, prompt_version, source_snapshot, input_fingerprint, request_hash,
  config_json, usage_json, error_code, started_at, completed_at
)
SELECT ${runId}, 'annotation',
       CASE WHEN n <= ${historyArtifacts} THEN 'superseded' ELSE 'completed' END,
       1, 'benchmark', 'deterministic-semantic-fixture-v1', 'api', 'low',
       'semantic-memory-fixture-v1', (SELECT value FROM meta WHERE key='snapshot'),
       printf('semantic-fixture-input-%08d', n),
       printf('semantic-fixture-request-%08d-v1', n), '{}',
       '{"input_tokens":0,"output_tokens":0,"total_tokens":0,"cost_total":0}',
       NULL, '2026-08-22T00:00:00.000Z', '2026-08-22T00:00:00.000Z'
FROM sequence;

WITH RECURSIVE sequence(n) AS (
  VALUES(1)
  UNION ALL SELECT n + 1 FROM sequence WHERE n < ${historyArtifacts}
)
INSERT INTO scout_runs(
  id, scout_kind, status, gateway_protocol, provider, model, billing_path,
  reasoning, prompt_version, source_snapshot, input_fingerprint, request_hash,
  config_json, usage_json, error_code, started_at, completed_at
)
SELECT ${successorRunId}, 'annotation', 'completed', 1, 'benchmark',
       'deterministic-semantic-fixture-v1', 'api', 'low',
       'semantic-memory-fixture-v1', (SELECT value FROM meta WHERE key='snapshot'),
       printf('semantic-fixture-input-%08d', n),
       printf('semantic-fixture-request-%08d-v2', n), '{}',
       '{"input_tokens":0,"output_tokens":0,"total_tokens":0,"cost_total":0}',
       NULL, '2026-08-22T00:00:00.000Z', '2026-08-22T00:00:00.000Z'
FROM sequence;

WITH RECURSIVE sequence(n) AS (
  VALUES(1)
  UNION ALL SELECT n + 1 FROM sequence WHERE n < ${generatedLineages}
)
INSERT INTO semantic_artifacts(
  id, supersedes_artifact_id, artifact_type, canonical_name, body_json, model,
  prompt_version, confidence, source_snapshot, created_at, scout_run_id,
  input_fingerprint, artifact_fingerprint
)
SELECT ${artifactId}, NULL, 'annotation',
       printf('semantic-fixture-%08d', n),
       json_object('claims', json_array(
         printf('%s deterministic semantic memory lineage %08d', ${baseKeyword}, n),
         printf('indexed persistence evidence for lineage %08d', n),
         printf('bounded retrieval evidence for lineage %08d', n),
         printf('historical semantic evidence for lineage %08d', n)
       )),
       'deterministic-semantic-fixture-v1', 'semantic-memory-fixture-v1',
       'likely', (SELECT value FROM meta WHERE key='snapshot'),
       '2026-08-22T00:00:00.000Z', ${runId},
       printf('semantic-fixture-input-%08d', n),
       printf('semantic-fixture-artifact-%08d-v1', n)
FROM sequence;

WITH RECURSIVE sequence(n) AS (
  VALUES(1)
  UNION ALL SELECT n + 1 FROM sequence WHERE n < ${historyArtifacts}
)
INSERT INTO semantic_artifacts(
  id, supersedes_artifact_id, artifact_type, canonical_name, body_json, model,
  prompt_version, confidence, source_snapshot, created_at, scout_run_id,
  input_fingerprint, artifact_fingerprint
)
SELECT ${successorArtifactId}, ${artifactId}, 'annotation',
       printf('semantic-fixture-%08d', n),
       json_object('claims', json_array(
         printf('%s deterministic semantic memory lineage %08d revised', ${baseKeyword}, n),
         printf('indexed persistence evidence for lineage %08d revised', n),
         printf('bounded retrieval evidence for lineage %08d revised', n),
         printf('historical semantic evidence for lineage %08d revised', n)
       )),
       'deterministic-semantic-fixture-v1', 'semantic-memory-fixture-v1',
       'likely', (SELECT value FROM meta WHERE key='snapshot'),
       '2026-08-22T00:00:00.000Z', ${successorRunId},
       printf('semantic-fixture-input-%08d', n),
       printf('semantic-fixture-artifact-%08d-v2', n)
FROM sequence;

WITH RECURSIVE lineages(n) AS (
  VALUES(1)
  UNION ALL SELECT n + 1 FROM lineages WHERE n < ${generatedLineages}
), history(n) AS (
  VALUES(1)
  UNION ALL SELECT n + 1 FROM history WHERE n < ${historyArtifacts}
), artifact_ids(id, lineage) AS (
  SELECT ${templateArtifactId} + n, n FROM lineages
  UNION ALL
  SELECT ${templateArtifactId + generatedLineages} + n, n FROM history
), support_slots(slot) AS (VALUES(0), (1), (2), (3))
INSERT INTO semantic_supports(
  artifact_id, claim_path, anchor_key, role, evidence_file,
  evidence_start_line, evidence_end_line, source_hash, context_hash, confidence
)
SELECT artifact_ids.id, printf('/claims/%d', support_slots.slot),
       template.anchor_key, template.role, template.evidence_file,
       template.evidence_start_line, template.evidence_end_line,
       template.source_hash, template.context_hash, template.confidence
FROM artifact_ids
CROSS JOIN support_slots
JOIN semantic_fixture_support template
  ON template.slot=((artifact_ids.lineage - 1) * ${SEMANTIC_SUPPORTS_PER_GENERATED_ARTIFACT}
                    + support_slots.slot) % ${shape.supportTemplates} + 1;

WITH RECURSIVE relation_slots(slot) AS (
  VALUES(0)
  UNION ALL SELECT slot + 1 FROM relation_slots
  WHERE slot + 1 < ${SEMANTIC_RELATION_COUNT}
)
INSERT INTO semantic_relations(
  src_artifact_id, dst_artifact_id, relation, claim_path, confidence,
  dst_fingerprint
)
SELECT ${shape.detailArtifactId},
       ${templateArtifactId + historyArtifacts + 1} + relation_slots.slot,
       'related_to', printf('/claims/%d', relation_slots.slot % 4), 'likely',
       child.artifact_fingerprint
FROM relation_slots
JOIN semantic_artifacts child
  ON child.id=${templateArtifactId + historyArtifacts + 1} + relation_slots.slot;

DROP TABLE semantic_fixture_support;
COMMIT;
PRAGMA wal_checkpoint(TRUNCATE);
`;
}

export function semanticRunIndexSql() {
  return `CREATE INDEX ${SEMANTIC_RUN_INDEX_NAME}
ON semantic_artifacts(scout_run_id);
PRAGMA wal_checkpoint(TRUNCATE);`;
}

export function semanticMemoryCases(shape, support) {
  const broad = {
    vector: false,
    limit: 20,
    supports_per_artifact: 2,
    response_bytes: 24_000,
    debug: false,
  };
  const detail = {
    vector: false,
    artifact: shape.detailArtifactId,
    supports_per_artifact: 4,
    response_bytes: 128_000,
    debug: false,
  };
  return Object.freeze([
    Object.freeze({
      id: 'recent',
      arguments: Object.freeze({ ...broad, query: '' }),
      expectedCandidates: shape.currentArtifacts,
      expectedHandles: 20,
    }),
    Object.freeze({
      id: 'lexical-common',
      arguments: Object.freeze({ ...broad, query: 'deterministic' }),
      expectedCandidates: shape.currentArtifacts,
      expectedHandles: 20,
    }),
    Object.freeze({
      id: 'lexical-selective',
      arguments: Object.freeze({ ...broad, query: 'quartz' }),
      expectedCandidates: shape.quartzArtifacts,
      expectedHandles: Math.min(20, shape.quartzArtifacts),
    }),
    Object.freeze({
      id: 'lexical-miss',
      arguments: Object.freeze({ ...broad, query: 'unfindablefixturetoken' }),
      expectedCandidates: 0,
      expectedHandles: 0,
    }),
    Object.freeze({
      id: 'anchor-scoped',
      arguments: Object.freeze({ ...broad, query: '', anchor: support.anchor_key }),
      expectedCandidates: shape.anchorScopedArtifacts,
      expectedHandles: 20,
    }),
    Object.freeze({
      id: 'related-to',
      arguments: Object.freeze({ ...broad, query: '', related_to: shape.detailArtifactId }),
      expectedCandidates: shape.totalRelations,
      expectedHandles: 20,
    }),
    Object.freeze({
      id: 'artifact-body',
      arguments: Object.freeze({ ...detail, view: 'body' }),
      expectedCandidates: 1,
      expectedArtifacts: 1,
    }),
    Object.freeze({
      id: 'artifact-full-source',
      arguments: Object.freeze({
        ...detail,
        view: 'full',
        include_source: true,
        source_limit: 4,
        source_depth: 2,
        source_bytes: 2_000,
        relation_limit: SEMANTIC_RELATION_COUNT,
      }),
      expectedCandidates: 1,
      expectedArtifacts: 1,
      expectedRelations: SEMANTIC_RELATION_COUNT,
      expectedSources: 4,
    }),
  ]);
}
