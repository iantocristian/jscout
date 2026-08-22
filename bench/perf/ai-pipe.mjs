#!/usr/bin/env node

import { spawn } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  rmSync,
  symlinkSync,
  utimesSync,
  writeFileSync,
} from 'node:fs';
import { cpus, freemem, platform, release, totalmem } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';

import {
  McpClient,
  backupDatabase,
  childEnvironment,
  commandOutput,
  distribution,
  fileMetadata,
  findPackageRoot,
  isExecutable,
  makeWorkspace,
  parseTextToolResponse,
  refusePathWithin,
  run,
  sha256Bytes,
  stageGitArchive,
  stopChild,
  stopChildren,
  writeJsonExclusive,
} from './lib.mjs';
import {
  AI_PIPE_REVISION,
  CORPUS_INVARIANTS,
  EMBEDDING_FIXTURE,
  ENRICHMENT_INVARIANTS,
  NEIGHBORHOOD_ANCHORS,
  NEIGHBORHOOD_CASES,
  SCOUT_CARD_ANCHOR,
  SCOUT_PLAN_INVARIANTS,
  SCOUT_WORKFLOW_SEED,
  SEARCH_LIMIT,
  SEARCH_QUERIES,
  WATCH_TOUCH_PATH,
} from './ai-pipe-fixture.mjs';

const scriptDirectory = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(scriptDirectory, '../..');
const mockInference = join(scriptDirectory, 'mock-inference.mjs');
const mockGateway = join(scriptDirectory, 'mock-gateway.mjs');

const usage = `Provider-free local performance harness for the pinned ai-pipe corpus.

Usage:
  node bench/perf/ai-pipe.mjs --repo PATH --output FILE [options]

Options:
  --binary PATH             JScout binary; use a release build (default: target/release/jscout)
  --revision SHA            ai-pipe revision (default: pinned fixture revision)
  --suite quick|baseline|full|LIST
                            quick: index, search, neighborhood, scout
                            baseline: quick + embedding
                            full: baseline + watch + enrichment
  --samples N               override repeatable measured sample/pass counts
  --warmups N               override search, neighborhood, and scout warmup passes
  --checker-sidecar PATH    required by enrichment if the default is unavailable
  --node-modules PATH       ai-pipe dependency tree required by enrichment
  --sqlite PATH             sqlite3 executable (default: sqlite3)
  --keep-workdir            retain the isolated corpus and databases
  --force                   overwrite an existing output JSON file
  --help                    show this text
`;

function parseArguments(argv) {
  const options = {
    binary: join(projectRoot, 'target/release/jscout'),
    revision: AI_PIPE_REVISION,
    suite: 'quick',
    sqlite: 'sqlite3',
    checkerSidecar: join(projectRoot, 'checker/src/main.mjs'),
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
      '--suite': 'suite',
      '--samples': 'samples',
      '--warmups': 'warmups',
      '--checker-sidecar': 'checkerSidecar',
      '--node-modules': 'nodeModules',
      '--sqlite': 'sqlite',
    }[argument];
    if (!key) throw new Error(`unknown argument: ${argument}\n\n${usage}`);
    const value = argv[++index];
    if (value === undefined) throw new Error(`missing value for ${argument}`);
    options[key] = value;
  }
  if (!options.repo || !options.output) throw new Error(`--repo and --output are required\n\n${usage}`);
  for (const key of ['samples', 'warmups']) {
    if (options[key] === undefined) continue;
    options[key] = Number(options[key]);
    const minimum = key === 'warmups' ? 0 : 1;
    if (!Number.isSafeInteger(options[key]) || options[key] < minimum) {
      throw new Error(`${key} must be an integer >= ${minimum}`);
    }
  }
  return options;
}

function selectedSuites(value) {
  const named = {
    quick: ['index', 'search', 'neighborhood', 'scout'],
    baseline: ['index', 'search', 'neighborhood', 'scout', 'embedding'],
    full: ['index', 'watch', 'enrichment', 'search', 'neighborhood', 'scout', 'embedding'],
  };
  const suites = named[value] ?? value.split(',').map((item) => item.trim()).filter(Boolean);
  const allowed = new Set(['index', 'watch', 'enrichment', 'search', 'neighborhood', 'scout', 'embedding']);
  for (const suite of suites) {
    if (!allowed.has(suite)) throw new Error(`unknown suite: ${suite}`);
  }
  if (!suites.includes('index')) suites.unshift('index');
  return [...new Set(suites)];
}

function countsForMode(options) {
  const smoke = options.suite === 'quick';
  const selected = (normal, quick = 1) => options.samples ?? (smoke ? quick : normal);
  const warmups = (normal) => options.warmups ?? (smoke ? 0 : normal);
  return {
    index: selected(5),
    watch: selected(11, 2),
    searchPasses: selected(5),
    neighborhood: selected(50, 2),
    neighborhoodWarmups: warmups(5),
    scout: selected(40, 2),
    scoutWarmups: warmups(5),
    searchWarmups: warmups(1),
    embeddingPopulation: selected(5),
    embeddingSynced: selected(15, 2),
    embeddingRepair: selected(5),
    enrichmentDryRun: selected(5),
    enrichmentReuse: selected(5),
  };
}

function sampledMetric(samples, timing, unit = 'ms') {
  return {
    unit,
    timing,
    samples,
    summary: distribution(samples),
  };
}

function measurement(id, parameters, metrics, counters = {}, validation = {}) {
  return {
    id,
    parameters,
    metrics,
    counters,
    validation,
  };
}

function assertEqual(actual, expected, label) {
  if (actual !== expected) throw new Error(`${label}: expected ${expected}, got ${actual}`);
}

function integrityCheck(context, database) {
  const result = commandOutput(context.sqlite, [database, 'PRAGMA integrity_check;'], { env: context.env });
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

function sqlString(value) {
  return `'${String(value).replaceAll("'", "''")}'`;
}

function sqliteJson(sqlite, database, sql, env) {
  const text = commandOutput(sqlite, ['-json', database, sql], { env });
  return text ? JSON.parse(text) : [];
}

function databaseCounters(sqlite, database, env) {
  const [row] = sqliteJson(sqlite, database, `
    SELECT
      (SELECT count(*) FROM files) AS files,
      (SELECT count(*) FROM chunks) AS chunks,
      (SELECT count(*) FROM symbols) AS symbols,
      (SELECT count(*) FROM refs) AS refs,
      (SELECT count(*) FROM member_calls) AS member_calls,
      (SELECT count(*) FROM graph_nodes) AS graph_nodes,
      (SELECT count(*) FROM resolved_edges) AS resolved_edges,
      (SELECT count(*) FROM resolved_edges
         WHERE provenance = 'receiver-value-flow') AS receiver_value_flow_edges,
      (SELECT count(DISTINCT source_ref_id) FROM resolved_edges
         WHERE provenance = 'receiver-value-flow') AS receiver_value_flow_occurrences;
  `, env);
  return row;
}

function validateCorpusCounts(actual) {
  const mappings = {
    indexed_files: 'files',
    chunks: 'chunks',
    symbols: 'symbols',
    references: 'refs',
    member_calls: 'member_calls',
    graph_nodes: 'graph_nodes',
    graph_edges: 'resolved_edges',
    receiver_value_flow_edges: 'receiver_value_flow_edges',
    receiver_value_flow_occurrences: 'receiver_value_flow_occurrences',
  };
  for (const [invariant, actualKey] of Object.entries(mappings)) {
    const expected = CORPUS_INVARIANTS[invariant];
    if (actual[actualKey] !== expected) {
      throw new Error(`corpus invariant ${invariant}: expected ${expected}, got ${actual[actualKey]}`);
    }
  }
}

function indexDatabase(context, database) {
  return run(context.binary, [
    '--config', context.config,
    'index', context.corpus,
    '--database', database,
    '--no-deps',
  ], { env: context.env, timeoutMs: 5 * 60_000 });
}

function parseIndexReport(stdout) {
  const match = stdout.match(
    /indexed (\d+) files \(removed=(\d+), rejected=(\d+)\) — (\d+) chunks, (\d+) refs/,
  );
  if (!match) throw new Error(`missing indexing report: ${stdout}`);
  return {
    indexed: Number(match[1]),
    removed: Number(match[2]),
    rejected: Number(match[3]),
    chunks: Number(match[4]),
    refs: Number(match[5]),
  };
}

function validateIndexReport(report) {
  assertEqual(report.indexed, CORPUS_INVARIANTS.indexed_files, 'indexed files');
  assertEqual(report.removed, 0, 'removed files');
  assertEqual(report.rejected, CORPUS_INVARIANTS.rejected_files, 'rejected files');
  assertEqual(report.chunks, CORPUS_INVARIANTS.chunks, 'reported chunks');
  assertEqual(report.refs, CORPUS_INVARIANTS.references, 'reported references');
}

function runIndexSuite(context) {
  console.error('benchmark: fresh-database indexing');
  const directory = join(context.workspace, 'index');
  mkdirSync(directory, { recursive: true });
  const firstDatabase = join(directory, 'first.db');
  const first = indexDatabase(context, firstDatabase);
  validateIndexReport(parseIndexReport(first.stdout));
  const firstCounts = databaseCounters(context.sqlite, firstDatabase, context.env);
  validateCorpusCounts(firstCounts);
  const firstIntegrity = integrityCheck(context, firstDatabase);

  const samples = [];
  const databases = [firstDatabase];
  let seedDatabase = firstDatabase;
  for (let index = 0; index < context.counts.index; index += 1) {
    assertActive(context);
    const database = join(directory, `fresh-${index + 1}.db`);
    const result = indexDatabase(context, database);
    validateIndexReport(parseIndexReport(result.stdout));
    samples.push(result.elapsedMs);
    databases.push(database);
    seedDatabase = database;
    validateCorpusCounts(databaseCounters(context.sqlite, database, context.env));
  }
  const counters = databaseCounters(context.sqlite, seedDatabase, context.env);
  const databaseBytes = fileMetadata(seedDatabase).bytes;
  context.seedDatabase = seedDatabase;
  const measurements = [
    measurement(
      'index.first_post_archive',
      { database: 'new', filesystem_cache: 'uncontrolled' },
      { wall_ms: sampledMetric([first.elapsedMs], 'process wall, including startup') },
      firstCounts,
      { integrity_check: firstIntegrity, index_report_matches: true },
    ),
    measurement(
      'index.fresh_database_warm_filesystem',
      { database: 'new per sample', filesystem_cache: 'warm/uncontrolled' },
      { wall_ms: sampledMetric(samples, 'process wall, including startup') },
      { ...counters, database_bytes: databaseBytes },
      { corpus_counts_match: true, index_reports_match: true },
    ),
  ];
  for (const database of databases) {
    if (database !== seedDatabase) removeDatabaseFamily(context, database);
  }
  return measurements;
}

async function runWatchSuite(context) {
  console.error('benchmark: unchanged watch reconciliation');
  const database = join(context.workspace, 'watch.db');
  backupDatabase(context.sqlite, context.seedDatabase, database, context.env);
  const target = join(context.corpus, WATCH_TOUCH_PATH);
  if (!existsSync(target)) throw new Error(`watch fixture missing: ${target}`);
  const args = [
    '--config', context.config,
    'watch', context.corpus,
    '--database', database,
    '--no-deps', '--no-embed', '--no-product', '--no-enrich',
    '--debounce-ms', '100', '--reconcile-seconds', '0',
  ];
  const child = spawn(context.binary, args, { env: context.env, stdio: ['ignore', 'ignore', 'pipe'] });
  context.children.add(child);
  const lines = createInterface({ input: child.stderr, crlfDelay: Infinity });
  const successes = [];
  let stderr = '';
  let waiter = null;
  const timeoutFor = (label, predicate, timeoutMs = 30_000) => new Promise((resolveWait, reject) => {
    const timer = setTimeout(() => {
      waiter = null;
      reject(new Error(`watch timed out waiting for ${label}\n${stderr}`));
    }, timeoutMs);
    waiter = {
      predicate,
      reject: (error) => {
        clearTimeout(timer);
        waiter = null;
        reject(error);
      },
      resolve: (value) => {
        clearTimeout(timer);
        waiter = null;
        resolveWait(value);
      },
    };
  });
  lines.on('line', (line) => {
    stderr += `${line}\n`;
    const match = line.match(/watch generation=(\d+) phase=refresh .*status=succeeded .*indexed=(\d+) unchanged=(\d+).*projection=([^ ]+) elapsed_ms=(\d+)/);
    if (!match) return;
    const row = {
      generation: Number(match[1]),
      indexed: Number(match[2]),
      unchanged: Number(match[3]),
      projection: match[4],
      elapsedMs: Number(match[5]),
    };
    successes.push(row);
    if (waiter?.predicate({ type: 'success', row })) waiter.resolve(row);
    return;
  });
  lines.on('line', (line) => {
    const match = line.match(/watch generation=(\d+) status=clean/);
    const generation = Number(match?.[1]);
    const row = successes.find((item) => item.generation === generation);
    if (match && waiter?.predicate({ type: 'clean', generation, row })) {
      waiter.resolve({ generation, row });
    }
  });
  child.once('error', (error) => waiter?.reject(error));
  child.once('close', (code, signal) => {
    if (waiter) waiter.reject(new Error(`watch exited code=${code} signal=${signal}\n${stderr}`));
  });
  try {
    const initial = await timeoutFor(
      'initial generation',
      (event) => event.type === 'clean' && event.generation === 1,
      120_000,
    );
    let lastGeneration = initial.generation;
    await new Promise((resolveDelay) => setTimeout(resolveDelay, 250));
    const samples = [];
    for (let index = 0; index < context.counts.watch; index += 1) {
      assertActive(context);
      const pending = timeoutFor(
        `unchanged generation ${index + 1}`,
        (event) => event.type === 'clean' && event.generation > lastGeneration,
      );
      const timestamp = new Date(Date.now() + index + 1);
      utimesSync(target, timestamp, timestamp);
      const event = await pending;
      const row = event.row;
      lastGeneration = event.generation;
      if (row.indexed !== 0 || row.unchanged !== CORPUS_INVARIANTS.indexed_files || row.projection !== 'reused') {
        throw new Error(`unexpected watch result: ${JSON.stringify(row)}`);
      }
      samples.push(row.elapsedMs);
    }
    return [measurement(
      'watch.unchanged_generation',
      { event: 'same-content mtime change', debounce_ms: 100, debounce_included: false },
      { internal_refresh_ms: sampledMetric(samples, 'JScout internal refresh telemetry') },
      { files_unchanged: CORPUS_INVARIANTS.indexed_files, files_indexed: 0 },
      { projection: 'reused', corpus_content_unchanged: true },
    )];
  } finally {
    await stopChild(child);
    context.children.delete(child);
    removeDatabaseFamily(context, database);
  }
}

function searchArguments(context, query) {
  return [
    '--config', context.config,
    'search', context.corpus, query,
    '--database', context.seedDatabase,
    '--lexical-only', '--no-memory', '--no-expand',
    '-k', String(SEARCH_LIMIT), '--json',
  ];
}

async function runSearchSuite(context) {
  console.error('benchmark: lexical CLI and persistent MCP search');
  const cliSamples = [];
  const bm25Samples = [];
  const cliBytes = [];
  const cliDigests = new Map();
  const runCliPass = (measured) => {
    for (const query of SEARCH_QUERIES) {
      assertActive(context);
      const result = run(context.binary, searchArguments(context, query), { env: context.env });
      const value = JSON.parse(result.stdout);
      assertEqual(value.hits?.length, SEARCH_LIMIT, `CLI search hits for ${query}`);
      const bm25 = result.stderr.match(/timing: bm25 ([0-9.]+)ms/);
      if (!bm25) throw new Error(`CLI search omitted BM25 timing for ${query}`);
      const digest = sha256Bytes(result.stdout);
      if (cliDigests.has(query) && cliDigests.get(query) !== digest) {
        throw new Error(`CLI search output changed across passes: ${query}`);
      }
      cliDigests.set(query, digest);
      if (measured) {
        cliSamples.push(result.elapsedMs);
        bm25Samples.push(Number(bm25[1]));
        cliBytes.push(Buffer.byteLength(result.stdout));
      }
    }
  };
  for (let pass = 0; pass < context.counts.searchWarmups; pass += 1) runCliPass(false);
  for (let pass = 0; pass < context.counts.searchPasses; pass += 1) runCliPass(true);
  assertEqual(bm25Samples.length, SEARCH_QUERIES.length * context.counts.searchPasses, 'BM25 samples');

  const mcp = new McpClient({
    binary: context.binary,
    args: [
      '--config', context.config,
      'mcp', context.corpus,
      '--database', context.seedDatabase,
      '--profile', 'structural',
      '--result-transport', 'text',
    ],
    env: context.env,
  });
  context.children.add(mcp.child);
  const mcpSamples = [];
  const mcpBytes = [];
  const mcpDigests = new Map();
  const mcpArguments = (query) => ({
    query,
    limit: SEARCH_LIMIT,
    vector: false,
    rerank: false,
    include_memory: false,
    expand: false,
    response_bytes: 24_000,
  });
  const runMcpPass = async (measured) => {
    for (const query of SEARCH_QUERIES) {
      assertActive(context);
      const { response, elapsedMs } = await mcp.request('tools/call', {
        name: 'semantic_search',
        arguments: mcpArguments(query),
      });
      const { text, value } = parseTextToolResponse(response, `search: ${query}`);
      assertEqual(value.hits?.length, SEARCH_LIMIT, `MCP search hits for ${query}`);
      const digest = sha256Bytes(text);
      if (mcpDigests.has(query) && mcpDigests.get(query) !== digest) {
        throw new Error(`MCP search output changed across passes: ${query}`);
      }
      mcpDigests.set(query, digest);
      if (measured) {
        mcpSamples.push(elapsedMs);
        mcpBytes.push(Buffer.byteLength(text));
      }
    }
  };
  try {
    await mcp.initialize('jscout-search-benchmark');
    for (let pass = 0; pass < context.counts.searchWarmups; pass += 1) await runMcpPass(false);
    for (let pass = 0; pass < context.counts.searchPasses; pass += 1) await runMcpPass(true);
  } finally {
    try {
      await mcp.close();
    } finally {
      context.children.delete(mcp.child);
    }
  }

  return [
    measurement(
      'search.cli.lexical',
      {
        queries: SEARCH_QUERIES.length,
        measured_passes: context.counts.searchPasses,
        warmup_passes: context.counts.searchWarmups,
        limit: SEARCH_LIMIT,
      },
      {
        wall_ms: sampledMetric(cliSamples, 'process wall, including startup'),
        bm25_ms: sampledMetric(bm25Samples, 'JScout BM25 telemetry'),
        result_bytes: sampledMetric(cliBytes, 'serialized JSON stdout', 'bytes'),
      },
      { hits_per_response: SEARCH_LIMIT },
      { stable_per_query_output: true, complete_bm25_telemetry: true },
    ),
    measurement(
      'search.mcp.lexical',
      {
        queries: SEARCH_QUERIES.length,
        measured_passes: context.counts.searchPasses,
        warmup_passes: context.counts.searchWarmups,
        limit: SEARCH_LIMIT,
      },
      {
        roundtrip_ms: sampledMetric(mcpSamples, 'persistent MCP round trip'),
        result_bytes: sampledMetric(mcpBytes, 'serialized MCP text payload', 'bytes'),
      },
      { hits_per_response: SEARCH_LIMIT },
      { stable_per_query_output: true },
    ),
  ];
}

function neighborhoodOutput(response, label) {
  const { text, value } = parseTextToolResponse(response, label);
  const size = (item) => Array.isArray(item) ? item.length : Object.keys(item ?? {}).length;
  return {
    text,
    digest: sha256Bytes(text),
    bytes: Buffer.byteLength(text),
    nodes: size(value.graph?.nodes),
    edges: size(value.graph?.edges),
    omittedNodes: value.response?.omitted?.nodes ?? 0,
    omittedEdges: value.response?.omitted?.edges ?? 0,
    truncated: Boolean(value.response?.truncated),
  };
}

function validateNeighborhoodAnchors(context) {
  for (const [anchorClass, anchor] of Object.entries(NEIGHBORHOOD_ANCHORS)) {
    const [row] = sqliteJson(context.sqlite, context.seedDatabase, `
      SELECT count(*) AS degree
      FROM resolved_edges
      WHERE src_key = ${sqlString(anchor.id)} OR dst_key = ${sqlString(anchor.id)};
    `, context.env);
    assertEqual(row?.degree, anchor.degree, `${anchorClass} neighborhood anchor degree`);
  }
}

async function runNeighborhoodSuite(context) {
  console.error('benchmark: neighborhood traversal and response budgeting');
  validateNeighborhoodAnchors(context);
  const mcp = new McpClient({
    binary: context.binary,
    args: [
      '--config', context.config,
      'mcp', context.corpus,
      '--database', context.seedDatabase,
      '--profile', 'structural', '--result-transport', 'text',
    ],
    env: context.env,
  });
  context.children.add(mcp.child);
  const rows = new Map(NEIGHBORHOOD_CASES.map((testCase) => [testCase.id, []]));
  try {
    await mcp.initialize('jscout-neighborhood-benchmark');
    for (let pass = 0; pass < context.counts.neighborhoodWarmups + context.counts.neighborhood; pass += 1) {
      for (let offset = 0; offset < NEIGHBORHOOD_CASES.length; offset += 1) {
        assertActive(context);
        const testCase = NEIGHBORHOOD_CASES[(pass + offset) % NEIGHBORHOOD_CASES.length];
        const { response, elapsedMs } = await mcp.request('tools/call', {
          name: 'neighborhood',
          arguments: {
            anchor: testCase.anchor,
            depth: testCase.depth,
            direction: testCase.direction,
            node_limit: testCase.node_limit,
            edge_limit: testCase.edge_limit,
            min_confidence: testCase.min_confidence,
            origins: testCase.origins,
            response_bytes: testCase.response_bytes,
            debug: testCase.debug,
          },
        });
        const output = neighborhoodOutput(response, testCase.id);
        assertEqual(output.nodes, testCase.expected.nodes, `${testCase.id} node count`);
        assertEqual(output.edges, testCase.expected.edges, `${testCase.id} edge count`);
        assertEqual(output.omittedNodes, testCase.expected.omitted_nodes, `${testCase.id} omitted node count`);
        assertEqual(output.omittedEdges, testCase.expected.omitted_edges, `${testCase.id} omitted edge count`);
        assertEqual(output.truncated, testCase.expected.truncated, `${testCase.id} truncation state`);
        if (output.bytes > testCase.response_bytes) {
          throw new Error(`${testCase.id} exceeded its ${testCase.response_bytes}-byte response budget`);
        }
        if (pass >= context.counts.neighborhoodWarmups) {
          rows.get(testCase.id).push({ elapsedMs, ...output });
        }
      }
    }
  } finally {
    try {
      await mcp.close();
    } finally {
      context.children.delete(mcp.child);
    }
  }
  return NEIGHBORHOOD_CASES.map((testCase) => {
    const samples = rows.get(testCase.id);
    const digests = new Set(samples.map((row) => row.digest));
    const expectedDegree = NEIGHBORHOOD_ANCHORS[testCase.anchor_class].degree;
    return measurement(
      `neighborhood.${testCase.id}`,
      { ...testCase, expected_anchor_degree: expectedDegree },
      {
        roundtrip_ms: sampledMetric(
          samples.map((row) => row.elapsedMs),
          'persistent MCP round trip',
        ),
        result_bytes: sampledMetric(
          samples.map((row) => row.bytes),
          'serialized MCP text payload',
          'bytes',
        ),
      },
      {
        nodes: { samples: samples.map((row) => row.nodes), summary: distribution(samples.map((row) => row.nodes)) },
        edges: { samples: samples.map((row) => row.edges), summary: distribution(samples.map((row) => row.edges)) },
        omitted_nodes: {
          samples: samples.map((row) => row.omittedNodes),
          summary: distribution(samples.map((row) => row.omittedNodes)),
        },
        omitted_edges: {
          samples: samples.map((row) => row.omittedEdges),
          summary: distribution(samples.map((row) => row.omittedEdges)),
        },
      },
      {
        anchor_degree_matches_fixture: true,
        workload_cardinality_matches_fixture: true,
        within_response_budget: true,
        unique_response_hashes: digests.size,
        deterministic: digests.size === 1,
        truncated_values: [...new Set(samples.map((row) => row.truncated))],
      },
    );
  });
}

function runScoutSuite(context) {
  console.error('benchmark: provider-free scouting preparation');
  const missingGateway = join(context.workspace, 'gateway-must-not-run.mjs');
  const cases = [
    { id: 'workflows_auto', command: 'workflows', extra: [] },
    { id: 'cards_auto', command: 'cards', extra: [] },
    { id: 'workflow_explicit', command: 'workflows', extra: ['--seed', SCOUT_WORKFLOW_SEED, '--depth', '1'] },
    { id: 'card_explicit', command: 'cards', extra: ['--anchor', SCOUT_CARD_ANCHOR] },
  ];
  const results = [];
  for (const entry of cases) {
    const args = [
      '--config', context.config,
      'scout', entry.command, context.corpus,
      '--database', context.seedDatabase,
      '--gateway-path', missingGateway,
      '--max-calls', '512', '--dry-run',
      ...entry.extra,
    ];
    const runOnce = () => {
      assertActive(context);
      const result = run(context.binary, args, {
        env: context.env,
        timeoutMs: 5 * 60_000,
        maxBuffer: 128 * 1024 * 1024,
      });
      const parsed = JSON.parse(result.stdout);
      if (parsed.dry_run !== true) throw new Error(`${entry.id} did not report dry_run=true`);
      if (!Array.isArray(parsed.plan?.items)) throw new Error(`${entry.id} omitted plan items`);
      const counters = {
        calls_planned: parsed.calls_planned,
        plan_items: parsed.plan.items.length,
        skipped_items: parsed.plan.skipped?.length ?? 0,
        over_context_bytes_items: parsed.over_context_bytes_items,
        total_evidence_bytes: parsed.plan.items.reduce(
          (total, item) => total + item.evidence_bytes,
          0,
        ),
        total_request_bytes: parsed.plan.items.reduce((total, item) => total + item.request_bytes, 0),
      };
      for (const [name, value] of Object.entries(counters)) {
        if (!Number.isSafeInteger(value) || value < 0) {
          throw new Error(`${entry.id} emitted invalid ${name}: ${value}`);
        }
      }
      for (const [name, value] of Object.entries(SCOUT_PLAN_INVARIANTS[entry.id])) {
        assertEqual(counters[name], value, `${entry.id} ${name}`);
      }
      return { result, parsed, counters, digest: sha256Bytes(result.stdout) };
    };
    let expected;
    const verifyStable = (current) => {
      const stable = { digest: current.digest, counters: current.counters };
      if (expected && JSON.stringify(stable) !== JSON.stringify(expected)) {
        throw new Error(`${entry.id} scouting plan changed across runs`);
      }
      expected = stable;
    };
    for (let index = 0; index < context.counts.scoutWarmups; index += 1) verifyStable(runOnce());
    const samples = [];
    const outputBytes = [];
    let output;
    for (let index = 0; index < context.counts.scout; index += 1) {
      const current = runOnce();
      verifyStable(current);
      samples.push(current.result.elapsedMs);
      outputBytes.push(Buffer.byteLength(current.result.stdout));
      output = current;
    }
    results.push(measurement(
      `scout.prepare.${entry.id}`,
      { dry_run: true, max_calls: 512 },
      {
        wall_ms: sampledMetric(samples, 'process wall, including startup'),
        result_bytes: sampledMetric(outputBytes, 'serialized scouting plan', 'bytes'),
      },
      output.counters,
      {
        no_gateway_started: !existsSync(missingGateway),
        dry_run: output.parsed.dry_run,
        workload_cardinality_matches_fixture: true,
        stable_plan_hash: output.digest,
      },
    ));
  }

  const publishDatabase = join(context.workspace, 'scout-publish.db');
  backupDatabase(context.sqlite, context.seedDatabase, publishDatabase, context.env);
  const publishArgs = [
    '--config', context.config,
    'scout', 'cards', context.corpus,
    '--database', publishDatabase,
    '--gateway-path', mockGateway,
    '--anchor', SCOUT_CARD_ANCHOR,
    '--max-calls', '1', '--reasoning', 'low',
  ];
  const calls = (text) => Number(text.match(/model calls: (\d+)/)?.[1]);
  const scoutDatabaseState = () => sqliteJson(context.sqlite, publishDatabase, `
    SELECT
      (SELECT count(*) FROM semantic_artifacts WHERE artifact_type = 'card') AS card_artifacts,
      (SELECT count(*) FROM semantic_supports AS supports
         JOIN semantic_artifacts AS artifacts ON artifacts.id = supports.artifact_id
        WHERE artifacts.artifact_type = 'card') AS card_supports,
      (SELECT count(*) FROM scout_runs WHERE scout_kind = 'card' AND status = 'completed') AS completed_runs;
  `, context.env)[0];
  const initialDatabaseState = scoutDatabaseState();
  assertEqual(initialDatabaseState.card_artifacts, 0, 'initial card artifacts');
  assertEqual(initialDatabaseState.card_supports, 0, 'initial card supports');
  assertEqual(initialDatabaseState.completed_runs, 0, 'initial completed card scout runs');
  const publish = run(context.binary, publishArgs, { env: context.env, timeoutMs: 60_000 });
  if (calls(publish.stdout) !== 1) throw new Error(`unexpected scout publication report\n${publish.stdout}`);
  const databaseState = scoutDatabaseState();
  assertEqual(databaseState.card_artifacts, 1, 'published card artifacts');
  assertEqual(databaseState.completed_runs, 1, 'completed card scout runs');
  if (databaseState.card_supports < 1) throw new Error('published card has no persisted support');
  const scoutIntegrity = integrityCheck(context, publishDatabase);
  const reuse = run(context.binary, publishArgs, { env: context.env, timeoutMs: 60_000 });
  if (calls(reuse.stdout) !== 0) throw new Error(`unexpected scout reuse report\n${reuse.stdout}`);
  const reuseDatabaseState = scoutDatabaseState();
  if (JSON.stringify(reuseDatabaseState) !== JSON.stringify(databaseState)) {
    throw new Error('scout reuse changed persisted card state');
  }
  const reuseIntegrity = integrityCheck(context, publishDatabase);
  results.push(measurement(
    'scout.card_publication_smoke',
    { fake_gateway: true },
    { wall_ms: sampledMetric([publish.elapsedMs], 'process wall, including startup') },
    { model_calls: 1, ...databaseState },
    { published: true, integrity_check: scoutIntegrity },
  ));
  results.push(measurement(
    'scout.card_reuse_smoke',
    { fake_gateway: true },
    { wall_ms: sampledMetric([reuse.elapsedMs], 'process wall, including startup') },
    { model_calls: 0, ...reuseDatabaseState },
    { reused: true, database_state_unchanged: true, integrity_check: reuseIntegrity },
  ));
  removeDatabaseFamily(context, publishDatabase);
  return results;
}

async function startMockInference(context) {
  const child = spawn(process.execPath, [
    mockInference,
    '--port', '0',
    '--dimensions', String(EMBEDDING_FIXTURE.dimensions),
  ], {
    env: context.env,
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  context.children.add(child);
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  const ready = await new Promise((resolveReady, reject) => {
    const timer = setTimeout(() => reject(new Error(`mock inference startup timed out: ${stderr}`)), 10_000);
    lines.once('line', (line) => {
      clearTimeout(timer);
      try {
        resolveReady(JSON.parse(line));
      } catch (error) {
        reject(new Error(`invalid mock inference ready record: ${error.message}`));
      }
    });
    child.once('error', reject);
    child.once('exit', (code) => reject(new Error(`mock inference exited ${code}: ${stderr}`)));
  });
  if (!ready.url?.startsWith('http://127.0.0.1:')) {
    await stopChild(child);
    throw new Error(`mock inference did not bind loopback: ${JSON.stringify(ready)}`);
  }
  assertEqual(ready.model, EMBEDDING_FIXTURE.model, 'mock embedding model');
  assertEqual(ready.revision, EMBEDDING_FIXTURE.revision, 'mock embedding revision');
  assertEqual(ready.dimensions, EMBEDDING_FIXTURE.dimensions, 'mock embedding dimensions');
  return {
    url: ready.url,
    model: ready.model,
    revision: ready.revision,
    dimensions: ready.dimensions,
    async stop() {
      await stopChild(child);
      context.children.delete(child);
    },
  };
}

async function fetchJson(url, options = {}) {
  const response = await fetch(url, { ...options, signal: AbortSignal.timeout(5_000) });
  if (!response.ok) throw new Error(`${options.method ?? 'GET'} ${url}: HTTP ${response.status}`);
  return response.json();
}

function parseEmbeddingReport(stdout) {
  const match = stdout.match(/code embeddings: missing=(\d+) embedded=(\d+) cached_reused=(\d+) occurrences_synced=(\d+)/);
  if (!match) throw new Error(`missing embedding report: ${stdout}`);
  return {
    missing: Number(match[1]),
    embedded: Number(match[2]),
    cached_reused: Number(match[3]),
    occurrences_synced: Number(match[4]),
  };
}

async function runEmbeddingSuite(context) {
  console.error('benchmark: deterministic loopback embedding synchronization');
  const server = await startMockInference(context);
  const repairArguments = run(context.binary, ['embed', '--help'], { env: context.env })
    .stdout.includes('--repair') ? ['--repair'] : [];
  const config = join(context.workspace, 'embedding.toml');
  writeFileSync(config, `version = 1

[embedding]
provider = "local"
model = "${server.model}"
revision = "${server.revision}"
batch = ${EMBEDDING_FIXTURE.batch}
origins = ["repository", "workspace"]

[inference]
url = "${server.url}"

[diagnostics]
timing = true
debug = false
`);
  const runEmbed = async (database, extraArguments = []) => {
    assertActive(context);
    await fetchJson(`${server.url}/reset`, { method: 'POST' });
    const result = run(context.binary, [
      '--config', config, 'embed', context.corpus, '--database', database,
      ...extraArguments,
    ], { env: context.env, timeoutMs: 10 * 60_000 });
    const state = sqliteJson(context.sqlite, database, `
      SELECT
        (SELECT provider FROM embedding_profiles LIMIT 1) AS provider,
        (SELECT model FROM embedding_profiles LIMIT 1) AS model,
        (SELECT dimensions FROM embedding_profiles LIMIT 1) AS dimensions,
        (SELECT count(*) FROM embeddings) AS embeddings,
        (SELECT count(*) FROM embedding_index_entries) AS occurrences,
        (SELECT count(*) FROM meta WHERE key LIKE 'embedding_index_synced_v1:%') AS sync_markers;
    `, context.env)[0];
    return {
      result,
      report: parseEmbeddingReport(result.stdout),
      provider: await fetchJson(`${server.url}/stats`),
      state,
      integrity: integrityCheck(context, database),
    };
  };
  try {
    const population = [];
    let populatedDatabase;
    for (let index = 0; index < context.counts.embeddingPopulation; index += 1) {
      const database = join(context.workspace, `embedding-population-${index + 1}.db`);
      backupDatabase(context.sqlite, context.seedDatabase, database, context.env);
      const row = await runEmbed(database);
      population.push(row);
      if (populatedDatabase) removeDatabaseFamily(context, populatedDatabase);
      populatedDatabase = database;
    }
    const canonical = join(context.workspace, 'embedding-populated.db');
    backupDatabase(context.sqlite, populatedDatabase, canonical, context.env);
    removeDatabaseFamily(context, populatedDatabase);

    const synced = [];
    for (let index = 0; index < context.counts.embeddingSynced; index += 1) {
      const database = join(context.workspace, `embedding-synced-${index + 1}.db`);
      backupDatabase(context.sqlite, canonical, database, context.env);
      synced.push(await runEmbed(database));
      removeDatabaseFamily(context, database);
    }

    const repair = [];
    for (let index = 0; index < context.counts.embeddingRepair; index += 1) {
      const database = join(context.workspace, `embedding-repair-${index + 1}.db`);
      backupDatabase(context.sqlite, canonical, database, context.env);
      run(context.sqlite, [database, 'DELETE FROM embedding_index_entries; PRAGMA wal_checkpoint(TRUNCATE);'], { env: context.env });
      repair.push(await runEmbed(database, repairArguments));
      removeDatabaseFamily(context, database);
    }

    const [counts] = sqliteJson(context.sqlite, canonical, `
      SELECT
        (SELECT dimensions FROM embedding_profiles LIMIT 1) AS dimensions,
        (SELECT count(*) FROM embeddings) AS embeddings,
        (SELECT count(*) FROM embedding_index_entries) AS occurrences,
        (SELECT count(*) FROM meta WHERE key LIKE 'embedding_index_synced_v1:%') AS sync_markers;
    `, context.env);
    const databaseBytes = fileMetadata(canonical).bytes;
    const makeEmbeddingMeasurement = (id, rows, expected, expectedProviderRequests) => {
      for (const row of rows) {
        for (const [key, value] of Object.entries(expected)) {
          if (row.report[key] !== value) throw new Error(`${id} ${key}: ${row.report[key]} != ${value}`);
        }
        assertEqual(row.provider.configurationRequests, EMBEDDING_FIXTURE.configuration_requests, `${id} configuration requests`);
        assertEqual(row.provider.embedRequests, expectedProviderRequests, `${id} embedding requests`);
        assertEqual(row.provider.texts, expectedProviderRequests === 0 ? 0 : EMBEDDING_FIXTURE.unique_embeddings, `${id} provider texts`);
        assertEqual(row.provider.dimensions, EMBEDDING_FIXTURE.dimensions, `${id} provider dimensions`);
        assertEqual(row.state.provider, 'local', `${id} database provider`);
        assertEqual(row.state.model, EMBEDDING_FIXTURE.model, `${id} database model`);
        assertEqual(row.state.dimensions, EMBEDDING_FIXTURE.dimensions, `${id} database dimensions`);
        assertEqual(row.state.embeddings, EMBEDDING_FIXTURE.unique_embeddings, `${id} stored embeddings`);
        assertEqual(row.state.occurrences, EMBEDDING_FIXTURE.occurrence_entries, `${id} occurrence entries`);
        assertEqual(row.state.sync_markers, EMBEDDING_FIXTURE.sync_markers, `${id} sync markers`);
        assertEqual(row.integrity, 'ok', `${id} database integrity`);
      }
      return measurement(
        id,
        {
          dimensions: server.dimensions,
          model: server.model,
          revision: server.revision,
          provider: 'deterministic loopback fixture',
        },
        {
          wall_ms: sampledMetric(rows.map((row) => row.result.elapsedMs), 'process wall, including startup'),
          provider_handler_ms: sampledMetric(
            rows.map((row) => row.provider.handlerNs / 1e6),
            'loopback provider request-handler telemetry',
          ),
        },
        {
          report: rows.at(-1).report,
          provider_embed_requests: rows.map((row) => row.provider.embedRequests),
          provider_texts: rows.map((row) => row.provider.texts),
          provider_input_chars: rows.map((row) => row.provider.inputChars),
          provider_request_bytes: rows.map((row) => row.provider.requestBytes),
        },
        { reports_match: true, provider_counts_match: true, database_state_matches: true },
      );
    };
    const measurements = [
      makeEmbeddingMeasurement('embedding.populate', population, {
        missing: EMBEDDING_FIXTURE.unique_embeddings,
        embedded: EMBEDDING_FIXTURE.unique_embeddings,
        cached_reused: 0,
        occurrences_synced: EMBEDDING_FIXTURE.occurrence_entries,
      }, EMBEDDING_FIXTURE.embed_requests),
      makeEmbeddingMeasurement('embedding.synced', synced, {
        missing: 0,
        embedded: 0,
        cached_reused: EMBEDDING_FIXTURE.unique_embeddings,
        occurrences_synced: EMBEDDING_FIXTURE.occurrence_entries,
      }, 0),
      makeEmbeddingMeasurement('embedding.repair_occurrence_index', repair, {
        missing: 0,
        embedded: 0,
        cached_reused: EMBEDDING_FIXTURE.unique_embeddings,
        occurrences_synced: EMBEDDING_FIXTURE.occurrence_entries,
      }, 0),
      {
        id: 'embedding.database_validation',
        parameters: {
          dimensions: EMBEDDING_FIXTURE.dimensions,
          model: EMBEDDING_FIXTURE.model,
          revision: EMBEDDING_FIXTURE.revision,
        },
        metrics: {},
        counters: { ...counts, database_bytes: databaseBytes },
        validation: {
          integrity_check: integrityCheck(context, canonical),
        },
      },
    ];
    removeDatabaseFamily(context, canonical);
    return measurements;
  } finally {
    await server.stop();
  }
}

function runEnrichmentSuite(context) {
  console.error('benchmark: checker enrichment and unchanged reuse');
  if (!context.nodeModules) throw new Error('--node-modules is required by the enrichment suite');
  if (!existsSync(context.checkerSidecar)) throw new Error(`checker sidecar not found: ${context.checkerSidecar}`);
  if (!existsSync(context.nodeModules)) throw new Error(`node_modules not found: ${context.nodeModules}`);
  const corpusModules = join(context.corpus, 'node_modules');
  if (existsSync(corpusModules)) throw new Error('archived corpus unexpectedly contains node_modules');
  symlinkSync(context.nodeModules, corpusModules, 'dir');

  const validateReport = (report, expected, label) => {
    for (const [key, value] of Object.entries(expected)) assertEqual(report[key], value, `${label} ${key}`);
    assertEqual(report.projects, ENRICHMENT_INVARIANTS.projects, `${label} projects`);
  };

  const dryRunSamples = [];
  let dryRunReport;
  for (let index = 0; index < context.counts.enrichmentDryRun; index += 1) {
    assertActive(context);
    const database = join(context.workspace, `enrichment-plan-${index + 1}.db`);
    backupDatabase(context.sqlite, context.seedDatabase, database, context.env);
    const result = run(context.binary, [
      '--config', context.config,
      'enrich', context.corpus,
      '--database', database,
      '--sidecar-path', context.checkerSidecar,
      '--timeout', '300', '--all', '--dry-run',
    ], { env: context.env, timeoutMs: 10 * 60_000 });
    dryRunReport = JSON.parse(result.stdout);
    validateReport(dryRunReport, {
      occurrences_queried: 0,
      occurrences_selected: ENRICHMENT_INVARIANTS.occurrences_selected,
      occurrences_omitted: 0,
      occurrences_resumed: 0,
      request_batches: 0,
      unknown_answers: 0,
      facts_published: 0,
      dry_run: true,
    }, 'enrichment plan');
    dryRunSamples.push(result.elapsedMs);
    removeDatabaseFamily(context, database);
  }

  const fullDatabase = join(context.workspace, 'enrichment-full.db');
  backupDatabase(context.sqlite, context.seedDatabase, fullDatabase, context.env);
  const full = run(context.binary, [
    '--config', context.config,
    'enrich', context.corpus,
    '--database', fullDatabase,
    '--sidecar-path', context.checkerSidecar,
    '--timeout', '300', '--all', '--full',
  ], { env: context.env, timeoutMs: 30 * 60_000 });
  const fullReport = JSON.parse(full.stdout);
  validateReport(fullReport, {
    occurrences_queried: ENRICHMENT_INVARIANTS.occurrences_selected,
    occurrences_selected: ENRICHMENT_INVARIANTS.occurrences_selected,
    occurrences_omitted: 0,
    occurrences_resumed: 0,
    request_batches: ENRICHMENT_INVARIANTS.request_batches,
    unknown_answers: ENRICHMENT_INVARIANTS.unknown_answers,
    facts_published: ENRICHMENT_INVARIANTS.facts_published,
    dry_run: false,
  }, 'full enrichment');
  const [fullProjection] = sqliteJson(context.sqlite, fullDatabase, `
    SELECT
      sum(provenance = 'receiver-value-flow') AS value_flow_facts,
      sum(provenance = 'checker') AS checker_facts,
      sum(provenance IN ('receiver-value-flow', 'checker')) AS combined_facts
    FROM resolved_edges;
  `, context.env);
  assertEqual(
    fullProjection.value_flow_facts,
    CORPUS_INVARIANTS.receiver_value_flow_edges,
    'projected value-flow facts',
  );
  assertEqual(fullProjection.checker_facts, fullReport.facts_published, 'projected checker facts');
  assertEqual(
    fullProjection.combined_facts,
    ENRICHMENT_INVARIANTS.combined_projected_facts,
    'combined occurrence-specific facts',
  );
  const [fullProjectRuns] = sqliteJson(context.sqlite, fullDatabase, `
    SELECT
      count(*) AS projects,
      sum(status = 'completed') AS completed_projects,
      sum(completed_occurrences) AS completed_occurrences
    FROM checker_project_runs
    WHERE batch_id = ${Number(fullReport.batch_id)};
  `, context.env);
  assertEqual(fullProjectRuns.projects, ENRICHMENT_INVARIANTS.projects, 'full enrichment project runs');
  assertEqual(fullProjectRuns.completed_projects, ENRICHMENT_INVARIANTS.projects, 'completed enrichment project runs');
  assertEqual(
    fullProjectRuns.completed_occurrences,
    ENRICHMENT_INVARIANTS.occurrences_selected,
    'completed enrichment occurrences',
  );
  const fullIntegrity = integrityCheck(context, fullDatabase);
  const fullSeed = join(context.workspace, 'enrichment-complete.db');
  backupDatabase(context.sqlite, fullDatabase, fullSeed, context.env);
  removeDatabaseFamily(context, fullDatabase);

  const reuseSamples = [];
  let reuseReport;
  for (let index = 0; index < context.counts.enrichmentReuse; index += 1) {
    assertActive(context);
    const database = join(context.workspace, `enrichment-reuse-${index + 1}.db`);
    backupDatabase(context.sqlite, fullSeed, database, context.env);
    const result = run(context.binary, [
      '--config', context.config,
      'enrich', context.corpus,
      '--database', database,
      '--sidecar-path', context.checkerSidecar,
      '--timeout', '300', '--all',
    ], { env: context.env, timeoutMs: 10 * 60_000 });
    reuseReport = JSON.parse(result.stdout);
    validateReport(reuseReport, {
      occurrences_queried: 0,
      occurrences_selected: ENRICHMENT_INVARIANTS.occurrences_selected,
      occurrences_omitted: 0,
      occurrences_resumed: ENRICHMENT_INVARIANTS.occurrences_resumed,
      request_batches: 0,
      unknown_answers: 0,
      facts_published: ENRICHMENT_INVARIANTS.facts_published,
      dry_run: false,
    }, 'unchanged enrichment');
    integrityCheck(context, database);
    reuseSamples.push(result.elapsedMs);
    removeDatabaseFamily(context, database);
  }
  const measurements = [
    measurement(
      'enrichment.plan',
      { all: true, dry_run: true },
      { wall_ms: sampledMetric(dryRunSamples, 'process wall, including startup') },
      {
        occurrences_selected: dryRunReport.occurrences_selected,
        projects: dryRunReport.projects,
      },
      { fixture_invariants_match: true },
    ),
    measurement(
      'enrichment.full',
      { all: true, full: true },
      { wall_ms: sampledMetric([full.elapsedMs], 'process wall, including startup') },
      {
        occurrences_selected: fullReport.occurrences_selected,
        request_batches: fullReport.request_batches,
        projects: fullReport.projects,
        facts_published: fullReport.facts_published,
        combined_projected_facts: fullProjection.combined_facts,
        unknown_answers: fullReport.unknown_answers,
        unknown_projects: fullReport.unknown_projects.length,
        configuration_problems: fullReport.configuration_problems,
        completed_project_runs: fullProjectRuns.completed_projects,
        completed_project_occurrences: fullProjectRuns.completed_occurrences,
      },
      { completed: true, fixture_invariants_match: true, integrity_check: fullIntegrity },
    ),
    measurement(
      'enrichment.unchanged_reuse',
      { all: true },
      { wall_ms: sampledMetric(reuseSamples, 'process wall, including startup') },
      {
        occurrences_resumed: reuseReport.occurrences_resumed,
        request_batches: reuseReport.request_batches,
      },
      { zero_checker_requests: true, fixture_invariants_match: true, integrity_check: 'ok' },
    ),
  ];
  removeDatabaseFamily(context, fullSeed);
  rmSync(corpusModules, { force: true });
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
  options.checkerSidecar = resolve(options.checkerSidecar);
  if (options.nodeModules) options.nodeModules = resolve(options.nodeModules);
  if (options.revision !== AI_PIPE_REVISION) {
    throw new Error(`fixture is pinned to ${AI_PIPE_REVISION}; received ${options.revision}`);
  }
  if (!isExecutable(options.binary)) throw new Error(`JScout binary is not executable: ${options.binary}`);
  if (!existsSync(options.repo)) throw new Error(`repository not found: ${options.repo}`);
  if (existsSync(options.output) && !options.force) throw new Error(`output already exists: ${options.output}`);
  const checkerPackageRoot = findPackageRoot(options.checkerSidecar);
  const protectedOutputRoots = [
    options.repo,
    projectRoot,
    options.binary,
    options.checkerSidecar,
  ];
  if (checkerPackageRoot) protectedOutputRoots.push(checkerPackageRoot);
  if (options.nodeModules) protectedOutputRoots.push(options.nodeModules);
  refusePathWithin(options.output, protectedOutputRoots);
  const binaryBefore = fileMetadata(options.binary);

  const suites = selectedSuites(options.suite);
  if (suites.includes('enrichment')) {
    if (!options.nodeModules) throw new Error('--node-modules is required by the enrichment suite');
    if (!existsSync(options.checkerSidecar)) throw new Error(`checker sidecar not found: ${options.checkerSidecar}`);
    if (!existsSync(options.nodeModules)) throw new Error(`node_modules not found: ${options.nodeModules}`);
  }
  const counts = countsForMode(options);
  const workspace = makeWorkspace();
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
    const resolvedRevision = commandOutput('git', ['-C', options.repo, 'rev-parse', options.revision], { env: bootstrapEnv });
    if (resolvedRevision !== AI_PIPE_REVISION) throw new Error(`unexpected corpus revision: ${resolvedRevision}`);
    sourceStatusBefore = commandOutput('git', ['-C', options.repo, 'status', '--porcelain=v2', '--untracked-files=all'], { env: bootstrapEnv });
    const corpus = join(workspace, 'corpus');
    stageGitArchive(options.repo, resolvedRevision, corpus, bootstrapEnv);
    const trackedFiles = Number(commandOutput('git', ['-C', options.repo, 'ls-tree', '-r', '--name-only', resolvedRevision], { env: bootstrapEnv }).split('\n').filter(Boolean).length);
    if (trackedFiles !== CORPUS_INVARIANTS.tracked_files) {
      throw new Error(`expected ${CORPUS_INVARIANTS.tracked_files} tracked ai-pipe files, got ${trackedFiles}`);
    }

    const env = childEnvironment(workspace, {
      JSCOUT_TASK_ID: 'ai-pipe-performance-harness',
    });
    const config = join(workspace, 'performance.toml');
    writeFileSync(config, `version = 1

[diagnostics]
timing = true
debug = false

[sidecars]
node = ${JSON.stringify(process.execPath)}

[watch]
embed = false
enrich = false
debounce_ms = 100
reconcile_seconds = 0
`);
    const context = {
      binary: options.binary,
      checkerSidecar: options.checkerSidecar,
      children,
      config,
      corpus,
      counts,
      env,
      interruptedSignal: () => interruptedSignal,
      keepWorkdir: options.keepWorkdir,
      nodeModules: options.nodeModules,
      sqlite: options.sqlite,
      workspace,
    };
    commandOutput(options.sqlite, ['--version'], { env });
    const [sqlitePreflight] = sqliteJson(options.sqlite, ':memory:', 'SELECT 1 AS value;', env);
    assertEqual(sqlitePreflight?.value, 1, 'sqlite3 JSON output preflight');
    const jscoutVersion = commandOutput(options.binary, ['--version'], { env });
    const measurements = [];
    measurements.push(...runIndexSuite(context));
    for (const suite of suites.filter((entry) => entry !== 'index')) {
      assertActive(context);
      if (suite === 'watch') measurements.push(...await runWatchSuite(context));
      if (suite === 'enrichment') measurements.push(...runEnrichmentSuite(context));
      if (suite === 'search') measurements.push(...await runSearchSuite(context));
      if (suite === 'neighborhood') measurements.push(...await runNeighborhoodSuite(context));
      if (suite === 'scout') measurements.push(...runScoutSuite(context));
      if (suite === 'embedding') measurements.push(...await runEmbeddingSuite(context));
      assertActive(context);
    }
    assertActive(context);
    const binaryAfter = fileMetadata(options.binary);
    if (
      binaryAfter.bytes !== binaryBefore.bytes
      || binaryAfter.sha256 !== binaryBefore.sha256
    ) {
      throw new Error('JScout binary changed during benchmark');
    }
    const sourceStatusAfter = commandOutput('git', ['-C', options.repo, 'status', '--porcelain=v2', '--untracked-files=all'], { env: bootstrapEnv });
    if (sourceStatusAfter !== sourceStatusBefore) throw new Error('source repository status changed during benchmark');
    const jscoutCommit = commandOutput('git', ['-C', projectRoot, 'rev-parse', 'HEAD'], { env });
    const jscoutTree = commandOutput('git', ['-C', projectRoot, 'rev-parse', 'HEAD^{tree}'], { env });
    const jscoutStatus = commandOutput('git', ['-C', projectRoot, 'status', '--porcelain=v2', '--untracked-files=all'], { env });
    const compactMetadata = (path) => {
      const metadata = fileMetadata(path);
      return { bytes: metadata.bytes, sha256: metadata.sha256 };
    };
    const checkerRoot = checkerPackageRoot ?? dirname(options.checkerSidecar);
    const checkerLock = join(checkerRoot, 'package-lock.json');
    const checkerDependencyLock = join(checkerRoot, 'node_modules/.package-lock.json');
    const dependencyLock = options.nodeModules ? join(options.nodeModules, '.package-lock.json') : null;
    const enrichmentDependencies = suites.includes('enrichment') ? {
      checker_sidecar: compactMetadata(options.checkerSidecar),
      checker_lock: existsSync(checkerLock) ? compactMetadata(checkerLock) : null,
      checker_node_modules_lock: existsSync(checkerDependencyLock)
        ? compactMetadata(checkerDependencyLock)
        : null,
      external_node_modules_lock: dependencyLock && existsSync(dependencyLock)
        ? compactMetadata(dependencyLock)
        : null,
      node_runtime: compactMetadata(process.execPath),
      node_runtime_version: process.version,
      dependency_tree_source: 'external checkout; contents are not copied or fully hashed',
    } : null;
    removeDatabaseFamily(context, context.seedDatabase);
    const report = {
      schema: 'jscout.performance.v1',
      generated_at: new Date().toISOString(),
      provenance: {
        harness_source_commit: jscoutCommit,
        harness_source_tree: jscoutTree,
        binary: {
          bytes: binaryBefore.bytes,
          sha256: binaryBefore.sha256,
          version: jscoutVersion,
          source_verification: 'unverified caller-provided artifact',
          stable_during_run: true,
        },
        harness_source_dirty: jscoutStatus.length > 0,
        harness_files: {
          orchestrator: compactMetadata(fileURLToPath(import.meta.url)),
          library: compactMetadata(join(scriptDirectory, 'lib.mjs')),
          fixture: compactMetadata(join(scriptDirectory, 'ai-pipe-fixture.mjs')),
          mock_inference: compactMetadata(mockInference),
          mock_gateway: compactMetadata(mockGateway),
        },
        enrichment_dependencies: enrichmentDependencies,
        corpus: 'ai-pipe',
        corpus_commit: resolvedRevision,
        corpus_tracked_files: trackedFiles,
        corpus_lock_sha256: existsSync(join(corpus, 'package-lock.json'))
          ? fileMetadata(join(corpus, 'package-lock.json')).sha256
          : null,
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
        suite: options.suite,
        suites,
        sample_counts: counts,
        requested_build_profile: 'release',
        binary_build_profile_verified: false,
        remote_model_requests: 0,
        corpus_staging: 'git archive of the pinned revision',
        databases: 'unique filenames; checkpointed SQLite backups for shared pre-states',
        filesystem_cache: 'uncontrolled; first post-archive index is reported separately',
        timing: 'monotonic wall clock; persistent MCP cases exclude process startup',
      },
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
