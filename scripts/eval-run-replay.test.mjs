import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { buildSnapshot } from "./eval-pr-snapshot.mjs";
import {
  preparedDatabaseManifest,
  REPLAY_EXECUTION_POLICY,
  embeddingEnvironmentForProfile,
  designPromptFor,
  implementationPromptFor,
  jscoutMcpEnvironmentArgs,
  nodeOptionsWithNextTeardown,
  validateDesignResponse,
  profilePlan,
  promptFor,
  startBrowserServer,
  validatePreparedDatabaseManifest,
  workspaceProcessGroups,
  scoutPublishedArtifacts,
  countSemanticArtifacts,
} from "./eval-run-replay.mjs";

test("replay forwards non-secret jscout runtime selectors into the MCP server", () => {
  assert.deepEqual(
    jscoutMcpEnvironmentArgs({
      JSCOUT_PI_AI_GATEWAY: "/opt/jscout/gateway/src/main.mjs",
      JSCOUT_LLM_MODEL: "openai-codex:gpt-5.6-terra",
      OPENAI_API_KEY: "must-not-enter-codex-config",
    }),
    [
      "--config",
      'mcp_servers.jscout.env.JSCOUT_PI_AI_GATEWAY="/opt/jscout/gateway/src/main.mjs"',
      "--config",
      'mcp_servers.jscout.env.JSCOUT_LLM_MODEL="openai-codex:gpt-5.6-terra"',
    ],
  );
});

test("Next teardown preload composes with existing NODE_OPTIONS", () => {
  const standalone = nodeOptionsWithNextTeardown();
  assert.match(standalone, /^--require=.*eval-next-teardown-preload\.cjs$/);
  assert.equal(nodeOptionsWithNextTeardown(standalone), standalone);
  assert.equal(
    nodeOptionsWithNextTeardown("--trace-warnings"),
    `--trace-warnings ${standalone}`,
  );
});

test("Next teardown preload answers pgrep from the scoped process registry", () => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-pgrep-registry-"));
  const registry = path.join(directory, "processes.jsonl");
  fs.writeFileSync(
    registry,
    [
      { kind: "process", pid: 4100, ppid: 4000, group: "launch-one" },
      { kind: "process", pid: 4101, ppid: 4100, group: "launch-one" },
      { kind: "process", pid: 5101, ppid: 5100, group: "other-launch" },
    ].map((record) => JSON.stringify(record)).join("\n") + "\n",
  );
  const preload = path.resolve(
    path.dirname(new URL(import.meta.url).pathname),
    "eval-next-teardown-preload.cjs",
  );
  const output = execFileSync(
    process.execPath,
    [
      "--require", preload,
      "-e",
      `
        const { spawn } = require("node:child_process");
        const child = spawn("pgrep", ["-P", "4100"]);
        let output = "";
        child.stdout.on("data", (chunk) => { output += chunk; });
        child.on("close", (code) => {
          if (code !== 0) process.exit(code);
          process.stdout.write(output);
        });
      `,
    ],
    {
      encoding: "utf8",
      env: { ...process.env, JSCOUT_EVAL_PROCESS_REGISTRY: registry },
    },
  );
  assert.equal(output.trim(), "4101");
  const records = fs.readFileSync(registry, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => JSON.parse(line));
  assert.ok(records.some((record) =>
    record.kind === "pgrep" &&
    record.parent_pid === 4100 &&
    JSON.stringify(record.registered_children) === "[4101]"
  ));
  fs.rmSync(directory, { recursive: true, force: true });
});

function git(repo, args) {
  return execFileSync("git", ["-C", repo, ...args], { encoding: "utf8" });
}

test("replay profiles and forced-search contract are explicit", () => {
  assert.deepEqual(profilePlan("structural"), { usesJscout: true, stages: [] });
  assert.deepEqual(profilePlan("checker"), { usesJscout: true, stages: ["enrich"] });
  assert.deepEqual(profilePlan("checker-embed"), {
    usesJscout: true,
    stages: ["enrich", "embed"],
  });
  assert.deepEqual(profilePlan("checker-scout"), {
    usesJscout: true,
    stages: ["enrich", "scout"],
  });
  assert.deepEqual(profilePlan("checker-scout-embed"), {
    usesJscout: true,
    stages: ["enrich", "scout", "embed-product"],
  });
  assert.deepEqual(profilePlan("production-order"), {
    usesJscout: true,
    stages: ["scout", "enrich", "embed-product"],
  });
  assert.deepEqual(profilePlan("memory"), {
    usesJscout: true,
    stages: ["enrich", "scout", "workflows", "cards", "summaries"],
  });
  assert.deepEqual(profilePlan("memory-embed"), {
    usesJscout: true,
    stages: [
      "enrich", "scout", "embed-product", "workflows", "cards", "summaries",
      "embed-semantic",
    ],
  });
  assert.deepEqual(embeddingEnvironmentForProfile("memory"), {});
  assert.deepEqual(embeddingEnvironmentForProfile("memory-embed"), {
    JSCOUT_EMBED_PROVIDER: "local",
  });
  assert.throws(() => profilePlan("invented"), /unknown profile/);

  const natural = promptFor({ story: "Fix it." }, "skill");
  const forced = promptFor({ story: "Fix it." }, "forced");
  assert.ok(!natural.includes("Do not use grep"));
  assert.ok(natural.includes("Dependencies are already installed"));
  assert.ok(natural.includes("localhost test servers"));
  assert.ok(natural.includes("do not access external network services"));
  assert.ok(forced.includes("Use jscout exclusively"));
  assert.ok(forced.includes("directly read files and line ranges identified by jscout"));

  const designPrompt = designPromptFor({ story: "Fix it." }, "skill");
  assert.ok(designPrompt.includes("Do not edit files"));
  assert.ok(designPrompt.includes("underlying mechanism"));
  assert.ok(!designPrompt.includes("Implement the change directly"));
  const design = {
    mechanism: "State is accepted before validation.",
    implementation_plan: ["Validate before accepting state."],
  };
  const implementationPrompt = implementationPromptFor(
    { story: "Fix it." },
    "skill",
    design,
  );
  assert.ok(implementationPrompt.includes("Implement the change directly"));
  assert.ok(implementationPrompt.includes("State is accepted before validation."));
  assert.ok(implementationPrompt.includes("Verify it against source and tests"));
  assert.throws(
    () => validateDesignResponse({ mechanism: "Only a guess." }),
    /constraints must be an array of strings/,
  );

  const constrained = promptFor({
    story: "Fix it.",
    execution_notes: ["Do not start the persistent package watcher."],
  }, "skill");
  assert.ok(constrained.includes("Task-specific execution constraints"));
  assert.ok(constrained.includes("Do not start the persistent package watcher"));

  // The e2e paragraph follows the browser server: promise an endpoint only
  // when one exists, otherwise steer the agent away from browser tests.
  const withBrowser = promptFor({ story: "Fix it." }, "skill", {
    browserEndpoint: "ws://127.0.0.1:1/token",
  });
  assert.ok(withBrowser.includes("pre-connected browser endpoint"));
  assert.ok(withBrowser.includes("NEXT_TEST_MODE=start pnpm testonly <path>"));
  assert.ok(withBrowser.includes("test-start-turbo / test-dev-* wrappers"));
  assert.ok(!withBrowser.includes("No browser endpoint is available"));
  assert.ok(!natural.includes("pre-connected browser endpoint"));
  assert.ok(natural.includes(
    "No browser endpoint is available in this environment; do not attempt browser e2e tests.",
  ));
  assert.deepEqual(REPLAY_EXECUTION_POLICY, {
    networkAccess: true,
    networkPolicy: "prompt-restricted-external; loopback-required",
    webTools: false,
  });
});

test("prepared database manifests reject a different source snapshot", () => {
  const expected = preparedDatabaseManifest({
    taskId: "task-one",
    parent: "parent-one",
    profile: "checker-embed",
    stages: ["enrich", "embed"],
  });
  assert.doesNotThrow(() => validatePreparedDatabaseManifest(expected, expected));
  assert.throws(
    () => validatePreparedDatabaseManifest({ ...expected, parent: "parent-two" }, expected),
    /manifest mismatch for parent/,
  );
});

test("browser server startup fails instead of dispatching a blind arm", async () => {
  const workspace = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-browser-missing-"));
  const log = path.join(workspace, "browser-server.log");
  await assert.rejects(
    startBrowserServer(workspace, log, 2_000),
    /browser server exited before publishing an endpoint/,
  );
  assert.ok(fs.existsSync(log));
  fs.rmSync(workspace, { recursive: true, force: true });
});

test("workspace process cleanup selects only exact workspace-rooted groups", () => {
  const rows = [
    "  101   90 node /private/tmp/jr/r-one/node_modules/jest/bin/jest.js",
    "  102   90 next-server",
    "  201  200 node /private/tmp/jr/r-two/node_modules/jest/bin/jest.js",
    "  301  300 node /private/tmp/jr/r-one-other/tool.js",
  ].join("\n");
  assert.deepEqual(workspaceProcessGroups(rows, "/private/tmp/jr/r-one"), [{
    pgid: 90,
    pids: [101],
    commands: ["node /private/tmp/jr/r-one/node_modules/jest/bin/jest.js"],
  }]);
});

// End-to-end: fixture repo -> snapshot -> stubbed agent edits one gold file
// -> runner grades the workspace and records patched vs pending adjudication.
test("replay runner drives workspace, stub agent, and grading end to end", () => {
  const base = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-replay-runner-"));
  const repo = path.join(base, "repo");
  fs.mkdirSync(path.join(repo, "src"), { recursive: true });
  fs.mkdirSync(path.join(repo, "node_modules/playwright"), { recursive: true });
  git(base, ["init", "-q", "-b", "main", repo]);
  git(repo, ["config", "user.email", "eval@test"]);
  git(repo, ["config", "user.name", "eval"]);
  fs.writeFileSync(path.join(repo, "src/a.js"), "export const a = () => 1;\n");
  fs.writeFileSync(path.join(repo, "src/b.js"), "export const b = () => 2;\n");
  fs.writeFileSync(
    path.join(repo, "node_modules/playwright/index.js"),
    `exports.chromium = {
  async launchServer() {
    const keepAlive = setInterval(() => {}, 1_000);
    return {
      wsEndpoint() { return "ws://127.0.0.1:43210/fake-browser"; },
      async close() {
        clearInterval(keepAlive);
        console.log("FAKE_BROWSER_SERVER_CLOSED");
      },
    };
  },
};
`,
  );
  git(repo, ["add", "."]);
  git(repo, ["add", "-f", "node_modules/playwright/index.js"]);
  git(repo, ["commit", "-qm", "base"]);
  fs.writeFileSync(path.join(repo, "src/a.js"), "export const a = (x) => x + 1;\n");
  fs.writeFileSync(path.join(repo, "src/b.js"), "export const b = (x) => x + 2;\n");
  git(repo, ["add", "."]);
  git(repo, ["commit", "-qm", "feat: increment"]);
  const sha = git(repo, ["rev-parse", "HEAD"]).trim();

  const runsRoot = path.join(base, "runs");
  buildSnapshot({
    repository: repo,
    sha,
    workspace: path.join(runsRoot, "replay-fixture", "pristine"),
    gold: path.join(runsRoot, "replay-fixture", "gold"),
  });

  const tasksFile = path.join(base, "tasks.json");
  fs.writeFileSync(
    tasksFile,
    JSON.stringify({
      execution_environment: { JSCOUT_REPLAY_TEST_ENV: "enabled" },
      tasks: [{ id: "replay-fixture", sha, story: "Inputs should be incremented." }],
    }),
  );

  // Stub agent: edits src/a.js in the workspace (--cd) and writes the final
  // message. It never sees the gold directory.
  const stub = path.join(base, "stub-agent.mjs");
  fs.writeFileSync(
    stub,
    `#!/usr/bin/env node
import fs from "node:fs";
import path from "node:path";
import { execFileSync } from "node:child_process";
const argv = process.argv.slice(2);
const value = (flag) => argv[argv.indexOf(flag) + 1];
const workspace = value("--cd");
if (process.env.JSCOUT_REPLAY_TEST_ENV !== "enabled") throw new Error("execution environment missing");
if (process.env.JSCOUT_EVAL_PHASE !== "design") {
  if (process.env.NEXT_TEST_BROWSER_WS_ENDPOINT !== "ws://127.0.0.1:43210/fake-browser") throw new Error("browser endpoint missing");
  if (process.env.HEADLESS !== "true") throw new Error("headless browser mode missing");
  if (!process.env.NODE_OPTIONS?.includes("eval-next-teardown-preload.cjs")) throw new Error("Next teardown preload missing");
  if (!process.env.JSCOUT_EVAL_PROCESS_REGISTRY) throw new Error("Next process registry missing");
}
const fdLimit = Number(execFileSync("/bin/sh", ["-c", "ulimit -n"], { encoding: "utf8" }).trim());
if (fdLimit < 65536) throw new Error("file descriptor limit was not inherited");
fs.writeFileSync(path.join(path.dirname(value("--output-last-message")), "agent-argv.json"), JSON.stringify(argv));
if (fs.existsSync(path.join(workspace, "gold"))) throw new Error("gold leaked into workspace");
if (process.env.JSCOUT_EVAL_PHASE === "design") {
  if (value("--sandbox") !== "read-only") throw new Error("design sandbox is not read-only");
  fs.writeFileSync(value("--output-last-message"), JSON.stringify({
    mechanism: "Both a and b participate in the increment behavior.",
    constraints: ["Preserve exports."],
    implementation_plan: ["Update a and inspect b."],
    files: ["src/a.js", "src/b.js"],
    symbols: ["a", "b"],
    validation_plan: ["Run the fixture tests."],
    uncertainties: [],
  }));
  console.log(JSON.stringify({ usage: { input_tokens: 7, output_tokens: 3 } }));
  process.exit(0);
}
fs.appendFileSync(path.join(workspace, "src/a.js"), "// stub edit\\n");
fs.writeFileSync(value("--output-last-message"), JSON.stringify({
  answer: "incremented a", files: ["src/a.js", "src/b.js"], symbols: ["a"], inspected_files: ["src/a.js"],
}));
console.log(JSON.stringify({ usage: { input_tokens: 10, output_tokens: 5 } }));
`,
  );
  fs.chmodSync(stub, 0o755);

  const responses = path.join(base, "responses.jsonl");
  const runner = path.resolve(path.dirname(new URL(import.meta.url).pathname), "eval-run-replay.mjs");
  execFileSync(process.execPath, [
    runner,
    "--tasks", tasksFile,
    "--repository", repo,
    "--runs-root", runsRoot,
    "--jscout", "/bin/true",
    "--responses", responses,
    "--telemetry", path.join(base, "telemetry.jsonl"),
    "--artifacts", path.join(base, "artifacts"),
    "--codex", stub,
    "--profiles", "grep",
    "--trial", "t1",
  ], { encoding: "utf8" });

  const rows = fs.readFileSync(responses, "utf8").split("\n").filter(Boolean).map((line) => JSON.parse(line));
  assert.equal(rows.length, 1);
  const row = rows[0];
  assert.equal(row.task_id, "replay-fixture");
  assert.equal(row.treatment, "control");
  assert.equal(row.model, "gpt-5.6-terra");
  assert.deepEqual(row.patched_files, ["src/a.js"]);
  assert.equal(row.gold_matched, 1);
  assert.equal(row.gold_pending_adjudication, 1, "src/b.js must be pending adjudication");
  assert.equal(row.total_tokens, 15);
  assert.equal(row.input_tokens, 10);
  assert.equal(row.cached_input_tokens, 0);
  assert.equal(row.noncached_input_tokens, 10);
  assert.equal(row.output_tokens, 5);
  assert.equal(row.command_calls, 0);
  assert.equal(row.failed_command_calls, 0);
  assert.equal(row.command_output_bytes, 0);
  assert.equal(row.max_command_output_bytes, 0);
  assert.equal(row.execution_network_access, true);
  assert.equal(
    row.execution_network_policy,
    "prompt-restricted-external; loopback-required",
  );
  assert.deepEqual(row.execution_environment, { JSCOUT_REPLAY_TEST_ENV: "enabled" });
  assert.equal(row.runner_error, undefined);

  const agentArgv = JSON.parse(
    fs.readFileSync(path.join(base, "artifacts", "grep-control-replay-fixture-t1", "agent-argv.json"), "utf8"),
  );
  assert.ok(agentArgv.includes("sandbox_workspace_write.network_access=true"));

  const runDir = path.join(base, "artifacts", "grep-control-replay-fixture-t1");
  const browser = JSON.parse(
    fs.readFileSync(path.join(runDir, "browser-server.json"), "utf8"),
  );
  assert.equal(browser.endpoint, "ws://127.0.0.1:43210/fake-browser");
  assert.equal(browser.next_teardown.strategy, "registered-pgrep-process-tree");
  assert.match(browser.next_teardown.preload, /eval-next-teardown-preload\.cjs$/);
  assert.equal(browser.next_teardown.registry, "agent-process-registry.jsonl");
  assert.ok(fs.existsSync(path.join(runDir, browser.next_teardown.registry)));
  assert.match(fs.readFileSync(browser.log, "utf8"), /FAKE_BROWSER_SERVER_CLOSED/);

  const grade = JSON.parse(
    fs.readFileSync(path.join(base, "artifacts", "grep-control-replay-fixture-t1", "grade.json"), "utf8"),
  );
  // Plan mentioned both files; patched metrics must not be inflated by that.
  assert.deepEqual(grade.coverage.patched.matched, ["src/a.js"]);
  assert.deepEqual(grade.coverage.patched.pending_adjudication, ["src/b.js"]);
  assert.equal(grade.coverage.plan_mentioned.matched.length, 2);
  assert.equal(grade.coverage.patched.confirmed_omission_rate, null);

  execFileSync(process.execPath, [
    runner,
    "--tasks", tasksFile,
    "--repository", repo,
    "--runs-root", runsRoot,
    "--jscout", "/bin/true",
    "--responses", responses,
    "--telemetry", path.join(base, "telemetry.jsonl"),
    "--artifacts", path.join(base, "artifacts"),
    "--codex", stub,
    "--profiles", "grep",
    "--trial", "t1",
    "--resume", "true",
  ], { encoding: "utf8" });
  assert.equal(
    fs.readFileSync(responses, "utf8").split("\n").filter(Boolean).length,
    1,
    "resume must skip a session with a completed response row",
  );

  const twoPhaseResponses = path.join(base, "two-phase-responses.jsonl");
  const twoPhaseArtifacts = path.join(base, "two-phase-artifacts");
  execFileSync(process.execPath, [
    runner,
    "--tasks", tasksFile,
    "--repository", repo,
    "--runs-root", runsRoot,
    "--jscout", "/bin/true",
    "--responses", twoPhaseResponses,
    "--telemetry", path.join(base, "two-phase-telemetry.jsonl"),
    "--artifacts", twoPhaseArtifacts,
    "--codex", stub,
    "--profiles", "grep",
    "--trial", "t2",
    "--workflow", "design-implement",
  ], { encoding: "utf8" });
  const twoPhaseRow = JSON.parse(fs.readFileSync(twoPhaseResponses, "utf8").trim());
  assert.equal(twoPhaseRow.workflow, "design-implement");
  assert.equal(twoPhaseRow.session, "grep-control-design-implement-replay-fixture-t2");
  assert.equal(twoPhaseRow.design.mechanism, "Both a and b participate in the increment behavior.");
  assert.equal(twoPhaseRow.design_total_tokens, 10);
  assert.equal(twoPhaseRow.implementation_total_tokens, 15);
  assert.equal(twoPhaseRow.total_tokens, 25);
  assert.equal(twoPhaseRow.design_duration_ms > 0, true);
  assert.equal(twoPhaseRow.implementation_duration_ms > 0, true);
  const twoPhaseRunDir = path.join(
    twoPhaseArtifacts,
    "grep-control-design-implement-replay-fixture-t2",
  );
  assert.equal(fs.statSync(path.join(twoPhaseRunDir, "design.patch")).size, 0);
  assert.match(
    fs.readFileSync(path.join(twoPhaseRunDir, "design-prompt.txt"), "utf8"),
    /Do not edit files/,
  );
  assert.match(
    fs.readFileSync(path.join(twoPhaseRunDir, "implementation-prompt.txt"), "utf8"),
    /Both a and b participate in the increment behavior/,
  );
  assert.equal(
    fs.readFileSync(path.join(twoPhaseRunDir, "events.jsonl"), "utf8")
      .split("\n").filter(Boolean).length,
    2,
  );

  fs.rmSync(base, { recursive: true, force: true });
});


test("failed-only scout output is not mistaken for published artifacts", () => {
  const allFailed = [
    "  failed: submission failed claim-level card validation",
    "model calls: 64; reports: 64; failed subjects: 64; skipped by call budget: 0",
    "Error: 64 of 64 scouting subject(s) failed; see the report above",
  ].join("\n");
  assert.equal(scoutPublishedArtifacts(allFailed), false);

  const published = [
    "  defining: 1",
    "  artifact: 24",
    "model calls: 64; reports: 64; failed subjects: 1",
  ].join("\n");
  assert.equal(scoutPublishedArtifacts(published), true);
});

test("database artifact count detects hard-abort publication growth", () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "jr-tolerance-"));
  const database = path.join(dir, "stage.db");
  execFileSync("sqlite3", [database,
    "CREATE TABLE semantic_artifacts(id INTEGER PRIMARY KEY, artifact_type TEXT);"]);
  assert.equal(countSemanticArtifacts(database), 0);
  execFileSync("sqlite3", [database,
    "INSERT INTO semantic_artifacts(artifact_type) VALUES('card'),('workflow');"]);
  assert.equal(countSemanticArtifacts(database), 2);
  assert.equal(countSemanticArtifacts(path.join(dir, "missing.db")), null);
  fs.rmSync(dir, { recursive: true, force: true });
});
