#!/usr/bin/env node

import {
  existsSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { cpus, freemem, platform, release, totalmem } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  McpClient,
  backupDatabase,
  childEnvironment,
  commandOutput,
  distribution,
  fileMetadata,
  isExecutable,
  makeWorkspace,
  parseTextToolResponse,
  refusePathWithin,
  run,
  sha256Bytes,
  stageGitArchive,
  stopChildren,
  writeJsonExclusive,
} from './lib.mjs';
import {
  AI_PIPE_REVISION,
  CORPUS_INVARIANTS,
  SCOUT_CARD_ANCHOR,
} from './ai-pipe-fixture.mjs';
import {
  DEFAULT_SEMANTIC_SAMPLES,
  DEFAULT_SEMANTIC_SCALES,
  DEFAULT_SEMANTIC_WARMUPS,
  SEMANTIC_MEMORY_FIXTURE_VERSION,
  SEMANTIC_RUN_INDEX_NAME,
  parseSemanticScales,
  semanticFixtureShape,
  semanticFixtureSql,
  semanticMemoryCases,
  semanticRunIndexSql,
} from './semantic-memory-fixture.mjs';

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(scriptDirectory, '../..');
const mockGateway = join(scriptDirectory, 'mock-gateway.mjs');

const usage = `Deterministic semantic-memory scale and index benchmark.

Usage:
  node bench/perf/semantic-memory.mjs --repo PATH --output FILE [options]

Options:
  --binary PATH       JScout release binary (default: target/release/jscout)
  --revision SHA      ai-pipe revision (default: pinned fixture revision)
  --scales LIST       current-artifact counts (default: ${DEFAULT_SEMANTIC_SCALES.join(',')})
  --samples N         measured passes per semantic case (default: ${DEFAULT_SEMANTIC_SAMPLES})
  --warmups N         warmup passes per semantic case (default: ${DEFAULT_SEMANTIC_WARMUPS})
  --sqlite PATH       sqlite3 executable (default: sqlite3)
  --keep-workdir      retain staged corpus and databases
  --force             overwrite an existing output JSON file
  --help              show this text
`;

function parseArguments(argv) {
  const options = {
    binary: join(projectRoot, 'target/release/jscout'),
    revision: AI_PIPE_REVISION,
    scales: DEFAULT_SEMANTIC_SCALES,
    samples: DEFAULT_SEMANTIC_SAMPLES,
    warmups: DEFAULT_SEMANTIC_WARMUPS,
    sqlite: 'sqlite3',
    keepWorkdir: false,
    force: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--help') return { help: true };
    if (argument === '--keep-workdir') {
      options.keepWorkdir = true;
      continue;
    }
    if (argument === '--force') {
      options.force = true;
      continue;
    }
    const key = {
      '--repo': 'repo',
      '--output': 'output',
      '--binary': 'binary',
      '--revision': 'revision',
      '--scales': 'scales',
      '--samples': 'samples',
      '--warmups': 'warmups',
      '--sqlite': 'sqlite',
    }[argument];
    if (!key) throw new Error(`unknown argument: ${argument}\n\n${usage}`);
    const value = argv[++index];
    if (value === undefined) throw new Error(`missing value for ${argument}`);
    options[key] = value;
  }
  if (!options.repo || !options.output) throw new Error(`--repo and --output are required\n\n${usage}`);
  options.scales = Array.isArray(options.scales)
    ? [...options.scales]
    : parseSemanticScales(options.scales);
  for (const [key, minimum] of [['samples', 1], ['warmups', 0]]) {
    options[key] = Number(options[key]);
    if (!Number.isSafeInteger(options[key]) || options[key] < minimum) {
      throw new Error(`${key} must be an integer >= ${minimum}`);
    }
  }
  return options;
}

function assertEqual(actual, expected, label) {
  if (actual !== expected) throw new Error(`${label}: expected ${expected}, got ${actual}`);
}

function sqlString(value) {
  return `'${String(value).replaceAll("'", "''")}'`;
}

function sqliteJson(context, database, sql) {
  const text = commandOutput(context.sqlite, ['-json', database, sql], { env: context.env });
  return text ? JSON.parse(text) : [];
}

function sqliteExplainJson(context, database, sql) {
  const text = commandOutput(
    context.sqlite,
    ['-json', database, '.explain off', `EXPLAIN QUERY PLAN ${sql}`],
    { env: context.env },
  );
  return text ? JSON.parse(text) : [];
}

function integrityCheck(context, database) {
  const result = commandOutput(context.sqlite, [database, 'PRAGMA integrity_check;'], {
    env: context.env,
  });
  if (result !== 'ok') throw new Error(`SQLite integrity check failed for ${database}: ${result}`);
  return result;
}

function removeDatabaseFamily(context, database) {
  if (context.keepWorkdir) return;
  for (const suffix of ['', '-wal', '-shm', '-journal']) rmSync(`${database}${suffix}`, { force: true });
}

function assertActive(context) {
  if (!context.interruptedSignal()) return;
  const error = new Error(`benchmark interrupted by ${context.interruptedSignal()}`);
  error.code = 'BENCHMARK_INTERRUPTED';
  throw error;
}

function sampledMetric(samples, timing, unit = 'ms') {
  return { unit, timing, samples, summary: distribution(samples) };
}

function measurement(id, parameters, metrics, counters = {}, validation = {}) {
  return { id, parameters, metrics, counters, validation };
}

function databasePages(context, database) {
  const [row] = sqliteJson(context, database, `
    SELECT
      (SELECT page_count FROM pragma_page_count) AS page_count,
      (SELECT page_size FROM pragma_page_size) AS page_size,
      (SELECT freelist_count FROM pragma_freelist_count) AS freelist_count;
  `);
  return {
    ...row,
    active_bytes: (row.page_count - row.freelist_count) * row.page_size,
    file_bytes: fileMetadata(database).bytes,
  };
}

function databaseCounters(context, database) {
  return sqliteJson(context, database, `
    SELECT
      (SELECT count(*) FROM files) AS files,
      (SELECT count(*) FROM chunks) AS chunks,
      (SELECT count(*) FROM symbols) AS symbols,
      (SELECT count(*) FROM refs) AS refs,
      (SELECT count(*) FROM graph_nodes) AS graph_nodes,
      (SELECT count(*) FROM resolved_edges) AS resolved_edges,
      (SELECT count(*) FROM scout_runs) AS scout_runs,
      (SELECT count(*) FROM semantic_artifacts) AS semantic_artifacts,
      (SELECT count(*) FROM semantic_artifacts artifact WHERE NOT EXISTS(
         SELECT 1 FROM semantic_artifacts successor
         WHERE successor.supersedes_artifact_id=artifact.id
       )) AS current_artifacts,
      (SELECT count(*) FROM semantic_supports) AS semantic_supports,
      (SELECT count(*) FROM semantic_relations) AS semantic_relations,
      (SELECT coalesce(sum(id), 0) FROM semantic_artifacts) AS artifact_id_sum,
      (SELECT coalesce(sum(id), 0) FROM scout_runs) AS run_id_sum;
  `)[0];
}

function indexDatabase(context, database) {
  const result = run(context.binary, [
    '--config', context.config,
    'index', context.corpus,
    '--database', database,
    '--no-deps',
  ], { env: context.env, timeoutMs: 5 * 60_000 });
  const match = result.stdout.match(
    /indexed (\d+) files \(removed=(\d+), rejected=(\d+)\) — (\d+) chunks, (\d+) refs/,
  );
  if (!match) throw new Error(`missing indexing report: ${result.stdout}`);
  const report = {
    indexed: Number(match[1]),
    removed: Number(match[2]),
    rejected: Number(match[3]),
    chunks: Number(match[4]),
    refs: Number(match[5]),
  };
  assertEqual(report.indexed, CORPUS_INVARIANTS.indexed_files, 'indexed files');
  assertEqual(report.removed, 0, 'removed files');
  assertEqual(report.rejected, CORPUS_INVARIANTS.rejected_files, 'rejected files');
  assertEqual(report.chunks, CORPUS_INVARIANTS.chunks, 'indexed chunks');
  assertEqual(report.refs, CORPUS_INVARIANTS.references, 'indexed references');
  const counters = databaseCounters(context, database);
  assertEqual(counters.files, CORPUS_INVARIANTS.indexed_files, 'database files');
  assertEqual(counters.chunks, CORPUS_INVARIANTS.chunks, 'database chunks');
  assertEqual(counters.symbols, CORPUS_INVARIANTS.symbols, 'database symbols');
  assertEqual(counters.refs, CORPUS_INVARIANTS.references, 'database references');
  assertEqual(counters.graph_nodes, CORPUS_INVARIANTS.graph_nodes, 'database graph nodes');
  assertEqual(counters.resolved_edges, CORPUS_INVARIANTS.graph_edges, 'database graph edges');
  return { result, report, counters, integrity: integrityCheck(context, database) };
}

function publishTemplate(context, database) {
  const args = [
    '--config', context.config,
    'scout', 'cards', context.corpus,
    '--database', database,
    '--gateway-path', mockGateway,
    '--anchor', SCOUT_CARD_ANCHOR,
    '--max-calls', '1', '--reasoning', 'low',
  ];
  const publication = run(context.binary, args, { env: context.env, timeoutMs: 60_000 });
  const calls = Number(publication.stdout.match(/model calls: (\d+)/)?.[1]);
  assertEqual(calls, 1, 'template model calls');
  const [template] = sqliteJson(context, database, `
    SELECT artifact.id AS artifact_id, artifact.scout_run_id AS run_id,
           run.input_fingerprint,
           (SELECT count(*) FROM semantic_supports
            WHERE artifact_id=artifact.id) AS supports
    FROM semantic_artifacts artifact
    JOIN scout_runs run ON run.id=artifact.scout_run_id
    WHERE artifact.artifact_type='card'
    ORDER BY artifact.id LIMIT 1;
  `);
  if (!template) throw new Error('fake gateway did not publish a semantic card');
  const [support] = sqliteJson(context, database, `
    SELECT anchor_key, role, evidence_file, evidence_start_line, evidence_end_line,
           source_hash, context_hash, confidence
    FROM semantic_supports
    WHERE artifact_id=${Number(template.artifact_id)}
    ORDER BY claim_path, anchor_key, evidence_file, evidence_start_line
    LIMIT 1;
  `);
  if (!support) throw new Error('semantic template omitted support evidence');
  assertEqual(support.anchor_key, SCOUT_CARD_ANCHOR, 'template support anchor');
  return {
    args,
    publication,
    artifactId: template.artifact_id,
    runId: template.run_id,
    inputFingerprint: template.input_fingerprint,
    supports: template.supports,
    support,
    integrity: integrityCheck(context, database),
  };
}

function buildSupportTemplates(context, database, template) {
  const candidates = sqliteJson(context, database, `
    WITH file_anchors AS (
      SELECT graph.node_key AS anchor_key, file.path AS evidence_file,
             graph.line AS evidence_line,
             row_number() OVER (
               PARTITION BY file.path ORDER BY graph.node_key
             ) AS file_rank
      FROM graph_nodes graph
      JOIN files file ON file.id=graph.file_id
      WHERE graph.node_key LIKE 'sym:%'
        AND graph.line > 0
        AND file.origin IN ('repository','workspace')
        AND graph.node_key<>${sqlString(template.support.anchor_key)}
    )
    SELECT anchor_key, evidence_file, evidence_line
    FROM file_anchors
    WHERE file_rank=1
    ORDER BY anchor_key
    LIMIT 31;
  `);
  assertEqual(candidates.length, 31, 'semantic support template candidates');
  const snapshot = sqliteJson(
    context,
    database,
    "SELECT value AS snapshot FROM meta WHERE key='snapshot';",
  )[0]?.snapshot;
  if (!snapshot) throw new Error('semantic support templates require a structural snapshot');
  const input = {
    type: 'annotation',
    name: 'deterministic semantic benchmark support templates',
    body: {
      claim: 'validated fixture support 1',
      claims: candidates.slice(1).map((_, index) => `validated fixture support ${index + 2}`),
    },
    supports: candidates.map((candidate, index) => ({
      claim_path: index === 0 ? '/claim' : `/claims/${index - 1}`,
      anchor: candidate.anchor_key,
      role: null,
      evidence_file: candidate.evidence_file,
      evidence_start_line: candidate.evidence_line,
      evidence_end_line: candidate.evidence_line,
      confidence: 'likely',
    })),
    confidence: 'likely',
    snapshot,
    supersedes: null,
  };
  const inputPath = join(context.workspace, 'semantic-support-templates.json');
  writeFileSync(inputPath, `${JSON.stringify(input, null, 2)}\n`);
  const publication = run(context.binary, [
    '--config', context.config,
    'annotate', context.corpus, inputPath,
    '--database', database,
  ], { env: context.env, timeoutMs: 60_000 });
  const value = JSON.parse(publication.stdout);
  const artifactId = value.artifact?.id ?? value.id;
  if (!Number.isSafeInteger(artifactId)) {
    throw new Error(`support template annotation omitted artifact id: ${publication.stdout}`);
  }
  const annotated = sqliteJson(context, database, `
    SELECT anchor_key, role, evidence_file, evidence_start_line, evidence_end_line,
           source_hash, context_hash, confidence
    FROM semantic_supports
    WHERE artifact_id=${artifactId}
    ORDER BY claim_path, anchor_key, evidence_file, evidence_start_line;
  `);
  assertEqual(annotated.length, 31, 'validated semantic support templates');
  run(context.sqlite, [database, `
    PRAGMA foreign_keys=ON;
    DELETE FROM semantic_artifacts WHERE id=${artifactId};
    PRAGMA wal_checkpoint(TRUNCATE);
  `], { env: context.env });
  const counters = databaseCounters(context, database);
  assertEqual(counters.semantic_artifacts, 1, 'support template cleanup artifacts');
  assertEqual(counters.scout_runs, 1, 'support template cleanup runs');
  assertEqual(counters.semantic_supports, template.supports, 'support template cleanup supports');
  return {
    publication,
    supports: [template.support, ...annotated],
    integrity: integrityCheck(context, database),
  };
}

function seedFixture(context, database, template, currentArtifacts) {
  const shape = semanticFixtureShape({
    currentArtifacts,
    templateArtifactId: template.artifactId,
    templateRunId: template.runId,
    templateSupports: template.supports,
    supportTemplates: context.supportTemplates.length,
  });
  run(context.sqlite, [database], {
    env: context.env,
    input: semanticFixtureSql(shape, context.supportTemplates),
    timeoutMs: 5 * 60_000,
  });
  const counters = databaseCounters(context, database);
  assertEqual(counters.semantic_artifacts, shape.totalArtifacts, 'fixture artifacts');
  assertEqual(counters.current_artifacts, shape.currentArtifacts, 'fixture current artifacts');
  assertEqual(counters.scout_runs, shape.totalRuns, 'fixture scout runs');
  assertEqual(counters.semantic_supports, shape.totalSupports, 'fixture supports');
  assertEqual(counters.semantic_relations, shape.totalRelations, 'fixture relations');
  const [freshness] = sqliteJson(context, database, `
    SELECT count(*) AS current_template_supports
    FROM semantic_supports support
    JOIN files file ON file.path=support.evidence_file
    WHERE support.source_hash=file.hash;
  `);
  assertEqual(freshness.current_template_supports, shape.totalSupports, 'fixture current source supports');
  return {
    shape,
    counters,
    pages: databasePages(context, database),
    integrity: integrityCheck(context, database),
  };
}

function semanticClient(context, database, name) {
  const client = new McpClient({
    binary: context.binary,
    args: [
      '--config', context.config,
      'mcp', context.corpus,
      '--database', database,
      '--profile', 'structural',
      '--result-transport', 'text',
    ],
    env: context.env,
    timeoutMs: 120_000,
  });
  context.children.add(client.child);
  client.benchmarkName = name;
  return client;
}

async function closeSemanticClient(context, client) {
  try {
    await client.close();
  } finally {
    context.children.delete(client.child);
  }
}

function validateSemanticResponse(testCase, value, diagnostic) {
  const detail = testCase.arguments.artifact !== undefined;
  assertEqual(value.mode, detail ? 'artifact_detail' : 'discovery', `${testCase.id} mode`);
  assertEqual(value.status, 'results', `${testCase.id} status`);
  if (diagnostic) {
    assertEqual(
      value.candidate_artifacts,
      testCase.expectedCandidates,
      `${testCase.id} candidate artifacts`,
    );
  }
  if (testCase.expectedHandles !== undefined) {
    assertEqual(value.artifact_handles?.length ?? 0, testCase.expectedHandles, `${testCase.id} handles`);
  }
  if (testCase.expectedArtifacts !== undefined) {
    assertEqual(
      value.semantic_artifacts?.length ?? 0,
      testCase.expectedArtifacts,
      `${testCase.id} artifacts`,
    );
    assertEqual(value.semantic_artifacts?.[0]?.freshness, 'fresh', `${testCase.id} freshness`);
  }
  if (testCase.expectedRelations !== undefined) {
    assertEqual(
      value.related_artifacts?.length ?? 0,
      testCase.expectedRelations,
      `${testCase.id} related artifacts`,
    );
  }
  if (testCase.expectedSources !== undefined) {
    assertEqual(
      value.source_evidence?.length ?? 0,
      testCase.expectedSources,
      `${testCase.id} source evidence`,
    );
    if (value.source_evidence.some((source) => source.source_status !== 'current-source')) {
      throw new Error(
        `${testCase.id} emitted non-current source evidence: ${JSON.stringify(
          value.source_evidence.map((source) => source.source_status),
        )}`,
      );
    }
  }
}

async function callSemanticCase(client, testCase, diagnostic = false) {
  const argumentsValue = diagnostic
    ? { ...testCase.arguments, debug: true }
    : testCase.arguments;
  const { response, elapsedMs } = await client.request('tools/call', {
    name: 'semantic_memory',
    arguments: argumentsValue,
  });
  const { text, value } = parseTextToolResponse(response, `semantic memory: ${testCase.id}`);
  validateSemanticResponse(testCase, value, diagnostic || testCase.arguments.view === 'full');
  return {
    elapsedMs,
    bytes: Buffer.byteLength(text),
    digest: sha256Bytes(text),
  };
}

async function runSemanticScale(context, database, fixture) {
  console.error(`benchmark: semantic memory with ${fixture.shape.currentArtifacts} current artifacts`);
  const cases = semanticMemoryCases(fixture.shape, context.template.support);
  const client = semanticClient(context, database, `scale-${fixture.shape.currentArtifacts}`);
  const rows = new Map(cases.map((testCase) => [testCase.id, []]));
  try {
    await client.initialize(`jscout-semantic-memory-${fixture.shape.currentArtifacts}`);
    for (const testCase of cases) {
      assertActive(context);
      await callSemanticCase(client, testCase, true);
    }
    const passes = context.warmups + context.samples;
    for (let pass = 0; pass < passes; pass += 1) {
      for (let offset = 0; offset < cases.length; offset += 1) {
        assertActive(context);
        const testCase = cases[(pass + offset) % cases.length];
        const row = await callSemanticCase(client, testCase);
        if (pass >= context.warmups) rows.get(testCase.id).push(row);
      }
    }
  } finally {
    await closeSemanticClient(context, client);
  }
  return cases.map((testCase) => {
    const samples = rows.get(testCase.id);
    const digests = new Set(samples.map((row) => row.digest));
    return measurement(
      `semantic_memory.${testCase.id}.scale_${fixture.shape.currentArtifacts}`,
      {
        current_artifacts: fixture.shape.currentArtifacts,
        history_artifacts: fixture.shape.historyArtifacts,
        total_artifacts: fixture.shape.totalArtifacts,
        samples: context.samples,
        warmups: context.warmups,
        arguments: testCase.arguments,
      },
      {
        roundtrip_ms: sampledMetric(samples.map((row) => row.elapsedMs), 'persistent MCP round trip'),
        result_bytes: sampledMetric(
          samples.map((row) => row.bytes),
          'serialized MCP text payload',
          'bytes',
        ),
      },
      {
        expected_candidates: testCase.expectedCandidates,
        returned_handles: testCase.expectedHandles,
        returned_artifacts: testCase.expectedArtifacts,
      },
      { stable_output: digests.size === 1, unique_response_hashes: digests.size },
    );
  });
}

function contentSignature(context, database) {
  return databaseCounters(context, database);
}

function explainRunQueries(context, database, template) {
  return {
    direct_run_lookup: sqliteExplainJson(
      context,
      database,
      `SELECT id FROM semantic_artifacts WHERE scout_run_id=${Number(template.runId)};`,
    ),
    reusable_run: sqliteExplainJson(context, database, `
      SELECT run.id FROM scout_runs run
      LEFT JOIN semantic_artifacts artifact ON artifact.scout_run_id=run.id
      WHERE run.scout_kind='card'
        AND run.input_fingerprint=${sqlString(template.inputFingerprint)}
        AND run.status='completed'
        AND (artifact.id IS NULL OR NOT EXISTS(
          SELECT 1 FROM semantic_artifacts successor
          WHERE successor.supersedes_artifact_id=artifact.id
        ));
    `),
  };
}

function lookupScript(shape, lookups) {
  const runIds = Array.from({ length: lookups }, (_, index) => {
    const generatedOffset = (index * 7_919) % (shape.totalRuns - 1);
    return shape.templateRunId + 1 + generatedOffset;
  });
  return `PRAGMA query_only=ON;\n.output /dev/null\n${runIds
    .map((runId) => `SELECT id FROM semantic_artifacts WHERE scout_run_id=${runId};`)
    .join('\n')}\n`;
}

function runLookupIndexExperiment(context, databases, fixture) {
  const lookupsPerSample = 2_000;
  const sampleCount = Math.max(10, Math.min(30, context.samples));
  const script = lookupScript(fixture.shape, lookupsPerSample);
  const rows = { baseline: [], indexed: [] };
  for (let pass = 0; pass < sampleCount + 2; pass += 1) {
    const order = pass % 2 === 0 ? ['baseline', 'indexed'] : ['indexed', 'baseline'];
    for (const arm of order) {
      assertActive(context);
      const result = run(context.sqlite, [databases[arm]], {
        env: context.env,
        input: script,
        timeoutMs: 5 * 60_000,
      });
      if (pass >= 2) rows[arm].push(result.elapsedMs);
    }
  }
  return ['baseline', 'indexed'].map((arm) => measurement(
    `semantic_index.run_lookup.${arm}`,
    {
      arm,
      candidate_index: arm === 'indexed' ? `${SEMANTIC_RUN_INDEX_NAME}(scout_run_id)` : null,
      artifact_rows: fixture.shape.totalArtifacts,
      lookups_per_sample: lookupsPerSample,
      samples: sampleCount,
      warmups: 2,
    },
    { wall_ms: sampledMetric(rows[arm], 'one sqlite3 process executing the lookup batch') },
    { successful_lookups_per_sample: lookupsPerSample },
    { paired_arm_order_rotated: true },
  ));
}

function cardCalls(stdout) {
  return Number(stdout.match(/model calls: (\d+)/)?.[1]);
}

function runReuseIndexExperiment(context, databases, template, fixture) {
  const sampleCount = Math.max(20, Math.min(50, context.samples * 2));
  const warmups = 5;
  const rows = { baseline: [], indexed: [] };
  for (let pass = 0; pass < sampleCount + warmups; pass += 1) {
    const order = pass % 2 === 0 ? ['baseline', 'indexed'] : ['indexed', 'baseline'];
    for (const arm of order) {
      assertActive(context);
      const args = template.args.map((argument, index, all) => (
        index > 0 && all[index - 1] === '--database' ? databases[arm] : argument
      ));
      const result = run(context.binary, args, { env: context.env, timeoutMs: 60_000 });
      assertEqual(cardCalls(result.stdout), 0, `${arm} card reuse model calls`);
      if (pass >= warmups) rows[arm].push(result.elapsedMs);
    }
  }
  return ['baseline', 'indexed'].map((arm) => measurement(
    `semantic_index.card_reuse.${arm}`,
    {
      arm,
      candidate_index: arm === 'indexed' ? `${SEMANTIC_RUN_INDEX_NAME}(scout_run_id)` : null,
      artifact_rows: fixture.shape.totalArtifacts,
      samples: sampleCount,
      warmups,
    },
    { wall_ms: sampledMetric(rows[arm], 'CLI process wall, including startup') },
    { model_calls_per_sample: 0 },
    { paired_arm_order_rotated: true, database_state_unchanged: true },
  ));
}

async function runSemanticIndexParity(context, databases, fixture) {
  const selected = new Set(['recent', 'lexical-selective', 'anchor-scoped']);
  const cases = semanticMemoryCases(fixture.shape, context.template.support)
    .filter((testCase) => selected.has(testCase.id));
  const clients = {
    baseline: semanticClient(context, databases.baseline, 'index-baseline'),
    indexed: semanticClient(context, databases.indexed, 'index-indexed'),
  };
  const rows = new Map();
  for (const testCase of cases) {
    rows.set(testCase.id, { baseline: [], indexed: [] });
  }
  const sampleCount = Math.max(10, Math.min(20, context.samples));
  try {
    await clients.baseline.initialize('jscout-semantic-index-baseline');
    await clients.indexed.initialize('jscout-semantic-index-indexed');
    for (const testCase of cases) {
      const baseline = await callSemanticCase(clients.baseline, testCase, true);
      const indexed = await callSemanticCase(clients.indexed, testCase, true);
      assertEqual(indexed.digest, baseline.digest, `${testCase.id} diagnostic index parity`);
    }
    for (let pass = 0; pass < sampleCount + 2; pass += 1) {
      for (let caseOffset = 0; caseOffset < cases.length; caseOffset += 1) {
        const testCase = cases[(pass + caseOffset) % cases.length];
        const order = (pass + caseOffset) % 2 === 0
          ? ['baseline', 'indexed']
          : ['indexed', 'baseline'];
        const pair = {};
        for (const arm of order) {
          assertActive(context);
          pair[arm] = await callSemanticCase(clients[arm], testCase);
          if (pass >= 2) rows.get(testCase.id)[arm].push(pair[arm]);
        }
        assertEqual(pair.indexed.digest, pair.baseline.digest, `${testCase.id} index parity`);
      }
    }
  } finally {
    await Promise.all(Object.values(clients).map((client) => closeSemanticClient(context, client)));
  }
  const measurements = [];
  for (const testCase of cases) {
    for (const arm of ['baseline', 'indexed']) {
      const samples = rows.get(testCase.id)[arm];
      measurements.push(measurement(
        `semantic_index.semantic_memory.${testCase.id}.${arm}`,
        {
          arm,
          candidate_index: arm === 'indexed' ? `${SEMANTIC_RUN_INDEX_NAME}(scout_run_id)` : null,
          artifact_rows: fixture.shape.totalArtifacts,
          samples: sampleCount,
          warmups: 2,
          arguments: testCase.arguments,
        },
        {
          roundtrip_ms: sampledMetric(samples.map((row) => row.elapsedMs), 'persistent MCP round trip'),
          result_bytes: sampledMetric(samples.map((row) => row.bytes), 'serialized MCP text payload', 'bytes'),
        },
        {},
        { byte_identical_across_index_arms: true, paired_arm_order_rotated: true },
      ));
    }
  }
  return measurements;
}

async function runCandidateIndexExperiment(context, baseline, template, fixture) {
  console.error(`benchmark: ${SEMANTIC_RUN_INDEX_NAME} A/B at ${fixture.shape.totalArtifacts} rows`);
  const indexed = join(context.workspace, 'semantic-index-indexed.db');
  backupDatabase(context.sqlite, baseline, indexed, context.env);
  const baselineSignature = contentSignature(context, baseline);
  const baselinePages = databasePages(context, baseline);
  const baselinePlans = explainRunQueries(context, baseline, template);
  run(context.sqlite, [indexed, semanticRunIndexSql()], { env: context.env });
  const indexedSignature = contentSignature(context, indexed);
  if (JSON.stringify(indexedSignature) !== JSON.stringify(baselineSignature)) {
    throw new Error('candidate index changed logical database content');
  }
  const indexedPages = databasePages(context, indexed);
  const indexedPlans = explainRunQueries(context, indexed, template);
  const databases = { baseline, indexed };
  const measurements = [
    measurement(
      'semantic_index.storage_and_plan',
      {
        candidate_index: `${SEMANTIC_RUN_INDEX_NAME}(scout_run_id)`,
        artifact_rows: fixture.shape.totalArtifacts,
      },
      {},
      {
        baseline_pages: baselinePages,
        indexed_pages: indexedPages,
        active_byte_delta: indexedPages.active_bytes - baselinePages.active_bytes,
        file_byte_delta: indexedPages.file_bytes - baselinePages.file_bytes,
      },
      {
        logical_content_unchanged: true,
        integrity_check: integrityCheck(context, indexed),
        query_plans: { baseline: baselinePlans, indexed: indexedPlans },
      },
    ),
    ...runLookupIndexExperiment(context, databases, fixture),
    ...runReuseIndexExperiment(context, databases, template, fixture),
    ...await runSemanticIndexParity(context, databases, fixture),
  ];
  const afterBaseline = contentSignature(context, baseline);
  const afterIndexed = contentSignature(context, indexed);
  if (
    JSON.stringify(afterBaseline) !== JSON.stringify(baselineSignature)
    || JSON.stringify(afterIndexed) !== JSON.stringify(indexedSignature)
  ) {
    throw new Error('candidate index experiment changed semantic fixture content');
  }
  removeDatabaseFamily(context, indexed);
  return measurements;
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.help) {
    process.stdout.write(usage);
    return;
  }
  options.repo = resolve(options.repo);
  options.output = resolve(options.output);
  options.binary = resolve(options.binary);
  if (options.revision !== AI_PIPE_REVISION) {
    throw new Error(`fixture is pinned to ${AI_PIPE_REVISION}; received ${options.revision}`);
  }
  if (!isExecutable(options.binary)) throw new Error(`JScout binary is not executable: ${options.binary}`);
  if (!existsSync(options.repo)) throw new Error(`repository not found: ${options.repo}`);
  if (existsSync(options.output) && !options.force) throw new Error(`output already exists: ${options.output}`);
  refusePathWithin(options.output, [options.repo, projectRoot, options.binary, mockGateway]);
  const binaryBefore = fileMetadata(options.binary);

  const workspace = makeWorkspace('jscout-semantic-memory-perf-');
  const children = new Set();
  let interruptedSignal = null;
  const terminateChildren = (signal) => {
    interruptedSignal ??= signal;
    process.exitCode = signal === 'SIGINT' ? 130 : 143;
    for (const child of children) child.kill('SIGTERM');
  };
  const onSigint = () => terminateChildren('SIGINT');
  const onSigterm = () => terminateChildren('SIGTERM');
  process.once('SIGINT', onSigint);
  process.once('SIGTERM', onSigterm);
  let sourceStatusBefore;
  try {
    const bootstrapEnv = childEnvironment(workspace);
    const resolvedRevision = commandOutput(
      'git', ['-C', options.repo, 'rev-parse', options.revision], { env: bootstrapEnv },
    );
    assertEqual(resolvedRevision, AI_PIPE_REVISION, 'ai-pipe revision');
    sourceStatusBefore = commandOutput(
      'git', ['-C', options.repo, 'status', '--porcelain=v2', '--untracked-files=all'],
      { env: bootstrapEnv },
    );
    const corpus = join(workspace, 'corpus');
    stageGitArchive(options.repo, resolvedRevision, corpus, bootstrapEnv);
    const trackedFiles = Number(commandOutput(
      'git', ['-C', options.repo, 'ls-tree', '-r', '--name-only', resolvedRevision],
      { env: bootstrapEnv },
    ).split('\n').filter(Boolean).length);
    assertEqual(trackedFiles, CORPUS_INVARIANTS.tracked_files, 'ai-pipe tracked files');

    const env = childEnvironment(workspace, { JSCOUT_TASK_ID: 'semantic-memory-performance-harness' });
    const config = join(workspace, 'semantic-memory.toml');
    // No embedding provider is configured: this benchmark isolates lexical
    // semantic-memory retrieval and SQLite persistence from model latency.
    writeFileSync(config, `version = 1

[diagnostics]
timing = false
debug = false

[search]
vector = false
rerank = false
attach_memory = false

[sidecars]
node = ${JSON.stringify(process.execPath)}
`);
    const context = {
      binary: options.binary,
      children,
      config,
      corpus,
      env,
      interruptedSignal: () => interruptedSignal,
      keepWorkdir: options.keepWorkdir,
      samples: options.samples,
      sqlite: options.sqlite,
      warmups: options.warmups,
      workspace,
    };
    commandOutput(options.sqlite, ['--version'], { env });
    assertEqual(sqliteJson(context, ':memory:', 'SELECT 1 AS value;')[0]?.value, 1, 'sqlite JSON preflight');
    const jscoutVersion = commandOutput(options.binary, ['--version'], { env });

    console.error('benchmark: pinned ai-pipe indexing');
    const structuralDatabase = join(workspace, 'structural.db');
    const indexed = indexDatabase(context, structuralDatabase);
    const templateDatabase = join(workspace, 'semantic-template.db');
    backupDatabase(options.sqlite, structuralDatabase, templateDatabase, env);
    removeDatabaseFamily(context, structuralDatabase);
    console.error('benchmark: deterministic semantic support template');
    const template = publishTemplate(context, templateDatabase);
    context.template = template;
    const supportTemplates = buildSupportTemplates(context, templateDatabase, template);
    context.supportTemplates = supportTemplates.supports;

    const measurements = [
      measurement(
        'semantic_fixture.index',
        { database: 'new', corpus_revision: resolvedRevision },
        { wall_ms: sampledMetric([indexed.result.elapsedMs], 'process wall, including startup') },
        indexed.counters,
        { index_report_matches: true, integrity_check: indexed.integrity },
      ),
      measurement(
        'semantic_fixture.template_publication',
        { fake_gateway: true, anchor: SCOUT_CARD_ANCHOR },
        { wall_ms: sampledMetric([template.publication.elapsedMs], 'process wall, including startup') },
        { model_calls: 1, supports: template.supports },
        { current_support_template: true, integrity_check: template.integrity },
      ),
      measurement(
        'semantic_fixture.support_templates',
        { annotation_validation: true },
        {
          wall_ms: sampledMetric(
            [supportTemplates.publication.elapsedMs],
            'process wall, including startup',
          ),
        },
        { validated_support_templates: supportTemplates.supports.length },
        { temporary_annotation_removed: true, integrity_check: supportTemplates.integrity },
      ),
    ];
    const fixtureRecords = [];
    let largestDatabase;
    let largestFixture;
    for (const scale of options.scales) {
      assertActive(context);
      const database = join(workspace, `semantic-${scale}.db`);
      backupDatabase(options.sqlite, templateDatabase, database, env);
      const fixture = seedFixture(context, database, template, scale);
      fixtureRecords.push({
        scale,
        shape: fixture.shape,
        counters: fixture.counters,
        pages: fixture.pages,
        integrity_check: fixture.integrity,
      });
      measurements.push(...await runSemanticScale(context, database, fixture));
      if (scale === options.scales.at(-1)) {
        largestDatabase = database;
        largestFixture = fixture;
      } else {
        removeDatabaseFamily(context, database);
      }
    }
    removeDatabaseFamily(context, templateDatabase);
    measurements.push(...await runCandidateIndexExperiment(
      context,
      largestDatabase,
      template,
      largestFixture,
    ));
    removeDatabaseFamily(context, largestDatabase);

    assertActive(context);
    const binaryAfter = fileMetadata(options.binary);
    if (binaryAfter.bytes !== binaryBefore.bytes || binaryAfter.sha256 !== binaryBefore.sha256) {
      throw new Error('JScout binary changed during benchmark');
    }
    const sourceStatusAfter = commandOutput(
      'git', ['-C', options.repo, 'status', '--porcelain=v2', '--untracked-files=all'],
      { env: bootstrapEnv },
    );
    if (sourceStatusAfter !== sourceStatusBefore) throw new Error('source repository status changed');
    const jscoutCommit = commandOutput('git', ['-C', projectRoot, 'rev-parse', 'HEAD'], { env });
    const jscoutTree = commandOutput('git', ['-C', projectRoot, 'rev-parse', 'HEAD^{tree}'], { env });
    const jscoutStatus = commandOutput(
      'git', ['-C', projectRoot, 'status', '--porcelain=v2', '--untracked-files=all'],
      { env },
    );
    const report = {
      schema: 'jscout.semantic-memory-performance.v1',
      generated_at: new Date().toISOString(),
      provenance: {
        harness_source_commit: jscoutCommit,
        harness_source_tree: jscoutTree,
        harness_source_dirty: jscoutStatus.length > 0,
        harness_files: {
          orchestrator: fileMetadata(fileURLToPath(import.meta.url)),
          fixture: fileMetadata(join(scriptDirectory, 'semantic-memory-fixture.mjs')),
          library: fileMetadata(join(scriptDirectory, 'lib.mjs')),
          ai_pipe_fixture: fileMetadata(join(scriptDirectory, 'ai-pipe-fixture.mjs')),
          mock_gateway: fileMetadata(mockGateway),
        },
        binary: {
          ...binaryBefore,
          version: jscoutVersion,
          stable_during_run: true,
          source_verification: 'operator-built from harness_source_commit before the recorded run',
        },
        corpus: 'ai-pipe',
        corpus_commit: resolvedRevision,
        corpus_tracked_files: trackedFiles,
        source_checkout_dirty: sourceStatusBefore.length > 0,
      },
      host: {
        os: platform(),
        os_release: release(),
        arch: process.arch,
        cpu: cpus()[0]?.model ?? null,
        logical_cpus: cpus().length,
        total_memory_bytes: totalmem(),
        free_memory_bytes_at_report: freemem(),
        node: process.version,
        rustc: commandOutput('rustc', ['--version'], { env }),
        sqlite: commandOutput(options.sqlite, ['--version'], { env }),
      },
      methodology: {
        fixture_version: SEMANTIC_MEMORY_FIXTURE_VERSION,
        scales: options.scales,
        samples_per_semantic_case: options.samples,
        warmups_per_semantic_case: options.warmups,
        requested_build_profile: 'release',
        binary_build_profile_verified: false,
        remote_model_requests: 0,
        semantic_vector_retrieval: false,
        corpus_staging: 'git archive of the pinned revision',
        filesystem_cache: 'warm/uncontrolled',
        timing: 'monotonic wall; MCP cases exclude process startup; CLI reuse includes it',
        candidate_index: `${SEMANTIC_RUN_INDEX_NAME}(scout_run_id)`,
      },
      fixtures: fixtureRecords,
      measurements,
    };
    assertActive(context);
    writeJsonExclusive(options.output, report, options.force);
    process.stdout.write(`${JSON.stringify(report, null, 2)}\n`);
    console.error(`benchmark: wrote ${options.output}`);
  } finally {
    await stopChildren(children);
    process.removeListener('SIGINT', onSigint);
    process.removeListener('SIGTERM', onSigterm);
    if (options.keepWorkdir) console.error(`benchmark: kept ${workspace}`);
    else rmSync(workspace, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error.stack ?? error.message);
  if (!process.exitCode) process.exitCode = 1;
});
