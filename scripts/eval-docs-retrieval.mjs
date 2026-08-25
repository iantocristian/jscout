#!/usr/bin/env node

import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { performance } from "node:perf_hooks";

const scriptPath = fileURLToPath(import.meta.url);
const projectRoot = resolve(dirname(scriptPath), "..");
const defaultManifest = join(projectRoot, "eval/fixtures/docs-retrieval/manifest.json");
const defaultBinary = join(projectRoot, "target/release/jscout");
const MAX_K = 20;
const RESPONSE_BYTES = 1_048_576;
const CUTOFFS = [1, 3, 5, 10];
const PROFILE_NAMES = new Set(["lexical", "fallback", "hybrid", "hybrid-rerank"]);
const PHASE2_PROFILES = ["lexical", "fallback", "hybrid", "hybrid-rerank"];
const PROVIDER_FREE_PROFILES = ["lexical", "fallback"];
const EMBEDDING_MODEL = "BAAI/bge-m3";
const EMBEDDING_REVISION = "5617a9f61b028005a4858fdac845db406aefb181";
const RERANKER_MODEL = "BAAI/bge-reranker-v2-m3";
const RERANKER_REVISION = "953dc6f6f85a1b2dbfca4c34a2796e7dde08d41e";

const usage = `Fixed-corpus evaluation for G24 documentation retrieval.

Usage:
  node scripts/eval-docs-retrieval.mjs --output FILE [options]

Options:
  --binary PATH             jscout binary (default: target/release/jscout)
  --manifest PATH           fixed corpus manifest
  --run-kind NAME           provider-free-check (default) or phase2-baseline
  --profiles LIST           comma-separated lexical,fallback,hybrid,hybrid-rerank
                            (default: lexical,fallback; hybrid arms require --provider-config)
  --provider-config PATH    explicit config used only for docs embed/vector/rerank arms
  --pass-env LIST           comma-separated secret variable names copied to provider calls
  --workdir PATH            empty/nonexistent isolated workspace to use
  --keep-workdir            retain the generated repositories and databases
  --force                   replace an existing output file
  --help                    show this text
`;

export function parseArguments(argv) {
  const options = {
    binary: defaultBinary,
    manifest: defaultManifest,
    runKind: "provider-free-check",
    profiles: [...PROVIDER_FREE_PROFILES],
    passEnv: [],
    keepWorkdir: false,
    force: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--help") return { help: true };
    if (argument === "--keep-workdir") {
      options.keepWorkdir = true;
      continue;
    }
    if (argument === "--force") {
      options.force = true;
      continue;
    }
    const key = {
      "--binary": "binary",
      "--manifest": "manifest",
      "--output": "output",
      "--run-kind": "runKind",
      "--profiles": "profiles",
      "--provider-config": "providerConfig",
      "--pass-env": "passEnv",
      "--workdir": "workdir",
    }[argument];
    if (!key) throw new Error(`unknown argument: ${argument}\n\n${usage}`);
    const value = argv[++index];
    if (value === undefined) throw new Error(`missing value for ${argument}`);
    options[key] = value;
  }
  if (!options.output) throw new Error(`--output is required\n\n${usage}`);
  options.binary = resolve(options.binary);
  options.manifest = resolve(options.manifest);
  options.output = resolve(options.output);
  options.providerConfig = options.providerConfig ? resolve(options.providerConfig) : null;
  options.workdir = options.workdir ? resolve(options.workdir) : null;
  options.profiles = Array.isArray(options.profiles)
    ? options.profiles
    : options.profiles.split(",").map((value) => value.trim()).filter(Boolean);
  options.passEnv = Array.isArray(options.passEnv)
    ? options.passEnv
    : options.passEnv.split(",").map((value) => value.trim()).filter(Boolean);
  if (options.profiles.length === 0) throw new Error("--profiles cannot be empty");
  if (new Set(options.profiles).size !== options.profiles.length) {
    throw new Error("--profiles cannot contain duplicates");
  }
  for (const profile of options.profiles) {
    if (!PROFILE_NAMES.has(profile)) throw new Error(`unknown profile: ${profile}`);
  }
  if (options.profiles.some((profile) => profile.startsWith("hybrid")) && !options.providerConfig) {
    throw new Error("hybrid profiles require --provider-config");
  }
  if (!new Set(["provider-free-check", "phase2-baseline"]).has(options.runKind)) {
    throw new Error(`unknown --run-kind: ${options.runKind}`);
  }
  const requiredProfiles = options.runKind === "phase2-baseline" ? PHASE2_PROFILES : PROVIDER_FREE_PROFILES;
  if (options.profiles.length !== requiredProfiles.length
      || requiredProfiles.some((profile) => !options.profiles.includes(profile))) {
    throw new Error(`${options.runKind} requires exactly: ${requiredProfiles.join(",")}`);
  }
  for (const name of options.passEnv) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) throw new Error(`invalid environment name: ${name}`);
    if (process.env[name] === undefined) throw new Error(`--pass-env variable is not set: ${name}`);
  }
  return options;
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function sha256File(path) {
  return sha256(readFileSync(path));
}

function directoryDigest(root) {
  const entries = [];
  function visit(directory) {
    for (const entry of readdirSync(directory, { withFileTypes: true }).toSorted((a, b) => {
      if (a.name < b.name) return -1;
      if (a.name > b.name) return 1;
      return 0;
    })) {
      const path = join(directory, entry.name);
      if (entry.isDirectory()) visit(path);
      else if (entry.isFile()) entries.push([relative(root, path), sha256File(path)]);
      else throw new Error(`fixture contains unsupported entry: ${path}`);
    }
  }
  visit(root);
  return sha256(`${entries.map(([path, digest]) => `${path}\0${digest}`).join("\n")}\n`);
}

function childEnvironment(workspace, additions = {}, passEnv = []) {
  const environment = {};
  for (const name of ["PATH", "LANG", "LC_ALL", "RUST_BACKTRACE", "SYSTEMROOT", "WINDIR"]) {
    if (process.env[name] !== undefined) environment[name] = process.env[name];
  }
  for (const name of passEnv) environment[name] = process.env[name];
  const temporary = join(workspace, "tmp");
  mkdirSync(temporary, { recursive: true });
  return {
    ...environment,
    TMPDIR: temporary,
    TMP: temporary,
    TEMP: temporary,
    NO_COLOR: "1",
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_CONFIG_NOSYSTEM: "1",
    ...additions,
  };
}

function run(command, args, options = {}) {
  const started = performance.now();
  const result = spawnSync(command, args, {
    cwd: options.cwd,
    env: options.env,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
    timeout: options.timeoutMs ?? 10 * 60 * 1_000,
  });
  const elapsedMs = performance.now() - started;
  if (result.error) throw new Error(`${command} failed to start: ${result.error.message}`);
  if (result.signal) throw new Error(`${command} ended by ${result.signal}`);
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(" ")} exited ${result.status}\n${result.stderr}`);
  }
  return { stdout: result.stdout, stderr: result.stderr, elapsedMs };
}

function output(command, args, options = {}) {
  return run(command, args, options).stdout.trim();
}

function readJson(path) {
  try {
    return JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    throw new Error(`${path}: ${error.message}`);
  }
}

function assertString(value, label) {
  if (typeof value !== "string" || value.length === 0) throw new Error(`${label} must be a non-empty string`);
}

function validateQrels(values, label) {
  if (!Array.isArray(values) || values.length === 0) throw new Error(`${label} must be a non-empty array`);
  const ids = new Set();
  for (const [index, value] of values.entries()) {
    if (!value || typeof value !== "object") throw new Error(`${label}[${index}] must be an object`);
    for (const key of ["answer_id", "path", "heading"]) assertString(value[key], `${label}[${index}].${key}`);
    validateRepositoryTarget(value.path, `${label}[${index}].path`);
    if (ids.has(value.answer_id)) throw new Error(`${label} repeats answer_id ${value.answer_id}`);
    ids.add(value.answer_id);
  }
}

export function validateManifest(manifest, manifestPath = "manifest") {
  if (!manifest || manifest.schema_version !== 1) throw new Error(`${manifestPath}: unsupported schema_version`);
  assertString(manifest.suite, `${manifestPath}.suite`);
  if (!Array.isArray(manifest.commits) || manifest.commits.length < 2) {
    throw new Error(`${manifestPath}.commits must contain at least two dated commits`);
  }
  const manifestRoot = dirname(resolve(manifestPath));
  const commitIds = new Set();
  let previousCommitTime = Number.NEGATIVE_INFINITY;
  for (const [index, commit] of manifest.commits.entries()) {
    for (const key of ["id", "date", "message"]) assertString(commit[key], `commits[${index}].${key}`);
    if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$/.test(commit.date)) {
      throw new Error(`commits[${index}].date must use YYYY-MM-DDTHH:mm:ssZ`);
    }
    const commitTime = Date.parse(commit.date);
    if (!Number.isFinite(commitTime)
        || new Date(commitTime).toISOString() !== commit.date.replace(/Z$/, ".000Z")) {
      throw new Error(`commits[${index}].date is not a valid UTC timestamp`);
    }
    if (commitTime <= previousCommitTime) {
      throw new Error(`commits[${index}].date must be later than the preceding commit`);
    }
    previousCommitTime = commitTime;
    if (commitIds.has(commit.id)) throw new Error(`duplicate commit id: ${commit.id}`);
    commitIds.add(commit.id);
    validateOverlay(commit.overlay, `commits[${index}].overlay`, manifestRoot);
    if (commit.delete !== undefined && !Array.isArray(commit.delete)) {
      throw new Error(`commits[${index}].delete must be an array`);
    }
    for (const [deleteIndex, target] of (commit.delete ?? []).entries()) {
      validateRepositoryTarget(target, `commits[${index}].delete[${deleteIndex}]`);
    }
  }
  if (!Array.isArray(manifest.variants) || manifest.variants.length === 0) {
    throw new Error(`${manifestPath}.variants must be a non-empty array`);
  }
  const variantIds = new Set();
  const allowedKinds = new Set(["clean", "shallow", "non_git", "dirty", "staged", "untracked"]);
  for (const [index, variant] of manifest.variants.entries()) {
    assertString(variant.id, `variants[${index}].id`);
    if (!/^[a-z0-9][a-z0-9_-]*$/.test(variant.id)) {
      throw new Error(`variants[${index}].id must be a lowercase filename-safe slug`);
    }
    assertString(variant.kind, `variants[${index}].kind`);
    if (!allowedKinds.has(variant.kind)) throw new Error(`unknown variant kind: ${variant.kind}`);
    if (variantIds.has(variant.id)) throw new Error(`duplicate variant id: ${variant.id}`);
    variantIds.add(variant.id);
    if (variant.overlay !== undefined) validateOverlay(variant.overlay, `variants[${index}].overlay`, manifestRoot);
    if (variant.kind === "shallow" && (!Number.isSafeInteger(variant.depth) || variant.depth < 1)) {
      throw new Error(`variants[${index}].depth must be a positive integer`);
    }
  }
  for (const required of ["clean", "shallow", "non_git", "dirty", "staged", "untracked"]) {
    if (![...manifest.variants].some((variant) => variant.kind === required)) {
      throw new Error(`manifest has no ${required} variant`);
    }
  }
  if (!Array.isArray(manifest.queries) || manifest.queries.length === 0) {
    throw new Error(`${manifestPath}.queries must be a non-empty array`);
  }
  const queryIds = new Set();
  const categories = new Set(["conflict", "evergreen", "canonical", "continuity", "working_tree"]);
  for (const [index, query] of manifest.queries.entries()) {
    for (const key of ["id", "query", "category"]) assertString(query[key], `queries[${index}].${key}`);
    if (!categories.has(query.category)) throw new Error(`unknown query category: ${query.category}`);
    if (queryIds.has(query.id)) throw new Error(`duplicate query id: ${query.id}`);
    queryIds.add(query.id);
    if (!Array.isArray(query.variants) || query.variants.length === 0) {
      throw new Error(`queries[${index}].variants must be a non-empty array`);
    }
    for (const variant of query.variants) {
      if (!variantIds.has(variant)) throw new Error(`${query.id}: unknown variant ${variant}`);
    }
    validateQrels(query.current, `${query.id}.current`);
    for (const field of ["older_conflicts", "recent_irrelevant"]) {
      if (query[field] !== undefined) validateQrels(query[field], `${query.id}.${field}`);
    }
    if (query.category === "conflict" && !query.older_conflicts?.length) {
      throw new Error(`${query.id}: conflict query requires older_conflicts`);
    }
    if (query.category === "evergreen" && !query.recent_irrelevant?.length) {
      throw new Error(`${query.id}: evergreen query requires recent_irrelevant`);
    }
  }
  return manifest;
}

function validateRepositoryTarget(target, label) {
  assertString(target, label);
  const parts = target.split("/");
  if (target.startsWith("/") || target.includes("\\")
      || parts.some((part) => part === "" || part === "." || part === "..")
      || parts[0].toLowerCase() === ".git") {
    throw new Error(`${label} is not a safe repository-relative path: ${target}`);
  }
}

function validateOverlay(overlay, label, manifestRoot) {
  if (!overlay || typeof overlay !== "object" || Array.isArray(overlay)) {
    throw new Error(`${label} must map repository paths to fixture files`);
  }
  for (const [target, source] of Object.entries(overlay)) {
    validateRepositoryTarget(target, `${label} target`);
    assertString(source, `${label}.${target}`);
    const sourcePath = resolve(manifestRoot, source);
    if (!existsSync(sourcePath) || !statSync(sourcePath).isFile()) {
      throw new Error(`${label} source does not exist: ${source}`);
    }
    if (!pathIsContained(realpathSync(manifestRoot), realpathSync(sourcePath))) {
      throw new Error(`${label} source escapes the fixture root: ${source}`);
    }
  }
}

function applyOverlay(root, overlay, manifestRoot) {
  for (const [target, source] of Object.entries(overlay ?? {})) {
    const destination = join(root, target);
    mkdirSync(dirname(destination), { recursive: true });
    cpSync(resolve(manifestRoot, source), destination);
  }
}

function git(repository, args, env) {
  return output("git", ["-C", repository, ...args], { env });
}

function gitStatus(repository, env) {
  return run("git", ["-C", repository, "status", "--porcelain=v1"], { env })
    .stdout.split(/\r?\n/)
    .filter((line) => line.length > 0);
}

export function materializeRepositories(manifest, manifestPath, workspace, env) {
  const manifestRoot = dirname(resolve(manifestPath));
  const source = join(workspace, "source");
  mkdirSync(source, { recursive: true });
  run("git", ["init", "--initial-branch=main", source], { env });
  git(source, ["config", "user.name", "JScout Docs Eval"], env);
  git(source, ["config", "user.email", "docs-eval@invalid.example"], env);
  for (const commit of manifest.commits) {
    for (const target of commit.delete ?? []) {
      validateRepositoryTarget(target, `${commit.id} delete target`);
      rmSync(join(source, target), { force: true, recursive: true });
    }
    applyOverlay(source, commit.overlay, manifestRoot);
    git(source, ["add", "--all"], env);
    run("git", ["-C", source, "commit", "--message", commit.message], {
      env: { ...env, GIT_AUTHOR_DATE: commit.date, GIT_COMMITTER_DATE: commit.date },
    });
  }

  const repositories = new Map();
  for (const variant of manifest.variants) {
    const destination = join(workspace, "repositories", variant.id);
    mkdirSync(dirname(destination), { recursive: true });
    if (variant.kind === "shallow") {
      run("git", ["clone", "--quiet", "--depth", String(variant.depth), pathToFileURL(source).href, destination], { env });
    } else {
      run("git", ["clone", "--quiet", "--no-local", source, destination], { env });
    }
    if (variant.kind === "non_git") rmSync(join(destination, ".git"), { recursive: true });
    applyOverlay(destination, variant.overlay, manifestRoot);
    if (variant.kind === "staged") git(destination, ["add", "--all"], env);
    repositories.set(variant.id, {
      id: variant.id,
      kind: variant.kind,
      path: destination,
      head: variant.kind === "non_git" ? null : git(destination, ["rev-parse", "HEAD"], env),
      shallow: variant.kind === "non_git" ? null : git(destination, ["rev-parse", "--is-shallow-repository"], env) === "true",
      status: variant.kind === "non_git" ? null : gitStatus(destination, env),
    });
  }
  return repositories;
}

function authoredHeadings(path) {
  const headings = [];
  for (const line of readFileSync(path, "utf8").split(/\r?\n/)) {
    const match = /^(#{1,6})[ \t]+(.+?)(?:[ \t]+#+)?[ \t]*$/.exec(line);
    if (match) headings.push({ level: match[1].length, text: match[2] });
  }
  return headings;
}

export function validateQrelsAgainstRepositories(manifest, repositories) {
  for (const query of manifest.queries) {
    const qrels = [...query.current, ...(query.older_conflicts ?? []), ...(query.recent_irrelevant ?? [])];
    for (const variantId of query.variants) {
      const repository = repositories.get(variantId);
      if (!repository) throw new Error(`${query.id}: materialized variant is missing: ${variantId}`);
      for (const qrel of qrels) {
        const sourcePath = join(repository.path, qrel.path);
        if (!existsSync(sourcePath) || !statSync(sourcePath).isFile()) {
          throw new Error(`${query.id}/${variantId}/${qrel.answer_id}: qrel path is missing: ${qrel.path}`);
        }
        const headings = authoredHeadings(sourcePath);
        let cursor = 0;
        for (const [index, expected] of qrel.heading.split(" > ").entries()) {
          const found = headings.findIndex(
            (heading, headingIndex) => headingIndex >= cursor && heading.level === index + 1 && heading.text === expected,
          );
          if (found < 0) {
            throw new Error(
              `${query.id}/${variantId}/${qrel.answer_id}: authored heading is missing: ${qrel.heading}`,
            );
          }
          cursor = found + 1;
        }
      }
    }
  }
}

function qrelMatches(hit, qrel) {
  return hit.path === qrel.path && hit.heading === qrel.heading;
}

function rankedAnswerIds(hits, query) {
  const qrels = [...query.current, ...(query.older_conflicts ?? []), ...(query.recent_irrelevant ?? [])];
  return hits.map((hit) => qrels.filter((qrel) => qrelMatches(hit, qrel)).map((qrel) => qrel.answer_id));
}

function firstRank(hits, qrels) {
  for (let index = 0; index < hits.length; index += 1) {
    if (qrels.some((qrel) => qrelMatches(hits[index], qrel))) return index + 1;
  }
  return null;
}

function recallAt(hits, qrels, cutoff) {
  const found = new Set();
  for (const hit of hits.slice(0, cutoff)) {
    for (const qrel of qrels) {
      if (qrelMatches(hit, qrel)) found.add(qrel.answer_id);
    }
  }
  return found.size / new Set(qrels.map((qrel) => qrel.answer_id)).size;
}

export function scoreRun(query, hits) {
  const currentRank = firstRank(hits, query.current);
  const olderRank = query.older_conflicts ? firstRank(hits, query.older_conflicts) : null;
  const irrelevantRank = query.recent_irrelevant ? firstRank(hits, query.recent_irrelevant) : null;
  const recall = Object.fromEntries(CUTOFFS.map((cutoff) => [cutoff, recallAt(hits, query.current, cutoff)]));
  return {
    current_rank: currentRank,
    reciprocal_rank: currentRank === null ? 0 : 1 / currentRank,
    current_recall: recall,
    older_conflict_rank: olderRank,
    older_conflict_visible_at_5: olderRank !== null && olderRank <= 5,
    older_conflict_visible_at_10: olderRank !== null && olderRank <= 10,
    current_ahead_of_older: currentRank !== null && olderRank !== null ? currentRank < olderRank : null,
    recent_irrelevant_rank: irrelevantRank,
    recent_irrelevant_inversion:
      currentRank !== null && irrelevantRank !== null ? irrelevantRank < currentRank : null,
  };
}

function mean(values) {
  return values.length === 0 ? null : values.reduce((sum, value) => sum + value, 0) / values.length;
}

export function summarizeProfile(runs) {
  const conflicts = runs.filter((run) => run.query.category === "conflict");
  const evergreen = runs.filter((run) => run.query.category === "evergreen");
  return {
    queries: runs.length,
    current_answer_recall: Object.fromEntries(
      CUTOFFS.map((cutoff) => [cutoff, mean(runs.map((run) => run.score.current_recall[cutoff]))]),
    ),
    mean_reciprocal_rank: mean(runs.map((run) => run.score.reciprocal_rank)),
    conflict_current_ahead: {
      numerator: conflicts.filter((run) => run.score.current_ahead_of_older === true).length,
      denominator: conflicts.filter((run) => run.score.current_ahead_of_older !== null).length,
    },
    older_conflict_visibility: {
      5: mean(conflicts.map((run) => Number(run.score.older_conflict_visible_at_5))),
      10: mean(conflicts.map((run) => Number(run.score.older_conflict_visible_at_10))),
    },
    evergreen_recent_irrelevant_inversions: evergreen.filter(
      (run) => run.score.recent_irrelevant_inversion === true,
    ).length,
  };
}

export function hasConflictTreatmentOpportunity(runs) {
  return runs.some((run) => {
    if (run.query.category !== "conflict") return false;
    const { current_rank: currentRank, older_conflict_rank: olderRank } = run.score;
    return currentRank !== null && olderRank !== null
      && olderRank < currentRank && currentRank - olderRank <= 3;
  });
}

function hitIdentity(hit) {
  return `${hit.path}\0${hit.heading}\0${hit.source_bytes[0]}\0${hit.source_bytes[1]}\0${hit.file_hash}`;
}

export function compareProfiles(runs, baselineProfile, candidateProfile) {
  const baseline = new Map(
    runs.filter((run) => run.profile === baselineProfile).map((run) => [`${run.variant}\0${run.query.id}`, run]),
  );
  const pairs = [];
  for (const candidate of runs.filter((run) => run.profile === candidateProfile)) {
    const key = `${candidate.variant}\0${candidate.query.id}`;
    const original = baseline.get(key);
    if (!original) throw new Error(`missing ${baselineProfile} pair for ${candidate.variant}/${candidate.query.id}`);
    const left = original.hits.map(hitIdentity);
    const right = candidate.hits.map(hitIdentity);
    pairs.push({
      variant: candidate.variant,
      query_id: candidate.query.id,
      exact_order: JSON.stringify(left) === JSON.stringify(right),
      current_recall_delta: Object.fromEntries(
        CUTOFFS.map((cutoff) => [cutoff, candidate.score.current_recall[cutoff] - original.score.current_recall[cutoff]]),
      ),
      current_rank_delta:
        original.score.current_rank === null || candidate.score.current_rank === null
          ? null
          : candidate.score.current_rank - original.score.current_rank,
    });
  }
  return {
    baseline: baselineProfile,
    candidate: candidateProfile,
    pairs: pairs.length,
    changed_orders: pairs.filter((pair) => !pair.exact_order).length,
    exact_order_parity: pairs.every((pair) => pair.exact_order),
    current_answer_recall_delta: Object.fromEntries(
      CUTOFFS.map((cutoff) => [cutoff, mean(pairs.map((pair) => pair.current_recall_delta[cutoff]))]),
    ),
    details: pairs,
  };
}

function normalizeHit(hit, query) {
  const normalized = {
    rank: hit.rank,
    path: hit.path,
    heading: hit.breadcrumb,
    lines: [hit.start_line, hit.end_line],
    source_bytes: [hit.source_start, hit.source_end],
    file_hash: hit.file_hash,
    content_sha256: sha256(hit.content),
    lexical_score: hit.lexical_score,
    vector_score: hit.vector_score,
    source_state: hit.source_state,
  };
  normalized.answer_ids = rankedAnswerIds([normalized], query)[0];
  return normalized;
}

function portableEndpoint(value) {
  if (value === null || value === undefined) return null;
  const endpoint = new URL(value);
  endpoint.username = "";
  endpoint.password = "";
  endpoint.search = "";
  endpoint.hash = "";
  return endpoint.toString();
}

function configShow(binary, config, root, env) {
  const value = JSON.parse(output(binary, ["--config", config, "config", "show", root, "--json"], { env }));
  const { embedding, inference, reranker } = value.effective;
  return {
    fingerprint: value.fingerprint,
    docs: value.effective.docs,
    embedding: {
      provider: embedding.provider,
      model: embedding.model,
      revision: embedding.revision,
      url: portableEndpoint(embedding.url),
      query_prefix: embedding.query_prefix,
      batch: embedding.batch,
      origins: embedding.origins,
    },
    inference: {
      url: portableEndpoint(inference.url),
      host: inference.host,
      port: inference.port,
      allow_remote: inference.allow_remote,
      batch_size: inference.batch_size,
      max_length: inference.max_length,
    },
    reranker: {
      url: portableEndpoint(reranker.url),
      model: reranker.model,
      revision: reranker.revision,
      top: reranker.top,
      max_chars: reranker.max_chars,
    },
    sources: value.sources,
  };
}

export function validatePhase2ProviderConfiguration(configuration) {
  const expected = [
    [configuration.embedding.provider, "local", "embedding provider"],
    [configuration.embedding.model, EMBEDDING_MODEL, "embedding model"],
    [configuration.embedding.revision, EMBEDDING_REVISION, "embedding revision"],
    [configuration.reranker.model, RERANKER_MODEL, "reranker model"],
    [configuration.reranker.revision, RERANKER_REVISION, "reranker revision"],
  ];
  for (const [actual, required, label] of expected) {
    if (actual !== required) throw new Error(`phase2-baseline ${label} must be ${required}; got ${actual}`);
  }
  for (const [label, value] of [
    ["inference", configuration.inference.url],
    ["reranker", configuration.reranker.url],
  ]) {
    const hostname = new URL(value).hostname;
    if (!["127.0.0.1", "localhost", "::1", "[::1]"].includes(hostname)) {
      throw new Error(`phase2-baseline ${label} endpoint must be loopback; got ${hostname}`);
    }
  }
}

function normalizeServiceConfiguration(value) {
  return {
    available: value.available,
    provider: value.provider,
    device: value.device,
    embedding: {
      model: value.embedding?.model,
      dimensions: value.embedding?.dimensions,
      revision: value.embedding?.revision,
      configuration: value.embedding?.configuration,
    },
    reranker: {
      model: value.reranker?.model,
      revision: value.reranker?.revision,
      configuration: value.reranker?.configuration,
    },
  };
}

export function validatePhase2ServiceConfiguration(configuration) {
  const expected = [
    [configuration.available, true, "availability"],
    [configuration.provider, "local", "provider"],
    [configuration.embedding.model, EMBEDDING_MODEL, "embedding model"],
    [configuration.embedding.revision, EMBEDDING_REVISION, "embedding revision"],
    [configuration.reranker.model, RERANKER_MODEL, "reranker model"],
    [configuration.reranker.revision, RERANKER_REVISION, "reranker revision"],
  ];
  for (const [actual, required, label] of expected) {
    if (actual !== required) throw new Error(`phase2-baseline service ${label} must be ${required}; got ${actual}`);
  }
}

async function queryServiceConfiguration(endpointValue) {
  const endpoint = new URL(endpointValue);
  endpoint.pathname = "/configuration";
  endpoint.search = "";
  endpoint.hash = "";
  const response = await fetch(endpoint, { signal: AbortSignal.timeout(10_000) });
  if (!response.ok) throw new Error(`inference configuration returned HTTP ${response.status}`);
  const configuration = normalizeServiceConfiguration(await response.json());
  validatePhase2ServiceConfiguration(configuration);
  return configuration;
}

function normalizeStatus(status) {
  const { canonical_root: _canonicalRoot, ...portable } = status;
  return portable;
}

function runJscoutJson(binary, config, args, env) {
  const result = run(binary, ["--config", config, ...args], { env });
  try {
    return {
      value: JSON.parse(result.stdout),
      elapsedMs: result.elapsedMs,
      stderr: result.stderr,
      stdoutSha256: sha256(result.stdout),
    };
  } catch (error) {
    throw new Error(`invalid JSON from jscout ${args.join(" ")}: ${error.message}\n${result.stdout}`);
  }
}

function expectedDiagnostics(profile) {
  return {
    lexical: { vector: "disabled", reranker: "disabled" },
    fallback: { vector: "not_configured", reranker: "disabled" },
    hybrid: { vector: "active", reranker: "disabled" },
    "hybrid-rerank": { vector: "active", reranker: "active" },
  }[profile];
}

function searchArguments(profile) {
  const flags = {
    lexical: ["--lexical-only"],
    fallback: ["--no-rerank"],
    hybrid: ["--vector", "--no-rerank"],
    "hybrid-rerank": ["--vector", "--rerank"],
  }[profile];
  return flags;
}

export function docsSearchArguments(profile, root, query, database) {
  return [
    "docs",
    "search",
    root,
    query,
    "--database",
    database,
    "--limit",
    String(MAX_K),
    "--response-bytes",
    String(RESPONSE_BYTES),
    "--debug-json",
    ...searchArguments(profile),
  ];
}

function writeExclusive(path, value, force) {
  mkdirSync(dirname(path), { recursive: true });
  if (existsSync(path) && !force) throw new Error(`output already exists: ${path}`);
  writeFileSync(path, `${JSON.stringify(value, null, 2)}\n`, { flag: force ? "w" : "wx" });
}

function prepareWorkspace(options) {
  if (!options.workdir) return mkdtempSync(join(tmpdir(), "jscout-docs-retrieval-eval-"));
  if (existsSync(options.workdir) && readdirSync(options.workdir).length > 0) {
    throw new Error(`--workdir must be empty or absent: ${options.workdir}`);
  }
  mkdirSync(options.workdir, { recursive: true });
  return options.workdir;
}

function pathIsContained(parent, candidate) {
  const path = relative(parent, candidate);
  return path === "" || (!isAbsolute(path) && path.split(/[\\/]/)[0] !== "..");
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.help) {
    process.stdout.write(usage);
    return;
  }
  for (const [label, path] of [["binary", options.binary], ["manifest", options.manifest]]) {
    if (!existsSync(path)) throw new Error(`${label} does not exist: ${path}`);
  }
  if (options.providerConfig && !existsSync(options.providerConfig)) {
    throw new Error(`provider config does not exist: ${options.providerConfig}`);
  }
  const manifest = validateManifest(readJson(options.manifest), options.manifest);
  const fixtureRoot = dirname(options.manifest);
  const workspace = prepareWorkspace(options);
  if (pathIsContained(workspace, options.output)) {
    throw new Error("--output must be outside --workdir because successful runs remove the workspace");
  }
  const baseEnv = childEnvironment(workspace);
  const providerEnv = childEnvironment(workspace, {}, options.passEnv);
  const baselineConfig = join(workspace, "baseline.toml");
  writeFileSync(
    baselineConfig,
    "version = 1\n\n[docs.search]\nvector = true\nrerank = false\nlimit = 10\nresponse_bytes = 1048576\n\n[llm]\nauth_file = \".jscout-eval-unused-auth.json\"\n",
  );
  let completed = false;
  try {
    const repositories = materializeRepositories(manifest, options.manifest, workspace, baseEnv);
    validateQrelsAgainstRepositories(manifest, repositories);
    const version = output(options.binary, ["--version"], { env: baseEnv });
    const gitVersion = output("git", ["--version"], { env: baseEnv });
    const sourceCommit = output("git", ["-C", projectRoot, "rev-parse", "HEAD"], { env: baseEnv });
    const sourceStatus = gitStatus(projectRoot, baseEnv);
    if (options.runKind === "phase2-baseline" && sourceStatus.length > 0) {
      throw new Error("phase2-baseline requires a clean source worktree");
    }
    const configurations = {
      baseline: configShow(options.binary, baselineConfig, repositories.values().next().value.path, baseEnv),
      provider: options.providerConfig
        ? configShow(options.binary, options.providerConfig, repositories.values().next().value.path, providerEnv)
        : null,
    };
    if (options.runKind === "phase2-baseline") {
      validatePhase2ProviderConfiguration(configurations.provider);
    }
    const serviceConfigurationBefore = options.runKind === "phase2-baseline"
      ? await queryServiceConfiguration(configurations.provider.inference.url)
      : null;
    const environments = [];
    const runs = [];
    for (const repository of repositories.values()) {
      const database = join(workspace, "databases", `${repository.id}.db`);
      mkdirSync(dirname(database), { recursive: true });
      const indexed = run(
        options.binary,
        ["--config", baselineConfig, "index", repository.path, "--database", database],
        { env: baseEnv },
      );
      const statusBeforeEmbed = runJscoutJson(
        options.binary,
        baselineConfig,
        ["docs", "status", repository.path, "--database", database, "--json"],
        baseEnv,
      ).value;
      let embed = null;
      let status = statusBeforeEmbed;
      if (options.profiles.some((profile) => profile.startsWith("hybrid"))) {
        embed = runJscoutJson(
          options.binary,
          options.providerConfig,
          ["docs", "embed", repository.path, "--database", database, "--json"],
          providerEnv,
        );
        if (!embed.value.generation_published) {
          throw new Error(`${repository.id}: documentation vector generation was not published`);
        }
        if (embed.value.snapshot !== statusBeforeEmbed.snapshot) {
          throw new Error(`${repository.id}: docs embed snapshot differs from indexed status`);
        }
        status = runJscoutJson(
          options.binary,
          baselineConfig,
          ["docs", "status", repository.path, "--database", database, "--json"],
          baseEnv,
        ).value;
        if (status.snapshot !== statusBeforeEmbed.snapshot || status.ready_vector_generation_count < 1) {
          throw new Error(`${repository.id}: post-embed docs status is not ready on the indexed snapshot`);
        }
      }
      environments.push({
        id: repository.id,
        kind: repository.kind,
        head: repository.head,
        shallow: repository.shallow,
        git_status: repository.status,
        index_elapsed_ms: indexed.elapsedMs,
        docs_status_before_embed: embed ? normalizeStatus(statusBeforeEmbed) : null,
        docs_status: normalizeStatus(status),
        docs_embed: embed?.value ?? null,
        docs_embed_elapsed_ms: embed?.elapsedMs ?? null,
      });
      for (const query of manifest.queries.filter((value) => value.variants.includes(repository.id))) {
        for (const profile of options.profiles) {
          const config = profile.startsWith("hybrid") ? options.providerConfig : baselineConfig;
          const env = profile.startsWith("hybrid") ? providerEnv : baseEnv;
          const searchArgs = docsSearchArguments(profile, repository.path, query.query, database);
          const response = runJscoutJson(options.binary, config, searchArgs, env);
          const repeated = runJscoutJson(options.binary, config, searchArgs, env);
          const expected = expectedDiagnostics(profile);
          if (response.value.truncated) throw new Error(`${repository.id}/${query.id}/${profile}: response truncated`);
          if (repeated.value.truncated) {
            throw new Error(`${repository.id}/${query.id}/${profile}: repeated response truncated`);
          }
          if (response.value.diagnostics.vector_status !== expected.vector) {
            throw new Error(
              `${repository.id}/${query.id}/${profile}: vector=${response.value.diagnostics.vector_status}, expected ${expected.vector}`,
            );
          }
          if (response.value.diagnostics.reranker_status !== expected.reranker) {
            throw new Error(
              `${repository.id}/${query.id}/${profile}: reranker=${response.value.diagnostics.reranker_status}, expected ${expected.reranker}`,
            );
          }
          if (repeated.value.diagnostics.vector_status !== expected.vector
              || repeated.value.diagnostics.reranker_status !== expected.reranker) {
            throw new Error(`${repository.id}/${query.id}/${profile}: repeated retrieval stage status changed`);
          }
          if (response.value.snapshot !== status.snapshot || repeated.value.snapshot !== status.snapshot) {
            throw new Error(`${repository.id}/${query.id}/${profile}: search snapshot differs from indexed status`);
          }
          if (profile.startsWith("hybrid")
              && response.value.diagnostics.vector_profile_id !== embed.value.profile_id) {
            throw new Error(`${repository.id}/${query.id}/${profile}: vector profile differs from docs embed`);
          }
          if (profile.startsWith("hybrid")
              && repeated.value.diagnostics.vector_profile_id !== embed.value.profile_id) {
            throw new Error(`${repository.id}/${query.id}/${profile}: repeated vector profile differs from docs embed`);
          }
          const hits = response.value.hits.map((hit) => normalizeHit(hit, query));
          const repeatedHits = repeated.value.hits.map((hit) => normalizeHit(hit, query));
          const stableOrder = JSON.stringify(hits.map(hitIdentity)) === JSON.stringify(repeatedHits.map(hitIdentity));
          if (!stableOrder) throw new Error(`${repository.id}/${query.id}/${profile}: repeated rank order changed`);
          if (hits.some((hit) => hit.source_state !== "current")) {
            throw new Error(`${repository.id}/${query.id}/${profile}: source resolution was not current`);
          }
          if (repeatedHits.some((hit) => hit.source_state !== "current")) {
            throw new Error(`${repository.id}/${query.id}/${profile}: repeated source resolution was not current`);
          }
          runs.push({
            variant: repository.id,
            profile,
            query: { id: query.id, category: query.category },
            elapsed_ms: response.elapsedMs,
            snapshot: response.value.snapshot,
            diagnostics: response.value.diagnostics,
            stdout_sha256: response.stdoutSha256,
            repeated_stdout_sha256: repeated.stdoutSha256,
            repeated_exact_order: stableOrder,
            hits,
            score: scoreRun(query, hits),
          });
        }
      }
    }
    const scoredRuns = runs.map((run) => ({
      ...run,
      query: manifest.queries.find((query) => query.id === run.query.id),
    }));
    const summaries = Object.fromEntries(
      options.profiles.map((profile) => [
        profile,
        summarizeProfile(scoredRuns.filter((run) => run.profile === profile)),
      ]),
    );
    const summariesByVariant = Object.fromEntries(
      [...repositories.keys()].map((variant) => [
        variant,
        Object.fromEntries(options.profiles.map((profile) => [
          profile,
          summarizeProfile(scoredRuns.filter((run) => run.variant === variant && run.profile === profile)),
        ])),
      ]),
    );
    const conflictTreatmentOpportunity = Object.fromEntries(
      options.profiles.map((profile) => [
        profile,
        hasConflictTreatmentOpportunity(scoredRuns.filter((run) => run.profile === profile)),
      ]),
    );
    if (options.runKind === "phase2-baseline"
        && Object.values(conflictTreatmentOpportunity).some((value) => !value)) {
      throw new Error("phase2-baseline has no obsolete-first conflict within the tested movement bounds");
    }
    const comparisons = [];
    for (const profile of options.profiles.filter((profile) => profile !== "lexical")) {
      comparisons.push(compareProfiles(
        scoredRuns,
        "lexical",
        profile,
      ));
    }
    const fallback = comparisons.find((comparison) => comparison.candidate === "fallback");
    if (fallback && !fallback.exact_order_parity) {
      throw new Error("BM25 fallback order differs from --lexical-only");
    }
    const serviceConfigurationAfter = options.runKind === "phase2-baseline"
      ? await queryServiceConfiguration(configurations.provider.inference.url)
      : null;
    if (JSON.stringify(serviceConfigurationBefore) !== JSON.stringify(serviceConfigurationAfter)) {
      throw new Error("inference service configuration changed during phase2-baseline");
    }
    const result = {
      schema: "jscout.docs-retrieval-eval.v1",
      schema_version: 1,
      run_kind: options.runKind,
      suite: manifest.suite,
      generated_at: new Date().toISOString(),
      inputs: {
        fixture_sha256: directoryDigest(fixtureRoot),
        manifest_sha256: sha256File(options.manifest),
        harness_sha256: sha256File(scriptPath),
        binary_sha256: sha256File(options.binary),
        binary_version: version,
        source_commit: sourceCommit,
        source_status: sourceStatus,
        git_version: gitVersion,
        provider_config_sha256: options.providerConfig ? sha256File(options.providerConfig) : null,
        profiles: options.profiles,
        max_k: MAX_K,
        response_bytes: RESPONSE_BYTES,
      },
      treatments: {
        lexical: ["--lexical-only"],
        fallback: ["--no-rerank"],
        hybrid: ["--vector", "--no-rerank"],
        "hybrid-rerank": ["--vector", "--rerank"],
      },
      configurations,
      service_configuration: serviceConfigurationBefore === null ? null : {
        before: serviceConfigurationBefore,
        after: serviceConfigurationAfter,
      },
      environments,
      summaries,
      summaries_by_variant: summariesByVariant,
      comparisons,
      validity: {
        bm25_fallback_exact_order: fallback?.exact_order_parity ?? false,
        repeated_orders_stable: runs.every((run) => run.repeated_exact_order),
        source_tree_clean: sourceStatus.length === 0,
        phase2_complete: PHASE2_PROFILES.every((profile) => options.profiles.includes(profile)),
        conflict_treatment_opportunity: conflictTreatmentOpportunity,
        required_profiles_present: (options.runKind === "phase2-baseline" ? PHASE2_PROFILES : PROVIDER_FREE_PROFILES)
          .every((profile) => options.profiles.includes(profile)),
        hybrid_measured: options.profiles.includes("hybrid"),
        hybrid_rerank_measured: options.profiles.includes("hybrid-rerank"),
      },
      decision: options.runKind === "phase2-baseline"
        ? "phase2-baseline-recorded"
        : "provider-free-check-recorded",
      runs,
    };
    writeExclusive(options.output, result, options.force);
    completed = true;
    process.stdout.write(`${JSON.stringify(result.summaries, null, 2)}\n`);
  } finally {
    if (!options.keepWorkdir && completed) rmSync(workspace, { recursive: true });
    else if (options.keepWorkdir || !completed) process.stderr.write(`evaluation workdir: ${workspace}\n`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) await main();
