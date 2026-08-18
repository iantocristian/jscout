#!/usr/bin/env node

// PR-replay runner: implement-a-real-change tasks in history-free workspaces.
//
// Separate from eval-run-codex.mjs (read-only localization) on purpose: replay
// runs need write access, per-run workspace copies, per-arm indexing, and
// patch grading. One runner per suite type, same conventions.
//
// Per (task × profile × trial):
//   1. export the parent with no history, install dependencies, and build;
//   2. create a synthetic one-commit Git baseline;
//   3. indexed arms install the skill, index, and mount the MCP server;
//   4. optionally run a read-only design call and inject its result into a
//      separate implementation call;
//   5. codex exec with --sandbox workspace-write and the implementation prompt;
//   6. save the agent patch, overlay hidden tests, and grade.
//
// The gold bundle stays outside every workspace and is never mounted.

import { execFileSync, spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import { createRequire } from "node:module";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

export const REPLAY_EXECUTION_POLICY = Object.freeze({
  networkAccess: true,
  networkPolicy: "prompt-restricted-external; loopback-required",
  webTools: false,
});

const NEXT_TEARDOWN_PRELOAD = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "eval-next-teardown-preload.cjs",
);

export function nodeOptionsWithNextTeardown(existing = "") {
  const requireOption = `--require=${NEXT_TEARDOWN_PRELOAD}`;
  const options = existing.trim();
  if (options.split(/\s+/).includes(requireOption)) return options;
  return options ? `${options} ${requireOption}` : requireOption;
}

// Codex launches configured MCP servers with the explicit server environment,
// not the runner's complete process environment. Forward only jscout's
// non-secret runtime selectors here; credentials remain outside argv/config.
const JSCOUT_MCP_FORWARDED_ENV = Object.freeze([
  "JSCOUT_PI_AI_GATEWAY",
  "JSCOUT_NODE",
  "JSCOUT_LLM_MODEL",
  "JSCOUT_LLM_REASONING",
  "JSCOUT_PI_AI_OPENAI_BASE_URL",
]);

export function jscoutMcpEnvironmentArgs(environment = {}) {
  const args = [];
  for (const name of JSCOUT_MCP_FORWARDED_ENV) {
    const value = environment[name];
    if (typeof value !== "string" || value.trim() === "") continue;
    args.push(
      "--config",
      `mcp_servers.jscout.env.${name}=${JSON.stringify(value)}`,
    );
  }
  return args;
}

let activeChild = null;
let activeBrowserChild = null;
let activeWorkspace = null;
let receivedSignal = null;

function killProcessGroup(child, signal) {
  if (!child?.pid) return;
  try {
    process.kill(-child.pid, signal);
  } catch {
    try { child.kill(signal); } catch { /* already gone */ }
  }
}

export function workspaceProcessGroups(psOutput, workspace) {
  const marker = `${path.resolve(workspace)}${path.sep}`;
  const groups = new Map();
  for (const line of psOutput.split(/\r?\n/)) {
    const match = line.match(/^\s*(\d+)\s+(\d+)\s+(.*)$/);
    if (!match || !match[3].includes(marker)) continue;
    const pid = Number(match[1]);
    const pgid = Number(match[2]);
    if (!Number.isInteger(pgid) || pgid <= 1) continue;
    const entry = groups.get(pgid) ?? { pgid, pids: [], commands: [] };
    entry.pids.push(pid);
    entry.commands.push(match[3]);
    groups.set(pgid, entry);
  }
  return [...groups.values()].sort((left, right) => left.pgid - right.pgid);
}

function inspectWorkspaceProcesses(workspace) {
  const result = spawnSync("ps", ["-axo", "pid=,pgid=,command="], {
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.error || result.status !== 0) return [];
  return workspaceProcessGroups(result.stdout, workspace);
}

function signalWorkspaceProcessGroups(groups, signal) {
  for (const group of groups) {
    try { process.kill(-group.pgid, signal); } catch { /* already gone */ }
  }
}

function waitSynchronously(milliseconds) {
  const waitArray = new Int32Array(new SharedArrayBuffer(4));
  Atomics.wait(waitArray, 0, 0, milliseconds);
}

function cleanupWorkspaceProcesses(workspace) {
  const found = inspectWorkspaceProcesses(workspace);
  signalWorkspaceProcessGroups(found, "SIGTERM");
  if (found.length === 0) return { terminated: [], killed: [] };
  waitSynchronously(500);
  const selected = new Set(found.map((group) => group.pgid));
  const remaining = inspectWorkspaceProcesses(workspace)
    .filter((group) => selected.has(group.pgid));
  signalWorkspaceProcessGroups(remaining, "SIGKILL");
  return { terminated: found, killed: remaining };
}

function recordWorkspaceCleanup(workspace, runDir, phase) {
  const groups = cleanupWorkspaceProcesses(workspace);
  if (groups.terminated.length === 0) return;
  const output = path.join(runDir, "process-cleanup.jsonl");
  fs.appendFileSync(output, `${JSON.stringify({ phase, groups })}\n`);
}

function handleSignal(signal) {
  receivedSignal = receivedSignal ?? signal;
  killProcessGroup(activeChild, "SIGTERM");
  try { activeBrowserChild?.kill("SIGTERM"); } catch { /* already gone */ }
  if (activeWorkspace) cleanupWorkspaceProcesses(activeWorkspace);
}

process.on("SIGINT", () => handleSignal("SIGINT"));
process.on("SIGTERM", () => handleSignal("SIGTERM"));

function throwIfInterrupted() {
  if (receivedSignal) throw new Error(`replay runner interrupted by ${receivedSignal}`);
}

function parseArgs(argv) {
  const options = {
    codex: "codex",
    model: "gpt-5.6-terra",
    reasoning: "high",
    profiles: "grep,structural",
    treatments: "skill,forced",
    trial: "001",
    workflow: "single",
  };
  for (let index = 0; index < argv.length; index += 2) {
    const flag = argv[index];
    const value = argv[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error("arguments must be --name value pairs");
    }
    options[flag.slice(2)] = value;
  }
  for (const required of ["tasks", "repository", "runs-root", "jscout", "responses", "telemetry", "artifacts"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

// stdin MUST be "ignore": codex exec blocks forever on an open stdin pipe.
// stdout streams to eventsPath as it arrives so progress is observable from
// outside (tail -f) instead of buffering until exit.
function run(command, args, { cwd, env, eventsPath, stderrPath, timeoutMs }) {
  return new Promise((resolve) => {
    const started = Date.now();
    const child = spawn(command, args, {
      cwd,
      env,
      detached: true,
      stdio: ["ignore", "pipe", "pipe"],
    });
    activeChild = child;
    const out = fs.createWriteStream(eventsPath);
    const err = fs.createWriteStream(stderrPath);
    child.stdout.pipe(out);
    child.stderr.pipe(err);
    let timedOut = false;
    let hardKillTimer = null;
    const timer = timeoutMs
      ? setTimeout(() => {
        timedOut = true;
        killProcessGroup(child, "SIGTERM");
        hardKillTimer = setTimeout(() => killProcessGroup(child, "SIGKILL"), 5_000);
      }, timeoutMs)
      : null;
    child.on("close", (code) => {
      if (timer) clearTimeout(timer);
      if (hardKillTimer) clearTimeout(hardKillTimer);
      if (activeChild === child) activeChild = null;
      resolve({
        code: code ?? 1,
        timedOut,
        interrupted: receivedSignal,
        duration_ms: Date.now() - started,
      });
    });
    child.on("error", () => {
      if (timer) clearTimeout(timer);
      if (hardKillTimer) clearTimeout(hardKillTimer);
      if (activeChild === child) activeChild = null;
      resolve({
        code: 127,
        timedOut,
        interrupted: receivedSignal,
        duration_ms: Date.now() - started,
      });
    });
  });
}

function jsonLines(text) {
  return text.split(/\r?\n/).filter(Boolean).flatMap((line) => {
    try { return [JSON.parse(line)]; } catch { return []; }
  });
}

function completedReplaySessions(responses) {
  if (!fs.existsSync(responses)) return new Set();
  return new Set(
    jsonLines(fs.readFileSync(responses, "utf8"))
      .map((row) => row.session)
      .filter(Boolean),
  );
}

function isStrictChild(candidate, parent) {
  const relative = path.relative(path.resolve(parent), path.resolve(candidate));
  return relative !== "" && relative !== ".." && !relative.startsWith(`..${path.sep}`);
}

function nextInterruptedRunPath(runDir) {
  for (let attempt = 1; ; attempt += 1) {
    const candidate = `${runDir}.interrupted-${String(attempt).padStart(3, "0")}`;
    if (!fs.existsSync(candidate)) return candidate;
  }
}

function archiveInterruptedRun(runDir, workRoot) {
  if (!fs.existsSync(runDir) || fs.readdirSync(runDir).length === 0) return null;
  const archived = nextInterruptedRunPath(runDir);
  fs.renameSync(runDir, archived);
  const workspacePathFile = path.join(archived, "workspace-path.txt");
  if (fs.existsSync(workspacePathFile)) {
    const workspace = fs.readFileSync(workspacePathFile, "utf8").trim();
    if (workspace && isStrictChild(workspace, workRoot)) {
      recordWorkspaceCleanup(workspace, archived, "resume-before-restart");
      fs.rmSync(workspace, { recursive: true, force: true });
    }
  }
  return archived;
}

function sha256(file) {
  return crypto.createHash("sha256").update(fs.readFileSync(file)).digest("hex");
}

function verifyInstalledSkill(workspace, expectedSha) {
  const skill = path.join(workspace, ".agents", "skills", "jscout", "SKILL.md");
  if (!fs.existsSync(skill)) return "installed jscout skill was removed during the run";
  if (sha256(skill) !== expectedSha) return "installed jscout skill was modified during the run";
  return null;
}

function runLogged(command, args, {
  cwd,
  log,
  env = process.env,
  maxBuffer = 256 * 1024 * 1024,
  // Generative scout stages exit nonzero to report subject-local failures
  // while still publishing every successful artifact (scout_batch_exit).
  // Callers that can proceed on partial output pass tolerateExit with a
  // predicate over the log text; a nonzero exit is then recorded, not fatal.
  tolerateExit = null,
}) {
  const result = spawnSync(command, args, {
    cwd,
    env,
    encoding: "utf8",
    maxBuffer,
    stdio: ["ignore", "pipe", "pipe"],
  });
  const output = `${result.stdout ?? ""}${result.stderr ?? ""}`;
  fs.writeFileSync(log, output);
  if (result.error || result.status !== 0) {
    if (!result.error && tolerateExit && tolerateExit(output)) {
      process.stderr.write(
        `tolerated partial failure: ${command} ${args.join(" ")} exited ${result.status}; see ${log}\n`,
      );
      return;
    }
    throw new Error(
      `${command} ${args.join(" ")} failed (${result.status ?? result.error?.message ?? "unknown"}); see ${log}`,
    );
  }
}

// A generative scout run is usable when it PUBLISHED at least one artifact.
// The `reports:` summary counts failed subjects too, so it proves nothing —
// an all-timeout batch reports nonzero. Evidence of publication is either a
// per-subject `artifact: <id>` line in the output or, decisively (hard
// aborts print no summary at all), growth of the stage database's
// semantic_artifacts count.
export function scoutPublishedArtifacts(output) {
  return /^\s*artifact: \d+/m.test(output);
}

export function countSemanticArtifacts(database) {
  try {
    const result = spawnSync("sqlite3", [
      `file:${database}?immutable=1`,
      "SELECT count(*) FROM semantic_artifacts",
    ], { encoding: "utf8" });
    if (result.status !== 0) return null;
    const count = Number(result.stdout.trim());
    return Number.isFinite(count) ? count : null;
  } catch {
    return null;
  }
}

function gitCommitWithoutHooks(workspace, args) {
  execFileSync("git", ["-c", "core.hooksPath=/dev/null", ...args], {
    cwd: workspace,
    env: { ...process.env, HUSKY: "0" },
    maxBuffer: 256 * 1024 * 1024,
  });
}

function prepareArm(repository, parent, workspace, setupCommand, runDir, env) {
  fs.mkdirSync(workspace, { recursive: true });
  const tar = path.join(os.tmpdir(), `jscout-arm-${parent.slice(0, 12)}-${process.pid}.tar`);
  execFileSync("git", ["-C", repository, "archive", "--format=tar", "-o", tar, parent]);
  execFileSync("tar", ["-xf", tar, "-C", workspace]);
  fs.unlinkSync(tar);

  // The synthetic repository contains only the parent source snapshot. It
  // gives the grader an exact baseline without exposing upstream history.
  execFileSync("git", ["init", "-q", "-b", "eval-base"], { cwd: workspace });
  execFileSync("git", ["config", "user.email", "jscout-eval@invalid"], { cwd: workspace });
  execFileSync("git", ["config", "user.name", "jscout eval"], { cwd: workspace });
  execFileSync("git", ["add", "-f", "-A"], { cwd: workspace, maxBuffer: 256 * 1024 * 1024 });
  gitCommitWithoutHooks(workspace, ["commit", "-qm", "eval base"]);

  if (setupCommand) {
    runLogged("sh", ["-c", setupCommand], {
      cwd: workspace,
      log: path.join(runDir, "setup.log"),
      env,
    });
    // If setup changed an archived/tracked file, make that prepared state the
    // baseline. Generated and dependency files remain ignored.
    execFileSync("git", ["add", "-u"], { cwd: workspace });
    const staged = spawnSync("git", ["diff", "--cached", "--quiet"], { cwd: workspace });
    if (staged.status !== 0) {
      gitCommitWithoutHooks(workspace, ["commit", "--amend", "--no-edit", "-q"]);
    }
  }
}

const PROFILE_PLANS = Object.freeze({
  baseline: [],
  structural: [],
  checker: ["enrich"],
  "checker-embed": ["enrich", "embed"],
  "checker-scout": ["enrich", "scout"],
  "checker-scout-embed": ["enrich", "scout", "embed-product"],
  // Operational ordering: scout before checker so reconnaissance policy can
  // shape checker scheduling and product embedding. No base profile: the
  // corpus this order produces must not inherit additive-profile state.
  "production-order": ["scout", "enrich", "embed-product"],
  // The memory-only plane: generative workflow/card/summary artifacts persisted
  // over checker facts and reconnaissance policy. Keep this free of embeddings
  // so it remains an isolatable treatment.
  memory: ["enrich", "scout", "workflows", "cards", "summaries"],
  // Full semantic-memory treatment: the same persisted artifacts plus
  // product-scoped vectors and the local query embedder/reranker at runtime.
  // Keep this separate from `memory` so memory-only trials remain comparable.
  "memory-embed": [
    "enrich", "scout", "embed-product", "workflows", "cards", "summaries",
    "embed-semantic",
  ],
});

const PROFILE_BASES = Object.freeze({
  checker: "structural",
  "checker-embed": "checker",
  "checker-scout": "checker",
  "checker-scout-embed": "checker-scout",
  memory: "checker-scout",
  "memory-embed": "checker-scout-embed",
});

const PROFILE_INCREMENT = Object.freeze({
  checker: "enrich",
  "checker-embed": "embed",
  "checker-scout": "scout",
  "checker-scout-embed": "embed-product",
  memory: ["workflows", "cards", "summaries"],
  "memory-embed": ["workflows", "cards", "summaries", "embed-semantic"],
});

export function profilePlan(profile) {
  if (profile === "grep") return { usesJscout: false, stages: [] };
  const stages = PROFILE_PLANS[profile];
  if (!stages) throw new Error(`unknown profile: ${profile}`);
  return { usesJscout: true, stages };
}

export function embeddingEnvironmentForProfile(profile) {
  const plan = profilePlan(profile);
  return plan.stages.some((stage) => stage.startsWith("embed"))
    ? { JSCOUT_EMBED_PROVIDER: "local" }
    : {};
}

function copyDatabase(source, destination) {
  for (const suffix of ["", "-wal", "-shm"]) {
    const from = `${source}${suffix}`;
    if (fs.existsSync(from)) {
      fs.copyFileSync(from, `${destination}${suffix}`, fs.constants.COPYFILE_FICLONE);
    }
  }
}

export function preparedDatabaseManifest({ taskId, parent, profile, stages }) {
  return {
    schema_version: 1,
    task_id: taskId,
    parent,
    profile,
    stages: [...stages],
  };
}

function manifestPath(database) {
  return `${database}.manifest.json`;
}

function writePreparedDatabaseManifest(database, manifest) {
  fs.writeFileSync(manifestPath(database), `${JSON.stringify(manifest, null, 2)}\n`);
}

export function validatePreparedDatabaseManifest(actual, expected) {
  for (const field of ["schema_version", "task_id", "parent", "profile"]) {
    if (actual?.[field] !== expected[field]) {
      throw new Error(
        `prepared database manifest mismatch for ${field}: expected ${JSON.stringify(expected[field])}, got ${JSON.stringify(actual?.[field])}`,
      );
    }
  }
  if (JSON.stringify(actual.stages) !== JSON.stringify(expected.stages)) {
    throw new Error(
      `prepared database manifest mismatch for stages: expected ${JSON.stringify(expected.stages)}, got ${JSON.stringify(actual.stages)}`,
    );
  }
}

function requirePreparedDatabaseManifest(database, expected) {
  const file = manifestPath(database);
  if (!fs.existsSync(file)) {
    throw new Error(
      `prepared database has no provenance manifest: ${database}; rebuild it before reuse`,
    );
  }
  let actual;
  try {
    actual = JSON.parse(fs.readFileSync(file, "utf8"));
  } catch (error) {
    throw new Error(`unable to read prepared database manifest ${file}: ${error.message}`);
  }
  validatePreparedDatabaseManifest(actual, expected);
  return actual;
}

function prepareJscoutProfile({
  jscout,
  workspace,
  database,
  profile,
  runDir,
  scoutMaxCalls,
  scoutMaxSubjects,
  scoutWarnSubjects,
  memoryBudgets,
  baseDatabase,
  executionEnvironment,
}) {
  const plan = profilePlan(profile);
  const env = {
    ...process.env,
    ...executionEnvironment,
    ...embeddingEnvironmentForProfile(profile),
  };
  let stages = plan.stages;
  if (baseDatabase && fs.existsSync(baseDatabase)) {
    copyDatabase(baseDatabase, database);
    stages = [].concat(PROFILE_INCREMENT[profile]);
    fs.writeFileSync(
      path.join(runDir, "jscout-profile-base.json"),
      `${JSON.stringify({ profile: PROFILE_BASES[profile], database: baseDatabase }, null, 2)}\n`,
    );
  } else {
    runLogged(jscout, ["index", workspace, "--database", database], {
      cwd: workspace,
      env,
      log: path.join(runDir, "jscout-index.log"),
    });
  }
  for (const stage of stages) {
    if (stage === "enrich") {
      runLogged(jscout, ["enrich", workspace, "--database", database], {
        cwd: workspace,
        env,
        log: path.join(runDir, "jscout-enrich.log"),
      });
    } else if (stage === "scout") {
      runLogged(
        jscout,
        [
          "scout", "repository", workspace,
          "--database", database,
          "--max-calls", String(scoutMaxCalls),
          "--max-subjects", String(scoutMaxSubjects),
          "--warn-subjects", String(scoutWarnSubjects),
        ],
        {
          cwd: workspace,
          env,
          log: path.join(runDir, "jscout-scout.log"),
        },
      );
    } else if (stage === "workflows" || stage === "cards" || stage === "summaries") {
      const artifactsBefore = countSemanticArtifacts(database);
      runLogged(
        jscout,
        [
          "scout", stage, workspace,
          "--database", database,
          "--max-calls", String(memoryBudgets[stage]),
        ],
        {
          cwd: workspace,
          env,
          log: path.join(runDir, `jscout-${stage}.log`),
          tolerateExit: (output) => {
            if (scoutPublishedArtifacts(output)) return true;
            const after = countSemanticArtifacts(database);
            return after !== null && artifactsBefore !== null && after > artifactsBefore;
          },
        },
      );
    } else if (stage === "embed" || stage === "embed-product" || stage === "embed-semantic") {
      runLogged(
        jscout,
        [
          "embed", workspace,
          "--database", database,
          ...(stage === "embed-product" ? ["--product"] : []),
          ...(stage === "embed-semantic" ? ["--semantic-only"] : []),
        ],
        {
          cwd: workspace,
          env,
          log: path.join(runDir, "jscout-embed.log"),
        },
      );
    }
  }
}

function resolveExecutionEnvironment(taskSet, task) {
  const merged = {
    ...(taskSet.execution_environment ?? {}),
    ...(task.execution_environment ?? {}),
  };
  for (const [name, value] of Object.entries(merged)) {
    if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(name)) {
      throw new Error(`invalid execution-environment variable name: ${name}`);
    }
    if (typeof value !== "string") {
      throw new Error(`execution-environment value for ${name} must be a string`);
    }
  }
  return merged;
}

export function resolveBrowserServerPolicy(taskSet = {}, task = {}) {
  const policy = task.browser_server ?? taskSet.browser_server ?? "auto";
  if (!["auto", "required", "disabled"].includes(policy)) {
    throw new Error("browser_server must be auto, required, or disabled");
  }
  return policy;
}

export function browserServerCapability(workspace, policy = "auto") {
  if (policy === "disabled") {
    return {
      start: false,
      policy,
      reason: "browser server disabled by task configuration",
    };
  }
  try {
    const workspaceRequire = createRequire(path.join(path.resolve(workspace), "package.json"));
    return {
      start: true,
      policy,
      playwrightModule: workspaceRequire.resolve("playwright"),
    };
  } catch {
    if (policy === "required") {
      throw new Error(
        "browser_server is required but playwright is not installed in the prepared workspace",
      );
    }
    return {
      start: false,
      policy,
      reason: "playwright is not installed in the prepared workspace",
    };
  }
}

function stopBrowserServer(child, graceMs = 5_000) {
  if (!child || child.exitCode !== null || child.signalCode !== null) {
    if (activeBrowserChild === child) activeBrowserChild = null;
    return Promise.resolve();
  }
  return new Promise((resolve) => {
    let settled = false;
    const finish = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (activeBrowserChild === child) activeBrowserChild = null;
      resolve();
    };
    const timer = setTimeout(() => {
      try { child.kill("SIGKILL"); } catch { /* already gone */ }
      finish();
    }, graceMs);
    child.once("close", finish);
    try { child.kill("SIGTERM"); } catch { finish(); }
  });
}

// Chromium cannot launch inside Codex's seatbelt (no mach-register), so a
// browser server runs OUTSIDE the sandbox and the workspace's own e2e
// harness connects to it via NEXT_TEST_BROWSER_WS_ENDPOINT (native support
// in test/lib/browsers/playwright.ts). Loopback sockets are sandbox-allowed.
export function startBrowserServer(
  workspace,
  logPath,
  timeoutMs = 60_000,
  playwrightModule = "playwright",
) {
  return new Promise((resolve, reject) => {
    const log = fs.createWriteStream(logPath, { flags: "a" });
    const child = spawn(process.execPath, ["-e", `
      const { chromium } = require(process.argv[1]);
      chromium.launchServer({ host: '127.0.0.1', headless: true }).then((server) => {
        console.log('WS_ENDPOINT=' + server.wsEndpoint());
        process.on('SIGTERM', () => server.close().then(() => process.exit(0)));
      }).catch((error) => { console.error(error); process.exit(1); });
    `, playwrightModule], { cwd: workspace, stdio: ["ignore", "pipe", "pipe"] });
    activeBrowserChild = child;
    let out = "";
    let settled = false;
    const fail = async (error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      await stopBrowserServer(child);
      reject(error);
    };
    const timer = setTimeout(() => {
      void fail(new Error(`browser server did not start within ${timeoutMs}ms; see ${logPath}`));
    }, timeoutMs);
    child.stdout.on("data", (chunk) => {
      log.write(chunk);
      out += chunk;
      const match = out.match(/WS_ENDPOINT=(ws:[^\s]+)/);
      if (match && !settled) {
        settled = true;
        clearTimeout(timer);
        resolve({ child, endpoint: match[1] });
      }
    });
    child.stderr.pipe(log, { end: false });
    child.on("error", (error) => {
      void fail(new Error(`browser server failed to spawn: ${error.message}; see ${logPath}`));
    });
    child.on("close", (code, signal) => {
      clearTimeout(timer);
      if (activeBrowserChild === child) activeBrowserChild = null;
      log.end();
      if (!settled) {
        settled = true;
        reject(new Error(
          `browser server exited before publishing an endpoint (code=${code}, signal=${signal}); see ${logPath}`,
        ));
      }
    });
  });
}

function saveAgentPatch(workspace, output) {
  const untracked = execFileSync(
    "git",
    ["ls-files", "--others", "--exclude-standard"],
    { cwd: workspace, encoding: "utf8", maxBuffer: 64 * 1024 * 1024 },
  ).split(/\r?\n/).filter(Boolean);
  if (untracked.length > 0) {
    execFileSync("git", ["add", "-N", "--", ...untracked], {
      cwd: workspace,
      maxBuffer: 256 * 1024 * 1024,
    });
  }
  fs.writeFileSync(
    output,
    execFileSync("git", ["diff", "--binary", "HEAD"], {
      cwd: workspace,
      maxBuffer: 256 * 1024 * 1024,
    }),
  );
}

function tokenUsage(events) {
  let input = 0;
  let cachedInput = 0;
  let output = 0;
  let reasoningOutput = 0;
  for (const event of events) {
    const usage = event.usage ?? event.payload?.usage;
    if (usage) {
      // Codex may repeat cumulative usage in more than one event. Preserve the
      // largest cumulative values instead of double-counting them.
      input = Math.max(input, Number(usage.input_tokens ?? 0));
      cachedInput = Math.max(cachedInput, Number(usage.cached_input_tokens ?? 0));
      output = Math.max(output, Number(usage.output_tokens ?? 0));
      reasoningOutput = Math.max(
        reasoningOutput,
        Number(usage.reasoning_output_tokens ?? 0),
      );
    }
  }
  return {
    input_tokens: input,
    cached_input_tokens: cachedInput,
    noncached_input_tokens: Math.max(0, input - cachedInput),
    output_tokens: output,
    reasoning_output_tokens: reasoningOutput,
    total_tokens: input + output,
  };
}

function commandUsage(events) {
  const commands = events.filter(
    (event) => event.type === "item.completed" && event.item?.type === "command_execution",
  );
  const outputBytes = commands.map((event) =>
    Buffer.byteLength(event.item.aggregated_output ?? "", "utf8"),
  );
  return {
    command_calls: commands.length,
    failed_command_calls: commands.filter((event) => event.item.status === "failed").length,
    command_output_bytes: outputBytes.reduce((sum, bytes) => sum + bytes, 0),
    max_command_output_bytes: Math.max(0, ...outputBytes),
  };
}

function addUsage(left, right) {
  const combined = {};
  for (const key of [
    "input_tokens",
    "cached_input_tokens",
    "noncached_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
    "command_calls",
    "failed_command_calls",
    "command_output_bytes",
  ]) {
    combined[key] = Number(left[key] ?? 0) + Number(right[key] ?? 0);
  }
  combined.max_command_output_bytes = Math.max(
    Number(left.max_command_output_bytes ?? 0),
    Number(right.max_command_output_bytes ?? 0),
  );
  return combined;
}

function phaseUsage(events) {
  return { ...tokenUsage(events), ...commandUsage(events) };
}

function prefixedUsage(prefix, usage) {
  return Object.fromEntries(
    Object.entries(usage).map(([key, value]) => [`${prefix}_${key}`, value]),
  );
}

export function validateDesignResponse(value) {
  const fields = [
    "mechanism",
    "constraints",
    "implementation_plan",
    "files",
    "symbols",
    "validation_plan",
    "uncertainties",
  ];
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("design response must be an object");
  }
  const unexpected = Object.keys(value).filter((key) => !fields.includes(key));
  if (unexpected.length > 0) {
    throw new Error(`design response has unexpected fields: ${unexpected.join(", ")}`);
  }
  if (typeof value.mechanism !== "string" || value.mechanism.trim() === "") {
    throw new Error("design response mechanism must be a non-empty string");
  }
  for (const field of fields.slice(1)) {
    if (!Array.isArray(value[field]) || value[field].some((item) => typeof item !== "string")) {
      throw new Error(`design response ${field} must be an array of strings`);
    }
  }
  if (value.implementation_plan.length === 0) {
    throw new Error("design response implementation_plan must not be empty");
  }
  if (value.validation_plan.length === 0) {
    throw new Error("design response validation_plan must not be empty");
  }
  return value;
}

export function designPromptFor(task, treatment = "control") {
  const contract = [
    "You are the design phase of a repository implementation evaluation.",
    "Do not edit files, generate a patch, or run mutating commands. Investigate",
    "the checked-out snapshot and design the complete change before implementation begins.",
    "A separate implementation phase will receive your design verbatim.",
    "",
    "## Story",
    task.story,
    "",
    "## Design contract",
    "- Explain the underlying mechanism, not only the nearest failing assertion or edit site.",
    "- Trace the state, control, and data flow far enough to identify every production",
    "  surface that may need to change.",
    "- State constraints, uncertainties, and the evidence that would falsify the design.",
    "- Provide a concrete file/symbol implementation plan and validation plan.",
    "- Dependencies are already installed. Do not install or update them and do not",
    "  access external network services.",
  ];
  if (task.execution_notes?.length) {
    contract.push("- Task-specific execution constraints:");
    for (const note of task.execution_notes) contract.push(`  - ${note}`);
  }
  if (treatment === "forced") {
    contract.push(
      "- Use jscout exclusively for repository-wide code discovery, symbol lookup,",
      "  reference tracing, and architectural navigation. Do not use grep, rg,",
      "  git grep, find, IDE search, or custom repository-scanning scripts.",
      "- You may directly read files and line ranges identified by jscout.",
    );
  }
  contract.push(
    "- Work only inside this synthetic one-commit snapshot. Do not inspect other",
    "  filesystem locations, GitHub, upstream Git history/remotes, or web/search services.",
    "- Finish with one JSON object matching the design output schema.",
  );
  return contract.join("\n");
}

export function implementationPromptFor(task, treatment, design, options = {}) {
  return [
    promptFor(task, treatment, options),
    "",
    "## Phase 1 design",
    "The same execution model produced the following design from this exact snapshot",
    "in a preceding read-only phase. Use it as the working hypothesis during",
    "implementation. Verify it against source and tests, revise it if evidence",
    "contradicts it, and do not silently collapse the change back to only the nearest",
    "failing assertion or edit site.",
    "",
    "```json",
    JSON.stringify(design, null, 2),
    "```",
  ].join("\n");
}

function codexArgs({
  options,
  workspace,
  schema,
  lastMessage,
  sandbox,
  usesJscout,
  jscout,
  database,
  profile,
  telemetry,
  taskId,
  session,
  requestLog,
  environment,
}) {
  const args = [
    "exec",
    "--ignore-user-config",
    "--ephemeral",
    "--skip-git-repo-check",
    "--sandbox", sandbox,
    "--cd", workspace,
    "--model", options.model,
    "--json",
    "--output-schema", schema,
    "--output-last-message", lastMessage,
    "--config", `model_reasoning_effort=${JSON.stringify(options.reasoning)}`,
    "--config", "approval_policy=\"never\"",
    "--config", "features.multi_agent=false",
    "--config", "features.apps=false",
    "--config", `features.browser_use=${REPLAY_EXECUTION_POLICY.webTools}`,
    "--config", "features.computer_use=false",
    "--config", "features.plugins=false",
    "--config", `tools.web_search=${REPLAY_EXECUTION_POLICY.webTools}`,
  ];
  if (sandbox === "workspace-write") {
    // Next.js dev tests bind loopback servers. Codex's macOS no-network
    // sandbox blocks loopback as well as external traffic, so execution
    // keeps network capability while the prompt, empty remotes, disabled
    // web tools, and retained command stream enforce/audit the boundary.
    args.push(
      "--config",
      `sandbox_workspace_write.network_access=${REPLAY_EXECUTION_POLICY.networkAccess}`,
    );
  }
  if (usesJscout) {
    args.push(
      "--config", `mcp_servers.jscout.command=${JSON.stringify(jscout)}`,
      "--config", `mcp_servers.jscout.args=${JSON.stringify(["mcp", workspace, "--database", database, "--profile", profile === "baseline" ? "baseline" : "structural", "--telemetry", telemetry, "--request-log", requestLog])}`,
      "--config", `mcp_servers.jscout.env.JSCOUT_TASK_ID=${JSON.stringify(taskId)}`,
      "--config", `mcp_servers.jscout.env.JSCOUT_SESSION_ID=${JSON.stringify(session)}`,
      "--config", `mcp_servers.jscout.env.JSCOUT_PROFILE_LABEL=${JSON.stringify(profile)}`,
      "--config", "mcp_servers.jscout.default_tools_approval_mode=\"approve\"",
    );
    if (embeddingEnvironmentForProfile(profile).JSCOUT_EMBED_PROVIDER) {
      args.push(
        "--config", "mcp_servers.jscout.env.JSCOUT_EMBED_PROVIDER=\"local\"",
      );
    }
    args.push(...jscoutMcpEnvironmentArgs(environment));
  }
  return args;
}

async function runCodexPhase({
  options,
  workspace,
  childEnvironment,
  args,
  prompt,
  eventsPath,
  stderrPath,
  phase,
  browserEndpoint,
}) {
  fs.writeFileSync(path.join(path.dirname(eventsPath), `${phase}-prompt.txt`), `${prompt}\n`);
  const processRegistry = browserEndpoint
    ? path.join(
      os.tmpdir(),
      `jscout-eval-next-processes-${process.pid}-${crypto.randomUUID()}.jsonl`,
    )
    : null;
  if (processRegistry) fs.writeFileSync(processRegistry, "");
  const result = await run(
    "/bin/sh",
    [
      "-c",
      'ulimit -n 65536 || { echo "failed to raise file-descriptor limit to 65536" >&2; exit 72; }; exec "$@"',
      "sh",
      options.codex,
      ...args,
      prompt,
    ],
    {
      cwd: workspace,
      env: {
        ...childEnvironment,
        JSCOUT_EVAL_PHASE: phase,
        ...(browserEndpoint
          ? {
            NEXT_TEST_BROWSER_WS_ENDPOINT: browserEndpoint,
            HEADLESS: "true",
            NODE_OPTIONS: nodeOptionsWithNextTeardown(childEnvironment.NODE_OPTIONS),
            JSCOUT_EVAL_PROCESS_REGISTRY: processRegistry,
          }
          : {}),
      },
      eventsPath,
      stderrPath,
      timeoutMs: Number(options[`${phase}-timeout`] ?? options["run-timeout"] ?? 1800) * 1000,
    },
  );
  if (processRegistry) {
    const preserved = path.join(path.dirname(eventsPath), `${phase}-process-registry.jsonl`);
    try {
      fs.copyFileSync(processRegistry, preserved);
    } finally {
      fs.rmSync(processRegistry, { force: true });
    }
  }
  return result;
}

export function promptFor(task, treatment = "control", options = {}) {
  const contract = [
    "You are implementing a real change in this repository.",
    "",
    `## Story`,
    task.story,
    "",
    "## Contract",
    "- Implement the change directly in the working directory; edit files as needed.",
    "- Investigate enough to cover every place the change genuinely affects.",
    "- Dependencies are already installed. You may run existing package-manager",
    "  scripts, builds, formatting, and tests, including localhost test servers.",
    "- Do not install or update dependencies and do not access external network services.",
  ];
  if (task.execution_notes?.length) {
    contract.push("- Task-specific execution constraints:");
    for (const note of task.execution_notes) contract.push(`  - ${note}`);
  }
  if (treatment === "forced") {
    contract.push(
      "- Use jscout exclusively for repository-wide code discovery, symbol lookup,",
      "  reference tracing, and architectural navigation. Do not use grep, rg,",
      "  git grep, find, IDE search, or custom repository-scanning scripts.",
      "- You may directly read files and line ranges identified by jscout, edit",
      "  files, inspect test/build output, and run tests or builds.",
    );
  }
  // Only promise a browser when one is actually there: an arm whose server
  // failed to start would otherwise be told to run e2e tests that cannot pass.
  if (options.browserEndpoint) {
    contract.push(
      "- Browser e2e tests work through a pre-connected browser endpoint: run",
      "  them with the repository's pnpm test scripts, e.g. HEADLESS=true",
      "  NEXT_SKIP_ISOLATE=1 NEXT_TEST_MODE=start pnpm testonly <path>. Prefer",
      "  this direct command",
      "  over test-start-turbo / test-dev-* wrappers so the pre-connected",
      "  endpoint and per-command teardown stay under harness control. Do not",
      "  invoke node run-tests.js directly; it replaces the browser endpoint",
      "  and will fail.",
    );
  } else {
    contract.push(
      "- No browser endpoint is available in this environment; do not attempt browser e2e tests.",
    );
  }
  contract.push(
    "- Do not start long-running dev watchers (pnpm dev, next dev, watch",
    "  builds); they exhaust file watchers on this tree and the e2e harness",
    "  manages its own servers.",
    "- Work only inside this synthetic one-commit snapshot. Do not inspect other",
    "  filesystem locations, GitHub, upstream Git history/remotes, or web/search",
    "  services. Do not fetch or clone repositories.",
    "- Finish with a JSON object matching the output schema: `answer` is a short",
    "  summary of what you changed and why; `files` lists every file you judged",
    "  relevant to the change (changed or deliberately left alone); `symbols`",
    "  lists the key functions/components involved; `inspected_files` lists",
    "  files you actually opened.",
  );
  return contract.join("\n");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const taskSet = JSON.parse(fs.readFileSync(path.resolve(options.tasks), "utf8"));
  const repository = path.resolve(options.repository);
  const runsRoot = path.resolve(options["runs-root"]);
  const jscout = path.resolve(options.jscout);
  const responses = path.resolve(options.responses);
  const telemetry = path.resolve(options.telemetry);
  const artifacts = path.resolve(options.artifacts);
  const schema = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../eval/agent-response.schema.json");
  const designSchema = path.resolve(
    path.dirname(fileURLToPath(import.meta.url)),
    "../eval/design-response.schema.json",
  );
  const gradeScript = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "eval-pr-grade.mjs");
  const profiles = options.profiles.split(",").map((value) => value.trim()).filter(Boolean);
  for (const profile of profiles) profilePlan(profile);
  const requestedTreatments = options.treatments
    .split(",")
    .map((value) => value.trim())
    .filter(Boolean);
  for (const treatment of requestedTreatments) {
    if (!["skill", "forced"].includes(treatment)) {
      throw new Error(`unknown treatment: ${treatment}`);
    }
  }
  if (!["single", "design-implement"].includes(options.workflow)) {
    throw new Error("--workflow must be single or design-implement");
  }
  const workRoot = path.resolve(options["work-root"] ?? "/tmp/jr");
  const preparedRoot = options["prepared-root"]
    ? path.resolve(options["prepared-root"])
    : path.join(artifacts, "prepared-databases");
  fs.mkdirSync(workRoot, { recursive: true });
  const resume = options.resume === "true";
  const scoutMaxCalls = String(options["scout-max-calls"] ?? "all").toLowerCase();
  const scoutMaxSubjects = String(options["scout-max-subjects"] ?? "all").toLowerCase();
  for (const [name, value] of [
    ["--scout-max-calls", scoutMaxCalls],
    ["--scout-max-subjects", scoutMaxSubjects],
  ]) {
    if (value !== "all" && (!Number.isInteger(Number(value)) || Number(value) < 1)) {
      throw new Error(`${name} must be a positive integer or all`);
    }
  }
  const memoryBudgets = {
    workflows: Number(options["memory-workflow-calls"] ?? 32),
    cards: Number(options["memory-card-calls"] ?? 64),
    summaries: Number(options["memory-summary-calls"] ?? 32),
  };
  for (const [stage, budget] of Object.entries(memoryBudgets)) {
    if (!Number.isInteger(budget) || budget < 1) {
      throw new Error(`--memory-${stage.replace(/s$/, "")}-calls must be a positive integer`);
    }
  }
  const scoutWarnSubjects = Number(options["scout-warn-subjects"] ?? 512);
  if (!Number.isInteger(scoutWarnSubjects) || scoutWarnSubjects < 1) {
    throw new Error("--scout-warn-subjects must be a positive integer");
  }
  if (!resume) {
    for (const outputPath of [responses, telemetry]) {
      if (fs.existsSync(outputPath) && fs.statSync(outputPath).size > 0) {
        throw new Error(`refusing to append to non-empty output: ${outputPath}`);
      }
    }
    if (fs.existsSync(artifacts) && fs.readdirSync(artifacts).length > 0) {
      throw new Error(`refusing to overwrite non-empty artifacts directory: ${artifacts}`);
    }
  }
  fs.mkdirSync(artifacts, { recursive: true });
  const completedSessions = resume ? completedReplaySessions(responses) : new Set();

  for (let taskIndex = 0; taskIndex < taskSet.tasks.length; taskIndex += 1) {
    const task = taskSet.tasks[taskIndex];
    const gold = path.join(runsRoot, task.id, "gold");
    for (const required of [gold]) {
      if (!fs.existsSync(required)) throw new Error(`missing snapshot directory: ${required}`);
    }
    if (!task.story) throw new Error(`task ${task.id} has no story`);
    const executionEnvironment = resolveExecutionEnvironment(taskSet, task);
    const browserPolicy = resolveBrowserServerPolicy(taskSet, task);
    const childEnvironment = { ...process.env, ...executionEnvironment };
    const goldTask = JSON.parse(fs.readFileSync(path.join(gold, "task.json"), "utf8"));
    const parent = task.parent ?? goldTask.parent;
    if (!parent) throw new Error(`task ${task.id} has no parent revision`);

    const orderedProfiles = taskIndex % 2 === 0 ? profiles : [...profiles].reverse();
    for (const profile of orderedProfiles) {
      const { usesJscout, stages } = profilePlan(profile);
      const treatments = usesJscout ? requestedTreatments : ["control"];
      const profileDatabase = usesJscout
        ? path.join(preparedRoot, task.id, `${profile}.db`)
        : null;
      const expectedProfileManifest = usesJscout
        ? preparedDatabaseManifest({ taskId: task.id, parent, profile, stages })
        : null;
      for (let treatmentIndex = 0; treatmentIndex < treatments.length; treatmentIndex += 1) {
        const treatment = treatments[treatmentIndex];
        const session = options.workflow === "single"
          ? `${profile}-${treatment}-${task.id}-${options.trial}`
          : `${profile}-${treatment}-${options.workflow}-${task.id}-${options.trial}`;
        const runDir = path.join(artifacts, session);
        if (completedSessions.has(session)) {
          process.stderr.write(`[${task.id}] ${profile}/${treatment} — already complete; skipping\n`);
          continue;
        }
        if (resume) {
          const archived = archiveInterruptedRun(runDir, workRoot);
          if (archived) {
            process.stderr.write(
              `[${task.id}] ${profile}/${treatment} — archived interrupted artifacts at ${archived}\n`,
            );
          }
        }
        const workspace = fs.mkdtempSync(path.join(workRoot, "r-"));
        activeWorkspace = workspace;
        fs.mkdirSync(runDir, { recursive: true });
        fs.writeFileSync(path.join(runDir, "workspace-path.txt"), `${workspace}\n`);
        prepareArm(
          repository,
          parent,
          workspace,
          task.setup_command ?? taskSet.setup_command,
          runDir,
          childEnvironment,
        );

        let installedSkillSha = null;
        const database = usesJscout ? path.join(runDir, "jscout.db") : null;
        if (usesJscout) {
          execFileSync(jscout, ["agent-guide", "--install", workspace], {
            stdio: ["ignore", "pipe", "pipe"],
          });
          installedSkillSha = sha256(
            path.join(workspace, ".agents", "skills", "jscout", "SKILL.md"),
          );
          execFileSync("git", ["add", "-f", ".agents/skills/jscout/SKILL.md"], {
            cwd: workspace,
          });
          gitCommitWithoutHooks(workspace, ["commit", "--amend", "--no-edit", "-q"]);
          if (sha256(path.join(workspace, ".agents", "skills", "jscout", "SKILL.md")) !== installedSkillSha) {
            throw new Error("synthetic baseline preparation changed the installed jscout skill");
          }
          if (treatmentIndex === 0 && fs.existsSync(profileDatabase)) {
            // Cross-model reuse: an already-prepared profile database (e.g.
            // from a prior trial) is cloned byte-identically instead of being
            // rebuilt, holding the retrieval substrate constant across
            // execution models. Recorded per-arm for provenance.
            const preparedManifest = requirePreparedDatabaseManifest(
              profileDatabase,
              expectedProfileManifest,
            );
            copyDatabase(profileDatabase, database);
            writePreparedDatabaseManifest(database, preparedManifest);
            fs.writeFileSync(
              path.join(runDir, "jscout-profile-reuse.json"),
              `${JSON.stringify({ source: profileDatabase, manifest: preparedManifest, reused_prepared: true }, null, 2)}\n`,
            );
          } else if (treatmentIndex === 0) {
            const baseProfile = PROFILE_BASES[profile];
            const baseDatabase = baseProfile
              ? path.join(preparedRoot, task.id, `${baseProfile}.db`)
              : null;
            if (baseDatabase && fs.existsSync(baseDatabase)) {
              requirePreparedDatabaseManifest(
                baseDatabase,
                preparedDatabaseManifest({
                  taskId: task.id,
                  parent,
                  profile: baseProfile,
                  stages: profilePlan(baseProfile).stages,
                }),
              );
            }
            prepareJscoutProfile({
              jscout,
              workspace,
              database,
              profile,
              runDir,
              scoutMaxCalls,
              scoutMaxSubjects,
              scoutWarnSubjects,
              memoryBudgets,
              baseDatabase,
              executionEnvironment,
            });
            writePreparedDatabaseManifest(database, expectedProfileManifest);
            fs.mkdirSync(path.dirname(profileDatabase), { recursive: true });
            copyDatabase(database, profileDatabase);
            writePreparedDatabaseManifest(profileDatabase, expectedProfileManifest);
          } else {
            const preparedManifest = requirePreparedDatabaseManifest(
              profileDatabase,
              expectedProfileManifest,
            );
            copyDatabase(profileDatabase, database);
            writePreparedDatabaseManifest(database, preparedManifest);
            fs.writeFileSync(
              path.join(runDir, "jscout-profile-reuse.json"),
              `${JSON.stringify({ source: profileDatabase, manifest: preparedManifest }, null, 2)}\n`,
            );
          }
        }

        const isTwoPhase = options.workflow === "design-implement";
        const lastMessage = path.join(runDir, "last-message.json");
        const implementationEventsPath = path.join(
          runDir,
          isTwoPhase ? "implementation-events.jsonl" : "events.jsonl",
        );
        const implementationStderrPath = path.join(
          runDir,
          isTwoPhase ? "implementation-stderr.log" : "stderr.log",
        );
        const implementationRequestLog = path.join(
          runDir,
          isTwoPhase ? "jscout-requests-implementation.jsonl" : "jscout-requests.jsonl",
        );
        let design = null;
        let designError = null;
        let designResult = { code: 0, timedOut: false, duration_ms: 0 };
        let designEvents = [];

        if (isTwoPhase) {
          const designLastMessage = path.join(runDir, "design-last-message.json");
          const designEventsPath = path.join(runDir, "design-events.jsonl");
          const designStderrPath = path.join(runDir, "design-stderr.log");
          const designRequestLog = path.join(runDir, "jscout-requests-design.jsonl");
          const designArgs = codexArgs({
            options,
            workspace,
            schema: designSchema,
            lastMessage: designLastMessage,
            sandbox: "read-only",
            usesJscout,
            jscout,
            database,
            profile,
            telemetry,
            taskId: task.id,
            session: `${session}-design`,
            requestLog: designRequestLog,
            environment: childEnvironment,
          });
          const designPrompt = designPromptFor(task, treatment);
          process.stderr.write(
            `[${task.id}] ${profile}/${treatment} — design events: ${designEventsPath}\n`,
          );
          designResult = await runCodexPhase({
            options,
            workspace,
            childEnvironment,
            args: designArgs,
            prompt: designPrompt,
            eventsPath: designEventsPath,
            stderrPath: designStderrPath,
            phase: "design",
          });
          recordWorkspaceCleanup(workspace, runDir, "after-design");
          if (designResult.interrupted) throwIfInterrupted();
          designEvents = jsonLines(
            fs.existsSync(designEventsPath) ? fs.readFileSync(designEventsPath, "utf8") : "",
          );
          designError = designResult.timedOut
            ? `design phase timed out after ${options["design-timeout"] ?? options["run-timeout"] ?? 1800}s`
            : designResult.code === 0
              ? null
              : `design phase codex exited ${designResult.code}`;
          try {
            design = validateDesignResponse(
              JSON.parse(fs.readFileSync(designLastMessage, "utf8")),
            );
          } catch (error) {
            designError = designError ?? `unable to parse design response: ${error.message}`;
          }
          fs.writeFileSync(
            path.join(runDir, "design-response.json"),
            `${JSON.stringify(design, null, 2)}\n`,
          );
          const designPatch = path.join(runDir, "design.patch");
          saveAgentPatch(workspace, designPatch);
          if (fs.statSync(designPatch).size > 0) {
            designError = designError ?? "design phase modified the source snapshot";
          }
        }

        let result = { code: 1, timedOut: false, interrupted: null, duration_ms: 0 };
        let browserReport = {
          started: false,
          policy: browserPolicy,
          reason: designError,
        };
        if (!designError) {
          const browserLog = path.join(runDir, "browser-server.log");
          const browserCapability = browserServerCapability(workspace, browserPolicy);
          const browser = browserCapability.start
            ? await startBrowserServer(
              workspace,
              browserLog,
              60_000,
              browserCapability.playwrightModule,
            )
            : null;
          browserReport = browser
            ? {
              started: true,
              policy: browserPolicy,
              endpoint: browser.endpoint,
              log: browserLog,
              next_teardown: {
                strategy: "registered-pgrep-process-tree",
                preload: NEXT_TEARDOWN_PRELOAD,
                registry: isTwoPhase
                  ? "implementation-process-registry.jsonl"
                  : "agent-process-registry.jsonl",
              },
            }
            : {
              started: false,
              policy: browserPolicy,
              reason: browserCapability.reason,
            };
          fs.writeFileSync(
            path.join(runDir, "browser-server.json"),
            `${JSON.stringify(browserReport, null, 2)}\n`,
          );
          const implementationArgs = codexArgs({
            options,
            workspace,
            schema,
            lastMessage,
            sandbox: "workspace-write",
            usesJscout,
            jscout,
            database,
            profile,
            telemetry,
            taskId: task.id,
            session: isTwoPhase ? `${session}-implementation` : session,
            requestLog: implementationRequestLog,
            environment: childEnvironment,
          });
          const implementationPrompt = isTwoPhase
            ? implementationPromptFor(task, treatment, design, { browserEndpoint: browser?.endpoint })
            : promptFor(task, treatment, { browserEndpoint: browser?.endpoint });
          process.stderr.write(
            `[${task.id}] ${profile}/${treatment} — implementation events: ${implementationEventsPath}\n`,
          );
          try {
            result = await runCodexPhase({
              options,
              workspace,
              childEnvironment,
              args: implementationArgs,
              prompt: implementationPrompt,
              eventsPath: implementationEventsPath,
              stderrPath: implementationStderrPath,
              phase: isTwoPhase ? "implementation" : "agent",
              browserEndpoint: browser?.endpoint,
            });
          } finally {
            await stopBrowserServer(browser?.child);
          }
        } else {
          fs.writeFileSync(
            path.join(runDir, "browser-server.json"),
            `${JSON.stringify(browserReport, null, 2)}\n`,
          );
          fs.writeFileSync(implementationEventsPath, "");
          fs.writeFileSync(implementationStderrPath, "");
        }
        recordWorkspaceCleanup(workspace, runDir, "after-agent");
        if (result.interrupted) throwIfInterrupted();

        if (usesJscout && isTwoPhase) {
          const combined = [
            path.join(runDir, "jscout-requests-design.jsonl"),
            implementationRequestLog,
          ].filter((file) => fs.existsSync(file))
            .map((file) => fs.readFileSync(file, "utf8"))
            .join("");
          fs.writeFileSync(path.join(runDir, "jscout-requests.jsonl"), combined);
        }
        if (isTwoPhase) {
          fs.writeFileSync(
            path.join(runDir, "events.jsonl"),
            [
              path.join(runDir, "design-events.jsonl"),
              implementationEventsPath,
            ].filter((file) => fs.existsSync(file))
              .map((file) => fs.readFileSync(file, "utf8"))
              .join(""),
          );
          fs.writeFileSync(
            path.join(runDir, "stderr.log"),
            [
              path.join(runDir, "design-stderr.log"),
              implementationStderrPath,
            ].filter((file) => fs.existsSync(file))
              .map((file) => fs.readFileSync(file, "utf8"))
              .join(""),
          );
        }

        const integrationError = usesJscout
          ? verifyInstalledSkill(workspace, installedSkillSha)
          : null;

        let answer = { answer: "", files: [], symbols: [], inspected_files: [] };
        let runnerError = designError ?? (result.timedOut
          ? `timed out after ${options["run-timeout"] ?? 1800}s`
          : result.code === 0
            ? null
            : `codex exited ${result.code}`);
        runnerError = runnerError ?? integrationError;
        if (!designError) {
          try {
            answer = JSON.parse(fs.readFileSync(lastMessage, "utf8"));
          } catch (error) {
            runnerError = runnerError ?? `unable to parse final response: ${error.message}`;
          }
        }
        const responsePath = path.join(runDir, "response.json");
        fs.writeFileSync(responsePath, `${JSON.stringify(answer, null, 2)}\n`);
        saveAgentPatch(workspace, path.join(runDir, "agent.patch"));

        const gradePath = path.join(runDir, "grade.json");
        if (designError) {
          fs.writeFileSync(
            path.join(runDir, "grade-skipped.json"),
            `${JSON.stringify({ reason: designError }, null, 2)}\n`,
          );
        } else {
          try {
            execFileSync(process.execPath, [
              gradeScript,
              "--workspace", workspace,
              "--gold", gold,
              "--in-place", "true",
              "--response", responsePath,
              ...(task.test_command ?? taskSet.test_command
                ? ["--test-command", task.test_command ?? taskSet.test_command]
                : []),
              "--output", gradePath,
            ], { encoding: "utf8", env: childEnvironment });
          } catch (error) {
            runnerError = runnerError ?? `grading failed: ${`${error.stderr ?? error.message}`.slice(0, 300)}`;
          }
        }
        recordWorkspaceCleanup(workspace, runDir, "after-grader");
        throwIfInterrupted();
        const gradeReport = fs.existsSync(gradePath)
          ? JSON.parse(fs.readFileSync(gradePath, "utf8"))
          : null;

        const implementationEvents = jsonLines(
          fs.existsSync(implementationEventsPath)
            ? fs.readFileSync(implementationEventsPath, "utf8")
            : "",
        );
        const designMetrics = phaseUsage(designEvents);
        const implementationMetrics = phaseUsage(implementationEvents);
        const combinedMetrics = addUsage(designMetrics, implementationMetrics);
        const requestRows = usesJscout
          ? jsonLines(
            fs.existsSync(path.join(runDir, "jscout-requests.jsonl"))
              ? fs.readFileSync(path.join(runDir, "jscout-requests.jsonl"), "utf8")
              : "",
          ).filter((entry) => entry.method === "tools/call")
          : [];
        const designRequestCount = isTwoPhase && usesJscout
          ? jsonLines(
            fs.existsSync(path.join(runDir, "jscout-requests-design.jsonl"))
              ? fs.readFileSync(path.join(runDir, "jscout-requests-design.jsonl"), "utf8")
              : "",
          ).filter((entry) => entry.method === "tools/call").length
          : 0;
        const row = {
          task_id: task.id,
          profile,
          treatment,
          workflow: options.workflow,
          session,
          telemetry_sessions: usesJscout
            ? isTwoPhase
              ? [`${session}-design`, `${session}-implementation`]
              : [session]
            : [],
          model: options.model,
          reasoning: options.reasoning,
          files: answer.files ?? [],
          symbols: answer.symbols ?? [],
          inspected_files: answer.inspected_files ?? [],
          answer: answer.answer ?? "",
          design,
          ...combinedMetrics,
          ...prefixedUsage("design", designMetrics),
          ...prefixedUsage("implementation", implementationMetrics),
          design_duration_ms: designResult.duration_ms,
          implementation_duration_ms: result.duration_ms,
          duration_ms: designResult.duration_ms + result.duration_ms,
          patched_files: gradeReport?.changes.map((change) => change.file) ?? [],
          gold_matched: gradeReport?.coverage.patched.matched.length ?? null,
          gold_pending_adjudication: gradeReport?.coverage.patched.pending_adjudication.length ?? null,
          layer1: gradeReport?.layer1?.status ?? null,
          jscout_skill_sha256: installedSkillSha,
          jscout_requests: requestRows.length,
          design_jscout_requests: designRequestCount,
          implementation_jscout_requests: requestRows.length - designRequestCount,
          execution_network_access: REPLAY_EXECUTION_POLICY.networkAccess,
          execution_network_policy: REPLAY_EXECUTION_POLICY.networkPolicy,
          execution_environment: executionEnvironment,
          browser_server_policy: browserPolicy,
          browser_server_started: browserReport.started,
        };
        if (runnerError) row.runner_error = runnerError;
        fs.appendFileSync(responses, `${JSON.stringify(row)}\n`);
        const keepWorkspace = options["keep-workspaces"] === "true";
        fs.writeFileSync(
          path.join(runDir, "workspace.json"),
          `${JSON.stringify({ path: workspace, kept: keepWorkspace }, null, 2)}\n`,
        );
        if (!keepWorkspace) fs.rmSync(workspace, { recursive: true, force: true });
        activeWorkspace = null;
      }
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error.message}\n`);
    process.exitCode = 1;
  });
}
