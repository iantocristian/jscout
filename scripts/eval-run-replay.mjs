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
//   4. codex exec with --sandbox workspace-write and the story prompt;
//   5. save the agent patch, overlay hidden tests, and grade.
//
// The gold bundle stays outside every workspace and is never mounted.

import { execFileSync, spawn, spawnSync } from "node:child_process";
import crypto from "node:crypto";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

export const REPLAY_EXECUTION_POLICY = Object.freeze({
  networkAccess: true,
  networkPolicy: "prompt-restricted-external; loopback-required",
  webTools: false,
});

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
}) {
  const result = spawnSync(command, args, {
    cwd,
    env,
    encoding: "utf8",
    maxBuffer,
    stdio: ["ignore", "pipe", "pipe"],
  });
  fs.writeFileSync(log, `${result.stdout ?? ""}${result.stderr ?? ""}`);
  if (result.error || result.status !== 0) {
    throw new Error(
      `${command} ${args.join(" ")} failed (${result.status ?? result.error?.message ?? "unknown"}); see ${log}`,
    );
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
  // The memory plane: generative workflow/card/summary artifacts persisted
  // into the semantic tables before any arm runs. Base checker-scout so the
  // artifacts form over checker facts and reconnaissance policy.
  memory: ["enrich", "scout", "workflows", "cards", "summaries"],
});

const PROFILE_BASES = Object.freeze({
  checker: "structural",
  "checker-embed": "checker",
  "checker-scout": "checker",
  "checker-scout-embed": "checker-scout",
  memory: "checker-scout",
});

const PROFILE_INCREMENT = Object.freeze({
  checker: "enrich",
  "checker-embed": "embed",
  "checker-scout": "scout",
  "checker-scout-embed": "embed-product",
  memory: ["workflows", "cards", "summaries"],
});

export function profilePlan(profile) {
  if (profile === "grep") return { usesJscout: false, stages: [] };
  const stages = PROFILE_PLANS[profile];
  if (!stages) throw new Error(`unknown profile: ${profile}`);
  return { usesJscout: true, stages };
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
    ...(plan.stages.some((stage) => stage.startsWith("embed"))
      ? { JSCOUT_EMBED_PROVIDER: "local" }
      : {}),
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
        },
      );
    } else if (stage === "embed" || stage === "embed-product") {
      runLogged(
        jscout,
        [
          "embed", workspace,
          "--database", database,
          ...(stage === "embed-product" ? ["--product"] : []),
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
export function startBrowserServer(workspace, logPath, timeoutMs = 60_000) {
  return new Promise((resolve, reject) => {
    const log = fs.createWriteStream(logPath, { flags: "a" });
    const child = spawn(process.execPath, ["-e", `
      const { chromium } = require('playwright');
      chromium.launchServer({ host: '127.0.0.1', headless: true }).then((server) => {
        console.log('WS_ENDPOINT=' + server.wsEndpoint());
        process.on('SIGTERM', () => server.close().then(() => process.exit(0)));
      }).catch((error) => { console.error(error); process.exit(1); });
    `], { cwd: workspace, stdio: ["ignore", "pipe", "pipe"] });
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
      "  NEXT_TEST_MODE=start pnpm testonly <path>. Avoid the pnpm",
      "  test-start-turbo / test-dev-* wrappers; their teardown can hang for",
      "  120s per run. Do not invoke node run-tests.js directly; it replaces",
      "  the browser endpoint and will fail.",
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
        const session = `${profile}-${treatment}-${task.id}-${options.trial}`;
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
          const database = path.join(runDir, "jscout.db");
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

        const lastMessage = path.join(runDir, "last-message.json");
        const args = [
          "exec",
          "--ignore-user-config",
          "--ephemeral",
          "--skip-git-repo-check",
          "--sandbox", "workspace-write",
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
          // Next.js dev tests bind loopback servers. Codex's macOS no-network
          // sandbox blocks loopback as well as external traffic, so execution
          // keeps network capability while the prompt, empty remotes, disabled
          // web tools, and retained command stream enforce/audit the boundary.
          "--config", `sandbox_workspace_write.network_access=${REPLAY_EXECUTION_POLICY.networkAccess}`,
        ];
        if (usesJscout) {
          const requestLog = path.join(runDir, "jscout-requests.jsonl");
          args.push(
            "--config", `mcp_servers.jscout.command=${JSON.stringify(jscout)}`,
            "--config", `mcp_servers.jscout.args=${JSON.stringify(["mcp", workspace, "--database", path.join(runDir, "jscout.db"), "--profile", profile === "baseline" ? "baseline" : "structural", "--telemetry", telemetry, "--request-log", requestLog])}`,
            "--config", `mcp_servers.jscout.env.JSCOUT_TASK_ID=${JSON.stringify(task.id)}`,
            "--config", `mcp_servers.jscout.env.JSCOUT_SESSION_ID=${JSON.stringify(session)}`,
            "--config", `mcp_servers.jscout.env.JSCOUT_PROFILE_LABEL=${JSON.stringify(profile)}`,
            "--config", "mcp_servers.jscout.default_tools_approval_mode=\"approve\"",
          );
          if (stages.some((stage) => stage.startsWith("embed"))) {
            args.push(
              "--config", "mcp_servers.jscout.env.JSCOUT_EMBED_PROVIDER=\"local\"",
            );
          }
        }
        const eventsPath = path.join(runDir, "events.jsonl");
        const stderrPath = path.join(runDir, "stderr.log");
        const browserLog = path.join(runDir, "browser-server.log");
        const browser = await startBrowserServer(workspace, browserLog);
        args.push(promptFor(task, treatment, { browserEndpoint: browser.endpoint }));
        fs.writeFileSync(
          path.join(runDir, "browser-server.json"),
          `${JSON.stringify({ endpoint: browser.endpoint, log: browserLog }, null, 2)}\n`,
        );
        process.stderr.write(`[${task.id}] ${profile}/${treatment} — live events: ${eventsPath}\n`);
        // sh wrapper raises the fd limit for codex and every inner agent
        // shell (bug-1 EMFILE); "$@" passes args through without re-quoting.
        let result;
        try {
          result = await run(
            "/bin/sh",
            [
              "-c",
              'ulimit -n 65536 || { echo "failed to raise file-descriptor limit to 65536" >&2; exit 72; }; exec "$@"',
              "sh",
              options.codex,
              ...args,
            ],
            {
              cwd: workspace,
              env: {
                ...childEnvironment,
                NEXT_TEST_BROWSER_WS_ENDPOINT: browser.endpoint,
                HEADLESS: "true",
              },
              eventsPath,
              stderrPath,
              timeoutMs: Number(options["run-timeout"] ?? 1800) * 1000,
            },
          );
        } finally {
          await stopBrowserServer(browser.child);
        }
        recordWorkspaceCleanup(workspace, runDir, "after-agent");
        if (result.interrupted) throwIfInterrupted();

        const integrationError = usesJscout
          ? verifyInstalledSkill(workspace, installedSkillSha)
          : null;

        let answer = { answer: "", files: [], symbols: [], inspected_files: [] };
        let runnerError = result.timedOut
          ? `timed out after ${options["run-timeout"] ?? 1800}s`
          : result.code === 0
            ? null
            : `codex exited ${result.code}`;
        runnerError = runnerError ?? integrationError;
        try {
          answer = JSON.parse(fs.readFileSync(lastMessage, "utf8"));
        } catch (error) {
          runnerError = runnerError ?? `unable to parse final response: ${error.message}`;
        }
        const responsePath = path.join(runDir, "response.json");
        fs.writeFileSync(responsePath, `${JSON.stringify(answer, null, 2)}\n`);
        saveAgentPatch(workspace, path.join(runDir, "agent.patch"));

        const gradePath = path.join(runDir, "grade.json");
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
        recordWorkspaceCleanup(workspace, runDir, "after-grader");
        throwIfInterrupted();
        const gradeReport = fs.existsSync(gradePath)
          ? JSON.parse(fs.readFileSync(gradePath, "utf8"))
          : null;

        const events = jsonLines(
          fs.existsSync(eventsPath) ? fs.readFileSync(eventsPath, "utf8") : "",
        );
        const row = {
          task_id: task.id,
          profile,
          treatment,
          session,
          model: options.model,
          reasoning: options.reasoning,
          files: answer.files ?? [],
          symbols: answer.symbols ?? [],
          inspected_files: answer.inspected_files ?? [],
          answer: answer.answer ?? "",
          ...tokenUsage(events),
          ...commandUsage(events),
          duration_ms: result.duration_ms,
          patched_files: gradeReport?.changes.map((change) => change.file) ?? [],
          gold_matched: gradeReport?.coverage.patched.matched.length ?? null,
          gold_pending_adjudication: gradeReport?.coverage.patched.pending_adjudication.length ?? null,
          layer1: gradeReport?.layer1?.status ?? null,
          jscout_skill_sha256: installedSkillSha,
          jscout_requests: usesJscout
            ? jsonLines(
              fs.existsSync(path.join(runDir, "jscout-requests.jsonl"))
                ? fs.readFileSync(path.join(runDir, "jscout-requests.jsonl"), "utf8")
                : "",
            ).filter((entry) => entry.method === "tools/call").length
            : 0,
          execution_network_access: REPLAY_EXECUTION_POLICY.networkAccess,
          execution_network_policy: REPLAY_EXECUTION_POLICY.networkPolicy,
          execution_environment: executionEnvironment,
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
