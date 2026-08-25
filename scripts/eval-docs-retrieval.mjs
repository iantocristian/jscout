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
const PHASE3_PROFILES = ["lexical", "hybrid", "hybrid-rerank"];
const PROVIDER_FREE_PROFILES = ["lexical", "fallback"];
const FRESHNESS_TREATMENTS = [
  { id: "disabled", enabled: false, bound: 1 },
  { id: "bound-1", enabled: true, bound: 1 },
  { id: "bound-2", enabled: true, bound: 2 },
  { id: "bound-3", enabled: true, bound: 3 },
];
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
  --run-kind NAME           provider-free-check (default), phase2-baseline,
                            or phase3-candidate
  --profiles LIST           comma-separated lexical,fallback,hybrid,hybrid-rerank
                            (default: lexical,fallback; hybrid arms require --provider-config)
  --provider-config PATH    explicit config used only for docs embed/vector/rerank arms
  --phase2-report PATH      frozen Phase 2 JSON report (required for phase3-candidate)
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
      "--phase2-report": "phase2Report",
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
  options.phase2Report = options.phase2Report ? resolve(options.phase2Report) : null;
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
  if (!new Set(["provider-free-check", "phase2-baseline", "phase3-candidate"]).has(options.runKind)) {
    throw new Error(`unknown --run-kind: ${options.runKind}`);
  }
  const requiredProfiles = options.runKind === "phase2-baseline"
    ? PHASE2_PROFILES
    : options.runKind === "phase3-candidate"
      ? PHASE3_PROFILES
      : PROVIDER_FREE_PROFILES;
  if (options.profiles.length !== requiredProfiles.length
      || requiredProfiles.some((profile) => !options.profiles.includes(profile))) {
    throw new Error(`${options.runKind} requires exactly: ${requiredProfiles.join(",")}`);
  }
  if (options.runKind === "phase3-candidate" && !options.phase2Report) {
    throw new Error("phase3-candidate requires --phase2-report");
  }
  if (options.runKind !== "phase3-candidate" && options.phase2Report) {
    throw new Error("--phase2-report is only valid with --run-kind phase3-candidate");
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

function parseExplicitRfc3339(value, label) {
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})(?:\.(\d+))?(Z|([+-])(\d{2}):(\d{2}))$/.exec(value);
  if (!match) throw new Error(`${label} must be RFC3339 with an explicit timezone`);
  const [, year, month, day, hour, minute, second, , zone, , offsetHour, offsetMinute] = match;
  const numeric = [year, month, day, hour, minute, second, offsetHour ?? "0", offsetMinute ?? "0"]
    .map(Number);
  const [yearValue, monthValue, dayValue, hourValue, minuteValue, secondValue, offsetHourValue, offsetMinuteValue]
    = numeric;
  const daysInMonth = new Date(Date.UTC(yearValue, monthValue, 0)).getUTCDate();
  if (monthValue < 1 || monthValue > 12 || dayValue < 1 || dayValue > daysInMonth
      || hourValue > 23 || minuteValue > 59 || secondValue > 59
      || (zone !== "Z" && (offsetHourValue > 23 || offsetMinuteValue > 59))) {
    throw new Error(`${label} is not a valid RFC3339 timestamp`);
  }
  const timestamp = Date.parse(value);
  if (!Number.isFinite(timestamp)) throw new Error(`${label} is not a valid RFC3339 timestamp`);
  return timestamp;
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
    const commitTime = parseExplicitRfc3339(commit.date, `commits[${index}].date`);
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

function runKey(run) {
  return `${run.variant}\0${run.query.id}`;
}

function identities(hits) {
  return hits.map(hitIdentity);
}

function identityDeltaAt(baselineHits, candidateHits, cutoff) {
  const baseline = new Set(identities(baselineHits.slice(0, cutoff)));
  const candidate = new Set(identities(candidateHits.slice(0, cutoff)));
  return {
    entrants: [...candidate].filter((identity) => !baseline.has(identity)).toSorted(),
    exits: [...baseline].filter((identity) => !candidate.has(identity)).toSorted(),
  };
}

function gitObservedCrossings(hits) {
  const gitFamily = new Set(["git", "working_tree"]);
  let crossings = 0;
  for (let left = 0; left < hits.length; left += 1) {
    for (let right = left + 1; right < hits.length; right += 1) {
      const first = hits[left];
      const second = hits[right];
      if (first.base_rank < second.base_rank) continue;
      if ((gitFamily.has(first.freshness_basis) && second.freshness_basis === "observed")
          || (first.freshness_basis === "observed" && gitFamily.has(second.freshness_basis))) {
        crossings += 1;
      }
    }
  }
  return crossings;
}

function movementSummary(runs) {
  const movements = runs.flatMap((run) => run.hits.map((hit) => hit.movement));
  const histogram = {};
  for (const movement of movements) histogram[String(movement)] = (histogram[String(movement)] ?? 0) + 1;
  return {
    maximum_absolute_movement: movements.length === 0 ? 0 : Math.max(...movements.map(Math.abs)),
    movement_histogram: Object.fromEntries(
      Object.entries(histogram).toSorted(([left], [right]) => Number(left) - Number(right)),
    ),
  };
}

export function compareFreshnessTreatments(runs, profile, treatment) {
  const baselineRuns = runs.filter((run) => run.profile === profile && run.treatment === "disabled");
  const candidateRuns = runs.filter((run) => run.profile === profile && run.treatment === treatment);
  const baseline = new Map(baselineRuns.map((run) => [runKey(run), run]));
  if (baseline.size !== baselineRuns.length || candidateRuns.length !== baselineRuns.length) {
    throw new Error(`${profile}/${treatment}: freshness treatment does not pair one-to-one with disabled runs`);
  }
  const details = candidateRuns.map((candidate) => {
    const original = baseline.get(runKey(candidate));
    if (!original) throw new Error(`missing disabled freshness pair for ${candidate.variant}/${candidate.query.id}`);
    const originalIdentities = identities(original.hits);
    const candidateIdentities = identities(candidate.hits);
    const topK = Object.fromEntries(CUTOFFS.map((cutoff) => [
      cutoff,
      identityDeltaAt(original.hits, candidate.hits, cutoff),
    ]));
    const candidateBaseMatches = candidate.hits.every((hit) => (
      hit.base_rank > original.hits.length
      || hitIdentity(hit) === hitIdentity(original.hits[hit.base_rank - 1])
    ));
    return {
      variant: candidate.variant,
      query_id: candidate.query.id,
      exact_order: JSON.stringify(originalIdentities) === JSON.stringify(candidateIdentities),
      top_k: topK,
      candidate_base_matches_disabled: candidateBaseMatches,
      current_recall_delta: Object.fromEntries(
        CUTOFFS.map((cutoff) => [cutoff, candidate.score.current_recall[cutoff] - original.score.current_recall[cutoff]]),
      ),
      current_rank_delta:
        original.score.current_rank === null || candidate.score.current_rank === null
          ? null
          : candidate.score.current_rank - original.score.current_rank,
      conflict_changed_to_current_first:
        original.score.current_ahead_of_older === false && candidate.score.current_ahead_of_older === true,
      conflict_reversed_to_obsolete_first:
        original.score.current_ahead_of_older === true && candidate.score.current_ahead_of_older === false,
      comparable_conflict_pair: comparableConflictPair(candidate),
    };
  });
  const movement = movementSummary(candidateRuns);
  const candidateHits = candidateRuns.flatMap((run) => run.hits);
  const configuredBound = Number(treatment.replace("bound-", ""));
  return {
    profile,
    baseline_treatment: "disabled",
    candidate_treatment: treatment,
    pairs: details.length,
    changed_orders: details.filter((detail) => !detail.exact_order).length,
    top_k_entrants: Object.fromEntries(CUTOFFS.map((cutoff) => [
      cutoff,
      details.reduce((count, detail) => count + detail.top_k[cutoff].entrants.length, 0),
    ])),
    top_k_exits: Object.fromEntries(CUTOFFS.map((cutoff) => [
      cutoff,
      details.reduce((count, detail) => count + detail.top_k[cutoff].exits.length, 0),
    ])),
    current_answer_recall_delta: Object.fromEntries(
      CUTOFFS.map((cutoff) => [cutoff, mean(details.map((detail) => detail.current_recall_delta[cutoff]))]),
    ),
    ...movement,
    validity: {
      movement_within_bound: movement.maximum_absolute_movement <= configuredBound,
      movement_values_consistent: candidateHits.every(
        (hit) => hit.movement === hit.base_rank - hit.rank,
      ),
      unknown_basis_stationary: candidateHits.every(
        (hit) => hit.freshness_basis !== "unknown" || hit.movement === 0,
      ),
      git_observed_do_not_cross: candidateRuns.every((run) => gitObservedCrossings(run.hits) === 0),
      candidate_bases_match_disabled: details.every((detail) => detail.candidate_base_matches_disabled),
    },
    details,
  };
}

export function comparePhase2RankedIdentities(runs, phase2Report) {
  const phase2 = new Map(phase2Report.runs.map((run) => [`${run.profile}\0${runKey(run)}`, run]));
  const disabled = runs.filter((run) => run.treatment === "disabled");
  const details = disabled.map((candidate) => {
    const key = `${candidate.profile}\0${runKey(candidate)}`;
    const baseline = phase2.get(key);
    if (!baseline) throw new Error(`Phase 2 report has no run for ${candidate.profile}/${candidate.variant}/${candidate.query.id}`);
    const exact = JSON.stringify(identities(candidate.hits)) === JSON.stringify(identities(baseline.hits));
    return {
      profile: candidate.profile,
      variant: candidate.variant,
      query_id: candidate.query.id,
      exact_ranked_identities: exact,
    };
  });
  return {
    pairs: details.length,
    exact_ranked_identities: details.every((detail) => detail.exact_ranked_identities),
    by_profile: Object.fromEntries(PHASE3_PROFILES.map((profile) => {
      const profileDetails = details.filter((detail) => detail.profile === profile);
      return [profile, {
        pairs: profileDetails.length,
        exact_ranked_identities: profileDetails.every((detail) => detail.exact_ranked_identities),
      }];
    })),
    details,
  };
}

function comparableConflictPair(run) {
  if (run.query.category !== "conflict") return false;
  const current = run.hits.find((hit) => run.query.current.some((qrel) => qrelMatches(hit, qrel)));
  const older = run.hits.find((hit) => run.query.older_conflicts?.some((qrel) => qrelMatches(hit, qrel)));
  if (!current || !older) return false;
  const gitFamily = new Set(["git", "working_tree"]);
  return (gitFamily.has(current.freshness_basis) && gitFamily.has(older.freshness_basis))
    || (current.freshness_basis === "observed" && older.freshness_basis === "observed");
}

function pairedTreatmentRuns(runs, profile, treatment) {
  const baseline = new Map(
    runs.filter((run) => run.profile === profile && run.treatment === "disabled")
      .map((run) => [runKey(run), run]),
  );
  return runs.filter((run) => run.profile === profile && run.treatment === treatment).map((candidate) => {
    const original = baseline.get(runKey(candidate));
    if (!original) throw new Error(`missing disabled arm for ${profile}/${runKey(candidate)}`);
    return { original, candidate };
  });
}

export function selectPhase3Default(runs, comparisons, phase2Parity) {
  const candidates = [1, 2, 3].map((bound) => {
    const treatment = `bound-${bound}`;
    const profileComparisons = PHASE3_PROFILES.map((profile) => {
      const comparison = comparisons.find(
        (value) => value.profile === profile && value.candidate_treatment === treatment,
      );
      if (!comparison) throw new Error(`missing freshness comparison for ${profile}/${treatment}`);
      const pairs = pairedTreatmentRuns(runs, profile, treatment);
      const baselineSummary = summarizeProfile(pairs.map(({ original }) => original));
      const candidateSummary = summarizeProfile(pairs.map(({ candidate }) => candidate));
      const comparableConflicts = pairs.filter(({ candidate }) => comparableConflictPair(candidate));
      const corrected = comparableConflicts.filter(
        ({ original, candidate }) => original.score.current_ahead_of_older === false
          && candidate.score.current_ahead_of_older === true,
      ).length;
      const reversed = comparableConflicts.filter(
        ({ original, candidate }) => original.score.current_ahead_of_older === true
          && candidate.score.current_ahead_of_older === false,
      ).length;
      const evergreenTopFiveRetained = pairs.every(({ original, candidate }) => (
        original.query.category !== "evergreen"
        || original.score.current_rank === null
        || original.score.current_rank > 5
        || (candidate.score.current_rank !== null && candidate.score.current_rank <= 5)
      ));
      const noNewEvergreenInversion = pairs.every(({ original, candidate }) => (
        original.query.category !== "evergreen"
        || original.score.recent_irrelevant_inversion === true
        || candidate.score.recent_irrelevant_inversion !== true
      ));
      const obsoleteTopTenRetained = pairs.every(({ original, candidate }) => (
        original.query.category !== "conflict"
        || original.score.older_conflict_rank === null
        || original.score.older_conflict_rank > 10
        || (candidate.score.older_conflict_rank !== null && candidate.score.older_conflict_rank <= 10)
      ));
      const hardGates = {
        ...comparison.validity,
        phase2_disabled_identity_parity: phase2Parity.by_profile[profile].exact_ranked_identities,
        required_stages_active: pairs.every(({ candidate }) => {
          const expected = expectedDiagnostics(profile);
          return candidate.diagnostics.vector_status === expected.vector
            && candidate.diagnostics.reranker_status === expected.reranker
            && candidate.diagnostics.freshness_status === "active"
            && candidate.diagnostics.max_rank_movement === bound;
        }),
        repeated_orders_stable: pairs.every(({ candidate }) => candidate.repeated_exact_order),
        current_sources_only: pairs.every(
          ({ candidate }) => candidate.hits.every((hit) => hit.source_state === "current"),
        ),
        all_candidates_reported: pairs.every(
          ({ candidate }) => candidate.hits.length === candidate.diagnostics.total_candidates,
        ),
      };
      const guardrails = {
        recall_at_5_not_lower:
          candidateSummary.current_answer_recall[5] >= baselineSummary.current_answer_recall[5],
        recall_at_10_not_lower:
          candidateSummary.current_answer_recall[10] >= baselineSummary.current_answer_recall[10],
        evergreen_top_five_retained: evergreenTopFiveRetained,
        no_new_recent_irrelevant_over_evergreen_inversion: noNewEvergreenInversion,
        obsolete_conflict_top_ten_retained: obsoleteTopTenRetained,
        no_intended_conflict_reversed: reversed === 0,
        intended_conflict_corrected: corrected > 0,
      };
      return {
        profile,
        hard_gates: hardGates,
        guardrails,
        corrected_comparable_conflicts: corrected,
        reversed_comparable_conflicts: reversed,
        current_first_conflict_pairs: pairs.filter(
          ({ candidate }) => candidate.query.category === "conflict"
            && comparableConflictPair(candidate)
            && candidate.score.current_ahead_of_older === true,
        ).length,
        current_answer_recall_at_3: candidateSummary.current_answer_recall[3],
        changed_order_count: comparison.changed_orders,
        passes:
          Object.values(hardGates).every(Boolean) && Object.values(guardrails).every(Boolean),
      };
    });
    const correctedComparableConflicts = profileComparisons.reduce(
      (sum, profile) => sum + profile.corrected_comparable_conflicts,
      0,
    );
    const currentFirstConflictPairs = profileComparisons.reduce(
      (sum, profile) => sum + profile.current_first_conflict_pairs,
      0,
    );
    const currentAnswerRecallAt3 = mean(profileComparisons.map((profile) => profile.current_answer_recall_at_3));
    const changedOrderCount = profileComparisons.reduce((sum, profile) => sum + profile.changed_order_count, 0);
    const passes = profileComparisons.every((profile) => profile.passes);
    return {
      bound,
      passes,
      corrected_comparable_conflicts: correctedComparableConflicts,
      current_first_conflict_pairs: currentFirstConflictPairs,
      current_answer_recall_at_3: currentAnswerRecallAt3,
      changed_order_count: changedOrderCount,
      profiles: profileComparisons,
    };
  });
  const passing = candidates.filter((candidate) => candidate.passes);
  const bestConflictCount = passing.length === 0
    ? null
    : Math.max(...passing.map((candidate) => candidate.current_first_conflict_pairs));
  const selected = passing
    .filter((candidate) => candidate.current_first_conflict_pairs === bestConflictCount)
    .toSorted((left, right) => left.bound - right.bound)[0] ?? null;
  return {
    candidates,
    selected_default: selected === null ? { freshness: false, max_rank_movement: null } : {
      freshness: true,
      max_rank_movement: selected.bound,
    },
    selection_rule: "best current-first conflict count, then smallest passing bound; report also records Recall@3 and changed-order tie inputs",
  };
}

function normalizeHit(hit, query) {
  const normalized = {
    rank: hit.rank,
    base_rank: hit.base_rank ?? hit.rank,
    movement: hit.movement ?? 0,
    path: hit.path,
    heading: hit.breadcrumb,
    lines: [hit.start_line, hit.end_line],
    source_bytes: [hit.source_start, hit.source_end],
    file_hash: hit.file_hash,
    content_sha256: sha256(hit.content),
    lexical_score: hit.lexical_score,
    vector_score: hit.vector_score,
    freshness_basis: hit.freshness_basis ?? null,
    freshness_value: hit.freshness_value ?? null,
    freshness_secondary_value: hit.freshness_secondary_value ?? null,
    freshness_detail: hit.freshness_detail ?? null,
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

export function validatePhase2Report(report, manifest, fingerprints) {
  if (!report || report.schema !== "jscout.docs-retrieval-eval.v1" || report.schema_version !== 1) {
    throw new Error("Phase 2 report has an unsupported schema");
  }
  if (report.run_kind !== "phase2-baseline" || report.decision !== "phase2-baseline-recorded") {
    throw new Error("Phase 2 report is not a recorded phase2-baseline");
  }
  if (report.suite !== manifest.suite) throw new Error("Phase 2 report suite differs from the manifest");
  if (report.inputs?.manifest_sha256 !== fingerprints.manifestSha256
      || report.inputs?.fixture_sha256 !== fingerprints.fixtureSha256) {
    throw new Error("Phase 2 report corpus fingerprints differ from the current fixture");
  }
  if (report.inputs?.profiles?.length !== PHASE2_PROFILES.length
      || new Set(report.inputs.profiles).size !== PHASE2_PROFILES.length
      || !PHASE2_PROFILES.every((profile) => report.inputs.profiles.includes(profile))) {
    throw new Error("Phase 2 report does not contain every frozen retrieval posture");
  }
  for (const gate of [
    "bm25_fallback_exact_order",
    "repeated_orders_stable",
    "phase2_complete",
    "required_profiles_present",
    "hybrid_measured",
    "hybrid_rerank_measured",
  ]) {
    if (report.validity?.[gate] !== true) throw new Error(`Phase 2 report failed validity gate: ${gate}`);
  }
  if (!Array.isArray(report.runs)) throw new Error("Phase 2 report has no runs");
  const expectedKeys = new Set(manifest.queries.flatMap((query) => query.variants.flatMap(
    (variant) => PHASE2_PROFILES.map((profile) => `${profile}\0${variant}\0${query.id}`),
  )));
  const keys = new Set();
  for (const run of report.runs) {
    const key = `${run.profile}\0${run.variant}\0${run.query?.id}`;
    if (keys.has(key)) throw new Error(`Phase 2 report repeats run: ${key.replaceAll("\0", "/")}`);
    if (!expectedKeys.has(key)) throw new Error(`Phase 2 report contains unexpected run: ${key.replaceAll("\0", "/")}`);
    keys.add(key);
  }
  if (keys.size !== expectedKeys.size) {
    throw new Error("Phase 2 report run count differs from the fixed manifest");
  }
  return report;
}

export function phase2ValidityForReport(phase3, profiles, fallbackComparison, phase2Report) {
  if (phase3) {
    return {
      bm25_fallback_exact_order: phase2Report.validity.bm25_fallback_exact_order,
      phase2_complete: phase2Report.validity.phase2_complete,
    };
  }
  return {
    bm25_fallback_exact_order: fallbackComparison?.exact_order_parity ?? false,
    phase2_complete: PHASE2_PROFILES.every((profile) => profiles.includes(profile)),
  };
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

export function docsSearchArguments(profile, root, query, database, treatment = null) {
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
    ...(treatment?.enabled === false ? ["--no-freshness"] : []),
    ...searchArguments(profile),
  ];
}

export function configWithFreshness(source, treatment) {
  const lines = source.split(/\r?\n/);
  const header = lines.findIndex((line) => /^\s*\[docs\.search\]\s*(?:#.*)?$/.test(line));
  if (header < 0) throw new Error("evaluation config must contain a [docs.search] table");
  let end = header + 1;
  while (end < lines.length && !/^\s*\[/.test(lines[end])) end += 1;
  const body = lines.slice(header + 1, end).filter(
    (line) => !/^\s*(?:freshness|max_rank_movement)\s*=/.test(line),
  );
  body.push(`freshness = ${treatment.enabled ? "true" : "false"}`);
  body.push(`max_rank_movement = ${treatment.bound}`);
  lines.splice(header + 1, end - header - 1, ...body);
  return `${lines.join("\n").replace(/\n+$/, "")}\n`;
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
  if (options.phase2Report && !existsSync(options.phase2Report)) {
    throw new Error(`Phase 2 report does not exist: ${options.phase2Report}`);
  }
  const manifest = validateManifest(readJson(options.manifest), options.manifest);
  const fixtureRoot = dirname(options.manifest);
  const manifestSha256 = sha256File(options.manifest);
  const fixtureSha256 = directoryDigest(fixtureRoot);
  const phase2Report = options.phase2Report
    ? validatePhase2Report(readJson(options.phase2Report), manifest, { manifestSha256, fixtureSha256 })
    : null;
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
  const treatmentConfigs = new Map();
  if (options.runKind === "phase3-candidate") {
    const sources = {
      baseline: readFileSync(baselineConfig, "utf8"),
      provider: readFileSync(options.providerConfig, "utf8"),
    };
    for (const treatment of FRESHNESS_TREATMENTS) {
      for (const [kind, source] of Object.entries(sources)) {
        const path = join(workspace, `phase3-${kind}-${treatment.id}.toml`);
        writeFileSync(path, configWithFreshness(source, treatment));
        treatmentConfigs.set(`${kind}\0${treatment.id}`, path);
      }
    }
  }
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
      phase3_treatments: options.runKind === "phase3-candidate"
        ? Object.fromEntries(FRESHNESS_TREATMENTS.map((treatment) => [treatment.id, {
          baseline: configShow(
            options.binary,
            treatmentConfigs.get(`baseline\0${treatment.id}`),
            repositories.values().next().value.path,
            baseEnv,
          ),
          provider: configShow(
            options.binary,
            treatmentConfigs.get(`provider\0${treatment.id}`),
            repositories.values().next().value.path,
            providerEnv,
          ),
        }]))
        : null,
    };
    if (["phase2-baseline", "phase3-candidate"].includes(options.runKind)) {
      validatePhase2ProviderConfiguration(configurations.provider);
    }
    if (options.runKind === "phase3-candidate") {
      for (const treatment of FRESHNESS_TREATMENTS) {
        for (const kind of ["baseline", "provider"]) {
          const docsSearch = configurations.phase3_treatments[treatment.id][kind].docs.search;
          if (docsSearch.freshness !== treatment.enabled
              || docsSearch.max_rank_movement !== treatment.bound) {
            throw new Error(`${treatment.id}/${kind}: effective freshness treatment differs from the matrix`);
          }
        }
      }
    }
    const serviceConfigurationBefore = ["phase2-baseline", "phase3-candidate"].includes(options.runKind)
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
          const treatments = options.runKind === "phase3-candidate"
            ? FRESHNESS_TREATMENTS
            : [{ id: null, enabled: false, bound: 0 }];
          for (const treatment of treatments) {
            const configKind = profile.startsWith("hybrid") ? "provider" : "baseline";
            const config = options.runKind === "phase3-candidate"
              ? treatmentConfigs.get(`${configKind}\0${treatment.id}`)
              : profile.startsWith("hybrid") ? options.providerConfig : baselineConfig;
            const env = profile.startsWith("hybrid") ? providerEnv : baseEnv;
            const searchArgs = docsSearchArguments(
              profile,
              repository.path,
              query.query,
              database,
              options.runKind === "phase3-candidate" ? treatment : null,
            );
            const response = runJscoutJson(options.binary, config, searchArgs, env);
            const repeated = runJscoutJson(options.binary, config, searchArgs, env);
            const expected = expectedDiagnostics(profile);
            const label = [repository.id, query.id, profile, treatment.id].filter(Boolean).join("/");
            if (response.value.truncated) throw new Error(`${label}: response truncated`);
            if (repeated.value.truncated) throw new Error(`${label}: repeated response truncated`);
            if (response.value.diagnostics.vector_status !== expected.vector) {
              throw new Error(
                `${label}: vector=${response.value.diagnostics.vector_status}, expected ${expected.vector}`,
              );
            }
            if (response.value.diagnostics.reranker_status !== expected.reranker) {
              throw new Error(
                `${label}: reranker=${response.value.diagnostics.reranker_status}, expected ${expected.reranker}`,
              );
            }
            if (repeated.value.diagnostics.vector_status !== expected.vector
                || repeated.value.diagnostics.reranker_status !== expected.reranker) {
              throw new Error(`${label}: repeated retrieval stage status changed`);
            }
            if (options.runKind === "phase3-candidate") {
              const expectedFreshness = treatment.enabled ? "active" : "disabled";
              const expectedBound = treatment.bound;
              if (response.value.diagnostics.freshness_status !== expectedFreshness
                  || response.value.diagnostics.max_rank_movement !== expectedBound
                  || repeated.value.diagnostics.freshness_status !== expectedFreshness
                  || repeated.value.diagnostics.max_rank_movement !== expectedBound) {
                throw new Error(`${label}: effective freshness diagnostics differ from the treatment`);
              }
            }
            if (response.value.snapshot !== status.snapshot || repeated.value.snapshot !== status.snapshot) {
              throw new Error(`${label}: search snapshot differs from indexed status`);
            }
            if (profile.startsWith("hybrid")
                && response.value.diagnostics.vector_profile_id !== embed.value.profile_id) {
              throw new Error(`${label}: vector profile differs from docs embed`);
            }
            if (profile.startsWith("hybrid")
                && repeated.value.diagnostics.vector_profile_id !== embed.value.profile_id) {
              throw new Error(`${label}: repeated vector profile differs from docs embed`);
            }
            const hits = response.value.hits.map((hit) => normalizeHit(hit, query));
            const repeatedHits = repeated.value.hits.map((hit) => normalizeHit(hit, query));
            const stableOrder = JSON.stringify(hits.map(hitIdentity)) === JSON.stringify(repeatedHits.map(hitIdentity));
            if (!stableOrder) throw new Error(`${label}: repeated rank order changed`);
            if (hits.some((hit) => hit.source_state !== "current")) {
              throw new Error(`${label}: source resolution was not current`);
            }
            if (repeatedHits.some((hit) => hit.source_state !== "current")) {
              throw new Error(`${label}: repeated source resolution was not current`);
            }
            runs.push({
              variant: repository.id,
              profile,
              treatment: treatment.id,
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
    }
    const scoredRuns = runs.map((run) => ({
      ...run,
      query: manifest.queries.find((query) => query.id === run.query.id),
    }));
    const phase3 = options.runKind === "phase3-candidate";
    const summaries = phase3
      ? Object.fromEntries(options.profiles.map((profile) => [
        profile,
        Object.fromEntries(FRESHNESS_TREATMENTS.map((treatment) => [
          treatment.id,
          summarizeProfile(scoredRuns.filter(
            (run) => run.profile === profile && run.treatment === treatment.id,
          )),
        ])),
      ]))
      : Object.fromEntries(options.profiles.map((profile) => [
        profile,
        summarizeProfile(scoredRuns.filter((run) => run.profile === profile)),
      ]));
    const summariesByVariant = Object.fromEntries(
      [...repositories.keys()].map((variant) => [
        variant,
        phase3
          ? Object.fromEntries(options.profiles.map((profile) => [
            profile,
            Object.fromEntries(FRESHNESS_TREATMENTS.map((treatment) => [
              treatment.id,
              summarizeProfile(scoredRuns.filter(
                (run) => run.variant === variant
                  && run.profile === profile
                  && run.treatment === treatment.id,
              )),
            ])),
          ]))
          : Object.fromEntries(options.profiles.map((profile) => [
            profile,
            summarizeProfile(scoredRuns.filter((run) => run.variant === variant && run.profile === profile)),
          ])),
      ]),
    );
    const conflictTreatmentOpportunity = Object.fromEntries(
      options.profiles.map((profile) => [
        profile,
        hasConflictTreatmentOpportunity(scoredRuns.filter(
          (run) => run.profile === profile && (!phase3 || run.treatment === "disabled"),
        )),
      ]),
    );
    if (options.runKind === "phase2-baseline"
        && Object.values(conflictTreatmentOpportunity).some((value) => !value)) {
      throw new Error("phase2-baseline has no obsolete-first conflict within the tested movement bounds");
    }
    const profileComparisonRuns = phase3
      ? scoredRuns.filter((run) => run.treatment === "disabled")
      : scoredRuns;
    const comparisons = [];
    for (const profile of options.profiles.filter((profile) => profile !== "lexical")) {
      comparisons.push(compareProfiles(
        profileComparisonRuns,
        "lexical",
        profile,
      ));
    }
    const fallback = comparisons.find((comparison) => comparison.candidate === "fallback");
    if (fallback && !fallback.exact_order_parity) {
      throw new Error("BM25 fallback order differs from --lexical-only");
    }
    const freshnessComparisons = phase3
      ? PHASE3_PROFILES.flatMap((profile) => [1, 2, 3].map(
        (bound) => compareFreshnessTreatments(scoredRuns, profile, `bound-${bound}`),
      ))
      : [];
    const phase2IdentityParity = phase3
      ? comparePhase2RankedIdentities(scoredRuns, phase2Report)
      : null;
    const defaultSelection = phase3
      ? selectPhase3Default(scoredRuns, freshnessComparisons, phase2IdentityParity)
      : null;
    const serviceConfigurationAfter = ["phase2-baseline", "phase3-candidate"].includes(options.runKind)
      ? await queryServiceConfiguration(configurations.provider.inference.url)
      : null;
    if (JSON.stringify(serviceConfigurationBefore) !== JSON.stringify(serviceConfigurationAfter)) {
      throw new Error(`inference service configuration changed during ${options.runKind}`);
    }
    const requiredProfiles = options.runKind === "phase2-baseline"
      ? PHASE2_PROFILES
      : phase3 ? PHASE3_PROFILES : PROVIDER_FREE_PROFILES;
    const decision = phase3
      ? defaultSelection.selected_default.freshness
        ? `freshness-bound-${defaultSelection.selected_default.max_rank_movement}-selected`
        : "freshness-disabled-selected"
      : options.runKind === "phase2-baseline"
        ? "phase2-baseline-recorded"
        : "provider-free-check-recorded";
    const phase2Validity = phase2ValidityForReport(
      phase3,
      options.profiles,
      fallback,
      phase2Report,
    );
    const result = {
      schema: "jscout.docs-retrieval-eval.v1",
      schema_version: 1,
      run_kind: options.runKind,
      suite: manifest.suite,
      generated_at: new Date().toISOString(),
      inputs: {
        fixture_sha256: fixtureSha256,
        manifest_sha256: manifestSha256,
        harness_sha256: sha256File(scriptPath),
        binary_sha256: sha256File(options.binary),
        binary_version: version,
        source_commit: sourceCommit,
        source_status: sourceStatus,
        git_version: gitVersion,
        provider_config_sha256: options.providerConfig ? sha256File(options.providerConfig) : null,
        phase2_report_sha256: options.phase2Report ? sha256File(options.phase2Report) : null,
        phase2_binary_sha256: phase2Report?.inputs?.binary_sha256 ?? null,
        profiles: options.profiles,
        freshness_treatments: phase3 ? FRESHNESS_TREATMENTS : null,
        max_k: MAX_K,
        response_bytes: RESPONSE_BYTES,
      },
      treatments: phase3 ? {
        retrieval: {
          lexical: ["--lexical-only"],
          hybrid: ["--vector", "--no-rerank"],
          "hybrid-rerank": ["--vector", "--rerank"],
        },
        freshness: {
          disabled: { config: { freshness: false, max_rank_movement: 1 }, flags: ["--no-freshness"] },
          "bound-1": { config: { freshness: true, max_rank_movement: 1 }, flags: [] },
          "bound-2": { config: { freshness: true, max_rank_movement: 2 }, flags: [] },
          "bound-3": { config: { freshness: true, max_rank_movement: 3 }, flags: [] },
        },
      } : {
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
      hybrid_lift_from_no_freshness_lexical: phase3 ? comparisons : null,
      freshness_comparisons: phase3 ? freshnessComparisons : null,
      phase2_disabled_identity_parity: phase2IdentityParity,
      default_selection: defaultSelection,
      validity: {
        bm25_fallback_exact_order: phase2Validity.bm25_fallback_exact_order,
        repeated_orders_stable: runs.every((run) => run.repeated_exact_order),
        source_tree_clean: sourceStatus.length === 0,
        phase2_complete: phase2Validity.phase2_complete,
        phase3_complete: phase3
          && PHASE3_PROFILES.every((profile) => options.profiles.includes(profile))
          && FRESHNESS_TREATMENTS.every((treatment) => runs.some((run) => run.treatment === treatment.id)),
        phase2_disabled_identity_parity: phase2IdentityParity?.exact_ranked_identities ?? null,
        conflict_treatment_opportunity: conflictTreatmentOpportunity,
        required_profiles_present: requiredProfiles.every((profile) => options.profiles.includes(profile)),
        hybrid_measured: options.profiles.includes("hybrid"),
        hybrid_rerank_measured: options.profiles.includes("hybrid-rerank"),
      },
      decision,
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
