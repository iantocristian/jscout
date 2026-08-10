#!/usr/bin/env node

// PR-replay runner: implement-a-real-change tasks in history-free workspaces.
//
// Separate from eval-run-codex.mjs (read-only localization) on purpose: replay
// runs need write access, per-run workspace copies, per-arm indexing, and
// patch grading. One runner per suite type, same conventions.
//
// Per (task × profile × trial):
//   1. copy <runs-root>/<task>/pristine -> per-run workspace;
//   2. indexed arms: `jscout index <workspace>` and mount the MCP server;
//   3. codex exec with --sandbox workspace-write and the story prompt;
//   4. tree-diff grade via eval-pr-grade.mjs (adjudicate-first discipline).
//
// The gold bundle stays outside every workspace and is never mounted.

import { execFileSync, spawn } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

function parseArgs(argv) {
  const options = {
    codex: "codex",
    model: "gpt-5.6-terra",
    reasoning: "high",
    profiles: "grep,structural",
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
  for (const required of ["tasks", "runs-root", "jscout", "responses", "telemetry", "artifacts"]) {
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
    const child = spawn(command, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
    const out = fs.createWriteStream(eventsPath);
    const err = fs.createWriteStream(stderrPath);
    child.stdout.pipe(out);
    child.stderr.pipe(err);
    let timedOut = false;
    const timer = timeoutMs
      ? setTimeout(() => {
        timedOut = true;
        child.kill("SIGKILL");
      }, timeoutMs)
      : null;
    child.on("close", (code) => {
      if (timer) clearTimeout(timer);
      resolve({ code: code ?? 1, timedOut, duration_ms: Date.now() - started });
    });
    child.on("error", () => {
      if (timer) clearTimeout(timer);
      resolve({ code: 127, timedOut, duration_ms: Date.now() - started });
    });
  });
}

function jsonLines(text) {
  return text.split(/\r?\n/).filter(Boolean).flatMap((line) => {
    try { return [JSON.parse(line)]; } catch { return []; }
  });
}

function tokenUsage(events) {
  let input = 0;
  let output = 0;
  for (const event of events) {
    const usage = event.usage ?? event.payload?.usage;
    if (usage) {
      input += Number(usage.input_tokens ?? 0);
      output += Number(usage.output_tokens ?? 0);
    }
  }
  return { total_tokens: input + output };
}

function promptFor(task) {
  return [
    "You are implementing a real change in this repository.",
    "",
    `## Story`,
    task.story,
    "",
    "## Contract",
    "- Implement the change directly in the working directory; edit files as needed.",
    "- Investigate enough to cover every place the change genuinely affects.",
    "- Do not run package installs; work with the code as-is.",
    "- Finish with a JSON object matching the output schema: `answer` is a short",
    "  summary of what you changed and why; `files` lists every file you judged",
    "  relevant to the change (changed or deliberately left alone); `symbols`",
    "  lists the key functions/components involved; `inspected_files` lists",
    "  files you actually opened.",
  ].join("\n");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const taskSet = JSON.parse(fs.readFileSync(path.resolve(options.tasks), "utf8"));
  const runsRoot = path.resolve(options["runs-root"]);
  const jscout = path.resolve(options.jscout);
  const responses = path.resolve(options.responses);
  const telemetry = path.resolve(options.telemetry);
  const artifacts = path.resolve(options.artifacts);
  const schema = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../eval/agent-response.schema.json");
  const gradeScript = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "eval-pr-grade.mjs");
  const profiles = options.profiles.split(",").map((value) => value.trim()).filter(Boolean);
  for (const profile of profiles) {
    if (!["grep", "baseline", "structural"].includes(profile)) {
      throw new Error(`unknown profile: ${profile}`);
    }
  }
  for (const outputPath of [responses, telemetry]) {
    if (fs.existsSync(outputPath) && fs.statSync(outputPath).size > 0) {
      throw new Error(`refusing to append to non-empty output: ${outputPath}`);
    }
  }
  if (fs.existsSync(artifacts) && fs.readdirSync(artifacts).length > 0) {
    throw new Error(`refusing to overwrite non-empty artifacts directory: ${artifacts}`);
  }
  fs.mkdirSync(artifacts, { recursive: true });

  for (let taskIndex = 0; taskIndex < taskSet.tasks.length; taskIndex += 1) {
    const task = taskSet.tasks[taskIndex];
    const pristine = path.join(runsRoot, task.id, "pristine");
    const gold = path.join(runsRoot, task.id, "gold");
    for (const required of [pristine, gold]) {
      if (!fs.existsSync(required)) throw new Error(`missing snapshot directory: ${required}`);
    }
    if (!task.story) throw new Error(`task ${task.id} has no story`);

    const orderedProfiles = taskIndex % 2 === 0 ? profiles : [...profiles].reverse();
    for (const profile of orderedProfiles) {
      const session = `${profile}-${task.id}-${options.trial}`;
      const runDir = path.join(artifacts, session);
      const workspace = path.join(runDir, "workspace");
      fs.mkdirSync(runDir, { recursive: true });
      fs.cpSync(pristine, workspace, { recursive: true });

      const usesJscout = profile !== "grep";
      if (usesJscout) {
        execFileSync(jscout, ["index", workspace], { stdio: ["ignore", "pipe", "pipe"] });
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
        "--config", "features.browser_use=false",
        "--config", "features.computer_use=false",
        "--config", "features.plugins=false",
        "--config", "tools.web_search=false",
      ];
      if (usesJscout) {
        args.push(
          "--config", `mcp_servers.jscout.command=${JSON.stringify(jscout)}`,
          "--config", `mcp_servers.jscout.args=${JSON.stringify(["mcp", workspace, "--profile", profile === "baseline" ? "baseline" : "structural", "--telemetry", telemetry])}`,
          "--config", `mcp_servers.jscout.env.JSCOUT_TASK_ID=${JSON.stringify(task.id)}`,
          "--config", `mcp_servers.jscout.env.JSCOUT_SESSION_ID=${JSON.stringify(session)}`,
          "--config", `mcp_servers.jscout.env.JSCOUT_PROFILE_LABEL=${JSON.stringify(profile)}`,
          "--config", "mcp_servers.jscout.default_tools_approval_mode=\"approve\"",
        );
      }
      args.push(promptFor(task));

      const eventsPath = path.join(runDir, "events.jsonl");
      const stderrPath = path.join(runDir, "stderr.log");
      process.stderr.write(`[${task.id}] ${profile} — live events: ${eventsPath}\n`);
      const result = await run(options.codex, args, {
        cwd: workspace,
        eventsPath,
        stderrPath,
        timeoutMs: Number(options["run-timeout"] ?? 1800) * 1000,
      });

      let answer = { answer: "", files: [], symbols: [], inspected_files: [] };
      let runnerError = result.timedOut
        ? `timed out after ${options["run-timeout"] ?? 1800}s`
        : result.code === 0
          ? null
          : `codex exited ${result.code}`;
      try {
        answer = JSON.parse(fs.readFileSync(lastMessage, "utf8"));
      } catch (error) {
        runnerError = runnerError ?? `unable to parse final response: ${error.message}`;
      }
      const responsePath = path.join(runDir, "response.json");
      fs.writeFileSync(responsePath, `${JSON.stringify(answer, null, 2)}\n`);

      const gradePath = path.join(runDir, "grade.json");
      try {
        execFileSync(process.execPath, [
          gradeScript,
          "--pristine", pristine,
          "--workspace", workspace,
          "--gold", gold,
          "--response", responsePath,
          "--output", gradePath,
        ], { encoding: "utf8" });
      } catch (error) {
        runnerError = runnerError ?? `grading failed: ${`${error.stderr ?? error.message}`.slice(0, 300)}`;
      }
      const gradeReport = fs.existsSync(gradePath)
        ? JSON.parse(fs.readFileSync(gradePath, "utf8"))
        : null;

      const row = {
        task_id: task.id,
        profile,
        session,
        model: options.model,
        reasoning: options.reasoning,
        files: answer.files ?? [],
        symbols: answer.symbols ?? [],
        inspected_files: answer.inspected_files ?? [],
        answer: answer.answer ?? "",
        ...tokenUsage(jsonLines(fs.existsSync(eventsPath) ? fs.readFileSync(eventsPath, "utf8") : "")),
        duration_ms: result.duration_ms,
        patched_files: gradeReport?.changes.map((change) => change.file) ?? [],
        gold_matched: gradeReport?.coverage.patched.matched.length ?? null,
        gold_pending_adjudication: gradeReport?.coverage.patched.pending_adjudication.length ?? null,
        layer1: gradeReport?.layer1?.status ?? null,
      };
      if (runnerError) row.runner_error = runnerError;
      fs.appendFileSync(responses, `${JSON.stringify(row)}\n`);
    }
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error.message}\n`);
    process.exitCode = 1;
  });
}
