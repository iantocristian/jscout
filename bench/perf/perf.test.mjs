import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import {
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  symlinkSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  McpClient,
  childEnvironment,
  distribution,
  findPackageRoot,
  nearestRank,
  pathIsWithin,
  refusePathWithin,
  stopChild,
} from './lib.mjs';

test('statistics use one nearest-rank definition and omit tiny-sample p95', () => {
  assert.equal(nearestRank([9, 1, 5, 3], 0.5), 3);
  assert.deepEqual(distribution([9, 1, 5, 3]), {
    n: 4,
    min: 1,
    median: 3,
    max: 9,
  });
  const twenty = distribution(Array.from({ length: 20 }, (_, index) => index + 1));
  assert.equal(twenty.p95, 19);
});

test('benchmark child environments do not inherit provider or auth settings', () => {
  const workspace = mkdtempSync(join(tmpdir(), 'jscout-perf-env-test-'));
  process.env.JSCOUT_EMBED_URL = 'https://must-not-leak.invalid';
  process.env.OPENAI_API_KEY = 'must-not-leak';
  process.env.NODE_OPTIONS = '--require=/must/not/leak';
  process.env.DYLD_INSERT_LIBRARIES = '/must/not/leak.dylib';
  process.env.LD_PRELOAD = '/must/not/leak.so';
  process.env.TMPDIR = '/must/not/leak-tmp';
  try {
    const environment = childEnvironment(workspace);
    assert.equal(environment.JSCOUT_EMBED_URL, undefined);
    assert.equal(environment.OPENAI_API_KEY, undefined);
    assert.equal(environment.NODE_OPTIONS, undefined);
    assert.equal(environment.DYLD_INSERT_LIBRARIES, undefined);
    assert.equal(environment.LD_PRELOAD, undefined);
    assert.equal(environment.HOME, join(workspace, 'home'));
    assert.equal(environment.TMPDIR, join(workspace, 'tmp'));
    assert.equal(environment.TMP, join(workspace, 'tmp'));
    assert.equal(environment.TEMP, join(workspace, 'tmp'));
  } finally {
    delete process.env.JSCOUT_EMBED_URL;
    delete process.env.OPENAI_API_KEY;
    delete process.env.NODE_OPTIONS;
    delete process.env.DYLD_INSERT_LIBRARIES;
    delete process.env.LD_PRELOAD;
    delete process.env.TMPDIR;
    rmSync(workspace, { recursive: true, force: true });
  }
});

test('path guards reject source descendants and symlink aliases', () => {
  const workspace = mkdtempSync(join(tmpdir(), 'jscout-perf-path-test-'));
  const source = join(workspace, 'source');
  const outside = join(workspace, 'outside');
  mkdirSync(source);
  mkdirSync(outside);
  const alias = join(outside, 'source-alias');
  symlinkSync(source, alias, 'dir');
  const existingOutput = join(outside, 'result.json');
  const target = join(outside, 'target.json');
  const binary = join(outside, 'jscout');
  writeFileSync(target, '{}');
  writeFileSync(binary, 'binary fixture');
  symlinkSync(target, existingOutput);
  try {
    assert.equal(pathIsWithin(source, join(source, 'child')), true);
    assert.equal(pathIsWithin(source, outside), false);
    assert.throws(() => refusePathWithin(join(source, 'result.json'), [source]), /within source data/);
    assert.throws(() => refusePathWithin(join(alias, 'nested', 'result.json'), [source]), /within source data/);
    assert.throws(() => refusePathWithin(existingOutput, [source]), /symbolic-link output/);
    assert.throws(() => refusePathWithin(binary, [binary]), /within source data/);
    assert.doesNotThrow(() => refusePathWithin(join(outside, 'safe.json'), [source]));
  } finally {
    rmSync(workspace, { recursive: true, force: true });
  }
});

test('package-root discovery does not assume a fixed sidecar depth', () => {
  const workspace = mkdtempSync(join(tmpdir(), 'jscout-perf-package-test-'));
  const packageRoot = join(workspace, 'checker');
  const sidecar = join(packageRoot, 'nested', 'main.mjs');
  const shallowSidecar = join(workspace, 'standalone.mjs');
  mkdirSync(join(packageRoot, 'nested'), { recursive: true });
  writeFileSync(join(packageRoot, 'package.json'), '{}');
  writeFileSync(sidecar, '');
  writeFileSync(shallowSidecar, '');
  try {
    assert.equal(findPackageRoot(sidecar), realpathSync(packageRoot));
    assert.equal(findPackageRoot(shallowSidecar), null);
  } finally {
    rmSync(workspace, { recursive: true, force: true });
  }
});

test('mock inference binds dynamically and returns deterministic dense unit vectors', async (t) => {
  const child = spawn(process.execPath, [
    fileURLToPath(new URL('./mock-inference.mjs', import.meta.url)),
    '--port', '0', '--dimensions', '16',
  ], { stdio: ['ignore', 'pipe', 'pipe'] });
  t.after(() => child.kill('SIGTERM'));
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  let startupTimer;
  const ready = await Promise.race([
    new Promise((resolveReady) => lines.once('line', (line) => resolveReady(JSON.parse(line)))),
    new Promise((_, reject) => {
      startupTimer = setTimeout(() => reject(new Error(`mock startup timed out: ${stderr}`)), 5_000);
    }),
  ]);
  clearTimeout(startupTimer);
  assert.match(ready.url, /^http:\/\/127\.0\.0\.1:\d+$/);

  const configuration = await (await fetch(`${ready.url}/configuration`)).json();
  assert.equal(configuration.embedding.dimensions, 16);
  assert.equal(configuration.embedding.configuration.normalize, true);

  const request = { model: ready.model, texts: ['same input', 'same input'] };
  const response = await fetch(`${ready.url}/embed`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(request),
  });
  assert.equal(response.status, 200);
  const payload = await response.json();
  assert.deepEqual(payload.vectors[0], payload.vectors[1]);
  assert.equal(payload.vectors[0].length, 16);
  const norm = Math.sqrt(payload.vectors[0].reduce((sum, value) => sum + value * value, 0));
  assert.ok(Math.abs(norm - 1) < 1e-12);
  assert.ok(payload.vectors[0].filter((value) => value !== 0).length > 8);

  const stats = await (await fetch(`${ready.url}/stats`)).json();
  assert.equal(stats.embedRequests, 1);
  assert.equal(stats.texts, 2);
  const missing = await fetch(`${ready.url}/missing`);
  assert.equal(missing.status, 404);

  child.kill('SIGTERM');
  const exitCode = await new Promise((resolveExit) => child.once('close', resolveExit));
  assert.equal(exitCode, 0, stderr);
});

test('mock gateway completes one deterministic card request and shuts down', async (t) => {
  const child = spawn(process.execPath, [
    fileURLToPath(new URL('./mock-gateway.mjs', import.meta.url)),
  ], { stdio: ['pipe', 'pipe', 'pipe'] });
  t.after(() => child.kill('SIGTERM'));
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  const queued = [];
  const waiting = [];
  lines.on('line', (line) => {
    const value = JSON.parse(line);
    const resolveNext = waiting.shift();
    if (resolveNext) resolveNext(value);
    else queued.push(value);
  });
  const next = () => queued.length > 0
    ? Promise.resolve(queued.shift())
    : new Promise((resolveNext) => waiting.push(resolveNext));
  const send = (value) => child.stdin.write(`${JSON.stringify({ protocol: 1, ...value })}\n`);

  send({ id: 'hello', kind: 'hello' });
  assert.equal((await next()).kind, 'ready');
  send({ id: 'capabilities', kind: 'capabilities' });
  assert.equal((await next()).kind, 'capabilities_result');
  send({
    id: 'complete',
    kind: 'complete',
    tool: { name: 'submit_symbol_card' },
    input: { anchor: 'sym:server/brokers/riskPolicy.mjs#::evaluateBrokerRiskPolicy@1' },
  });
  assert.equal((await next()).kind, 'started');
  const result = await next();
  assert.equal(result.kind, 'result');
  assert.equal(result.tool_call.name, 'submit_symbol_card');
  send({ id: 'shutdown', kind: 'shutdown' });
  assert.equal((await next()).kind, 'shutdown_result');
  const exitCode = await new Promise((resolveExit) => child.once('close', resolveExit));
  assert.equal(exitCode, 0, stderr);
});

test('MCP close gives an EOF-aware process a graceful shutdown window', async () => {
  const client = new McpClient({
    binary: process.execPath,
    args: ['-e', `
      process.on('SIGTERM', () => process.exit(42));
      process.stdin.resume();
      process.stdin.on('end', () => process.exit(0));
    `],
    env: process.env,
  });
  await client.close();
  assert.equal(client.child.exitCode, 0);
});

test('MCP requests reject when the child exits unsuccessfully', async () => {
  const client = new McpClient({
    binary: process.execPath,
    args: ['-e', `process.stdin.once('data', () => process.exit(7));`],
    env: process.env,
  });
  await assert.rejects(client.request('fixture', {}), /MCP exited code=7/);
  await assert.rejects(client.close(), /MCP exited code=7/);
});

test('stopChild escalates from TERM to KILL for a stubborn process', async (t) => {
  const child = spawn(process.execPath, ['-e', `
    process.on('SIGTERM', () => {});
    process.stdout.write('ready\\n');
    setInterval(() => {}, 1_000);
  `], { stdio: ['ignore', 'pipe', 'ignore'] });
  t.after(() => child.kill('SIGKILL'));
  const lines = createInterface({ input: child.stdout, crlfDelay: Infinity });
  await new Promise((resolveReady) => lines.once('line', resolveReady));
  await stopChild(child, 25);
  assert.equal(child.signalCode, 'SIGKILL');
});

test('checked historical result is valid JSON and identifies its different schema', () => {
  const path = fileURLToPath(new URL('../results/ai-pipe-2026-08-21.json', import.meta.url));
  const result = JSON.parse(readFileSync(path, 'utf8'));
  assert.equal(result.generated_by_checked_harness, false);
  assert.equal(result.future_harness_schema, 'jscout.performance.v1');
  assert.equal(result.provenance.ai_pipe_commit.length, 40);
});
