import { spawn, spawnSync } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import {
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  realpathSync,
  renameSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { createInterface } from 'node:readline';
import { performance } from 'node:perf_hooks';

export const nearestRank = (values, fraction) => {
  if (values.length === 0) return null;
  const sorted = values.toSorted((left, right) => left - right);
  return sorted[Math.max(0, Math.ceil(sorted.length * fraction) - 1)];
};

export const distribution = (values) => {
  if (values.length === 0) return null;
  const sorted = values.toSorted((left, right) => left - right);
  const summary = {
    n: sorted.length,
    min: sorted[0],
    median: nearestRank(sorted, 0.5),
    max: sorted.at(-1),
  };
  if (sorted.length >= 20) summary.p95 = nearestRank(sorted, 0.95);
  return summary;
};

export function sha256Bytes(value) {
  return createHash('sha256').update(value).digest('hex');
}

export function sha256File(path) {
  return sha256Bytes(readFileSync(path));
}

export function childEnvironment(workdir, additions = {}) {
  const inherited = [
    'PATH',
    'LANG',
    'LC_ALL',
    'RUST_BACKTRACE',
    'SYSTEMROOT',
    'WINDIR',
  ];
  const environment = {};
  for (const key of inherited) {
    if (process.env[key] !== undefined) environment[key] = process.env[key];
  }
  const home = join(workdir, 'home');
  const config = join(home, '.config');
  const cache = join(home, '.cache');
  const temporary = join(workdir, 'tmp');
  mkdirSync(config, { recursive: true });
  mkdirSync(cache, { recursive: true });
  mkdirSync(temporary, { recursive: true });
  return {
    ...environment,
    HOME: home,
    XDG_CONFIG_HOME: config,
    XDG_CACHE_HOME: cache,
    TMPDIR: temporary,
    TMP: temporary,
    TEMP: temporary,
    NO_COLOR: '1',
    ...additions,
  };
}

export function run(command, args, options = {}) {
  const started = performance.now();
  const result = spawnSync(command, args, {
    cwd: options.cwd,
    env: options.env,
    encoding: options.encoding ?? 'utf8',
    input: options.input,
    maxBuffer: options.maxBuffer ?? 128 * 1024 * 1024,
    timeout: options.timeoutMs ?? 10 * 60 * 1_000,
  });
  const elapsedMs = performance.now() - started;
  if (result.error) {
    throw new Error(`${command} failed to start: ${result.error.message}`);
  }
  if (result.signal) {
    throw new Error(`${command} ${args.join(' ')} ended by ${result.signal}`);
  }
  if (result.status !== 0) {
    throw new Error(
      `${command} ${args.join(' ')} exited ${result.status}\n${result.stderr ?? ''}`,
    );
  }
  return {
    stdout: result.stdout ?? '',
    stderr: result.stderr ?? '',
    elapsedMs,
  };
}

export function commandOutput(command, args, options = {}) {
  return run(command, args, options).stdout.trim();
}

export function makeWorkspace(prefix = 'jscout-ai-pipe-perf-') {
  return mkdtempSync(join(tmpdir(), prefix));
}

export function pathIsWithin(parent, candidate) {
  const difference = relative(resolve(parent), resolve(candidate));
  return difference === '' || (!difference.startsWith(`..${sep}`) && difference !== '..' && !isAbsolute(difference));
}

export function findPackageRoot(file) {
  let current = dirname(existsSync(file) ? realpathSync(file) : resolve(file));
  while (true) {
    if (existsSync(join(current, 'package.json'))) return current;
    const parent = dirname(current);
    if (parent === current) return null;
    current = parent;
  }
}

function lstatIfPresent(path) {
  try {
    return lstatSync(path);
  } catch (error) {
    if (error.code === 'ENOENT') return null;
    throw error;
  }
}

function resolveThroughExistingAncestor(path) {
  let current = resolve(path);
  const missing = [];
  while (!existsSync(current)) {
    const parent = dirname(current);
    if (parent === current) break;
    missing.unshift(basename(current));
    current = parent;
  }
  return resolve(realpathSync(current), ...missing);
}

export function refusePathWithin(candidate, forbidden) {
  const lexicalCandidate = resolve(candidate);
  if (lexicalCandidate === resolve(sep)) {
    throw new Error(`refusing dangerous output path: ${lexicalCandidate}`);
  }
  if (lstatIfPresent(lexicalCandidate)?.isSymbolicLink()) {
    throw new Error(`refusing symbolic-link output path: ${lexicalCandidate}`);
  }
  const physicalCandidate = resolveThroughExistingAncestor(lexicalCandidate);
  for (const item of forbidden) {
    const lexicalRoot = resolve(item);
    const physicalRoot = resolveThroughExistingAncestor(lexicalRoot);
    if (
      pathIsWithin(lexicalRoot, lexicalCandidate)
      || pathIsWithin(physicalRoot, physicalCandidate)
    ) {
      throw new Error(`refusing output path within source data: ${lexicalCandidate}`);
    }
  }
}

function walkSymlinks(root, current = root, links = []) {
  for (const entry of readdirSync(current, { withFileTypes: true })) {
    const path = join(current, entry.name);
    if (entry.isSymbolicLink()) links.push(path);
    else if (entry.isDirectory()) walkSymlinks(root, path, links);
  }
  return links;
}

export function assertContainedSymlinks(root) {
  for (const link of walkSymlinks(root)) {
    const target = readlinkSync(link);
    const resolvedTarget = resolve(dirname(link), target);
    if (!pathIsWithin(root, resolvedTarget)) {
      throw new Error(`archive contains escaping symlink: ${relative(root, link)} -> ${target}`);
    }
  }
}

export function stageGitArchive(source, revision, destination, env) {
  mkdirSync(destination, { recursive: true });
  const archive = join(dirname(destination), 'corpus.tar');
  run('git', ['-C', source, 'archive', '--format=tar', `--output=${archive}`, revision], { env });
  run('tar', ['-xf', archive, '-C', destination], { env });
  rmSync(archive, { force: true });
  assertContainedSymlinks(destination);
}

export function checkpointDatabase(sqlite, database, env) {
  const output = commandOutput(
    sqlite,
    [database, 'PRAGMA wal_checkpoint(TRUNCATE); PRAGMA integrity_check;'],
    { env },
  );
  if (output.split('\n').at(-1) !== 'ok') {
    throw new Error(`SQLite source failed integrity check: ${output}`);
  }
}

export function backupDatabase(sqlite, source, destination, env) {
  mkdirSync(dirname(destination), { recursive: true });
  for (const suffix of ['', '-wal', '-shm', '-journal']) {
    if (lstatIfPresent(`${destination}${suffix}`)) {
      throw new Error(`SQLite backup destination already exists: ${destination}${suffix}`);
    }
  }
  checkpointDatabase(sqlite, source, env);
  const escaped = destination.replaceAll("'", "''");
  run(sqlite, [source, `.backup '${escaped}'`], { env });
  const integrity = commandOutput(sqlite, [destination, 'PRAGMA integrity_check;'], { env });
  if (integrity !== 'ok') throw new Error(`SQLite backup failed integrity check: ${integrity}`);
}

export function writeJsonExclusive(path, value, force = false) {
  mkdirSync(dirname(path), { recursive: true });
  if (!force) {
    writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx' });
    return;
  }
  if (lstatIfPresent(path)?.isSymbolicLink()) {
    throw new Error(`refusing to overwrite symbolic-link output: ${path}`);
  }
  const temporary = join(dirname(path), `.${basename(path)}.${randomUUID()}.tmp`);
  try {
    writeFileSync(temporary, `${JSON.stringify(value, null, 2)}\n`, { flag: 'wx' });
    renameSync(temporary, path);
  } finally {
    rmSync(temporary, { force: true });
  }
}

export function fileMetadata(path) {
  const stats = statSync(path);
  return {
    bytes: stats.size,
    sha256: sha256File(path),
  };
}

export class McpClient {
  constructor({ binary, args, env, timeoutMs = 30_000 }) {
    this.timeoutMs = timeoutMs;
    this.nextId = 1;
    this.pending = new Map();
    this.stderr = '';
    this.closed = false;
    this.closing = false;
    this.exitError = null;
    this.child = spawn(binary, args, { stdio: ['pipe', 'pipe', 'pipe'], env });
    this.child.stderr.setEncoding('utf8');
    this.child.stderr.on('data', (chunk) => { this.stderr += chunk; });
    this.lines = createInterface({ input: this.child.stdout, crlfDelay: Infinity });
    this.lines.on('line', (line) => {
      let response;
      try {
        response = JSON.parse(line);
      } catch (error) {
        this.failAll(new Error(`invalid MCP response: ${error.message}: ${line.slice(0, 200)}`));
        return;
      }
      const pending = this.pending.get(response.id);
      if (!pending) return;
      this.pending.delete(response.id);
      clearTimeout(pending.timer);
      pending.resolve({ response, elapsedMs: performance.now() - pending.started });
    });
    this.child.once('error', (error) => this.failAll(error));
    this.child.once('close', (code, signal) => {
      this.closed = true;
      if ((code !== 0 && code !== null) || (signal && !this.closing)) {
        this.exitError = new Error(`MCP exited code=${code} signal=${signal}: ${this.stderr}`);
      }
      if (this.pending.size > 0) this.failAll(this.exitError ?? new Error(`MCP exited signal=${signal}`));
    });
  }

  failAll(error) {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }

  request(method, params) {
    if (this.closed) return Promise.reject(new Error('MCP process is closed'));
    const id = this.nextId++;
    return new Promise((resolveRequest, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`MCP request ${id} timed out after ${this.timeoutMs}ms`));
        void stopChild(this.child);
      }, this.timeoutMs);
      this.pending.set(id, { resolve: resolveRequest, reject, timer, started: performance.now() });
      this.child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`, (error) => {
        if (!error) return;
        const pending = this.pending.get(id);
        if (!pending) return;
        clearTimeout(pending.timer);
        this.pending.delete(id);
        reject(error);
      });
    });
  }

  async initialize(name = 'jscout-performance-harness') {
    const { response } = await this.request('initialize', {
      protocolVersion: '2025-06-18',
      capabilities: {},
      clientInfo: { name, version: '1' },
    });
    if (response.error) throw new Error(`MCP initialize failed: ${JSON.stringify(response.error)}`);
  }

  async close() {
    if (!this.closed) {
      this.closing = true;
      this.child.stdin.end();
      if (!(await waitForClose(this.child, 5_000))) await stopChild(this.child);
    }
    if (this.exitError) throw this.exitError;
  }
}

function waitForClose(child, timeoutMs) {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve(true);
  return new Promise((resolveWait) => {
    const timer = setTimeout(() => {
      child.removeListener('close', onClose);
      resolveWait(false);
    }, timeoutMs);
    const onClose = () => {
      clearTimeout(timer);
      resolveWait(true);
    };
    child.once('close', onClose);
  });
}

export async function stopChild(child, graceMs = 2_000) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  child.kill('SIGTERM');
  if (await waitForClose(child, graceMs)) return;
  child.kill('SIGKILL');
  await waitForClose(child, 1_000);
}

export async function stopChildren(children, graceMs = 2_000) {
  await Promise.all([...children].map((child) => stopChild(child, graceMs)));
  children.clear();
}

export function parseTextToolResponse(response, label) {
  if (response.error) throw new Error(`${label}: ${JSON.stringify(response.error)}`);
  const text = response.result?.content?.[0]?.text;
  if (typeof text !== 'string') throw new Error(`${label}: missing text MCP result`);
  return { text, value: JSON.parse(text) };
}

export function isExecutable(path) {
  try {
    return lstatSync(path).isFile() && (statSync(path).mode & 0o111) !== 0;
  } catch {
    return false;
  }
}
