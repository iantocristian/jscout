#!/usr/bin/env node

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

function parseArgs(argv) {
  const options = {
    codex: "codex",
    model: "gpt-5.6-terra",
    reasoning: "high",
    profiles: "baseline,structural",
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
  for (const required of ["tasks", "repository", "jscout", "responses", "telemetry", "artifacts"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  return options;
}

function run(command, args, { cwd, env }) {
  return new Promise((resolve) => {
    const started = Date.now();
    const child = spawn(command, args, { cwd, env, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", (error) => resolve({ code: null, stdout, stderr, error, duration_ms: Date.now() - started }));
    child.on("close", (code) => resolve({ code, stdout, stderr, error: null, duration_ms: Date.now() - started }));
  });
}

function jsonLines(text) {
  return text.split(/\r?\n/).filter(Boolean).flatMap((line) => {
    try { return [JSON.parse(line)]; } catch { return []; }
  });
}

function tokenUsage(events) {
  let input = 0;
  let cached = 0;
  let output = 0;
  for (const event of events) {
    const usage = event.usage ?? event.token_usage ?? event.data?.usage;
    if (!usage) continue;
    input = Math.max(input, Number(usage.input_tokens ?? usage.inputTokens ?? 0));
    cached = Math.max(cached, Number(usage.cached_input_tokens ?? usage.cachedInputTokens ?? 0));
    output = Math.max(output, Number(usage.output_tokens ?? usage.outputTokens ?? 0));
  }
  return { input_tokens: input, cached_input_tokens: cached, output_tokens: output, total_tokens: input + output };
}

function sameSet(left = [], right = []) {
  const a = new Set(left);
  const b = new Set(right);
  return a.size === b.size && [...a].every((value) => b.has(value));
}

function promptFor(task, { profile, requireJscout = false, requireDefinition = false } = {}) {
  const lines = [
    "You are completing a read-only repository-localization evaluation.",
    "Answer only from the checked-out repository. Do not edit files or run tests.",
    "Use any available repository tools when helpful; no particular tool is required.",
    "In `files` and `symbols`, include only the production locations and named symbols that directly answer the question.",
    "In `inspected_files`, include every repository file whose contents you read directly or through a tool-provided snippet.",
    "Use repository-relative POSIX file paths and bare symbol names.",
  ];
  if (profile === "grep") {
    lines.push("Use repository-local shell and filesystem search; this arm has no repository index configured.");
  } else if (requireJscout) {
    lines.push("Start with the configured jscout MCP server and use it as the primary localization interface; verify decisive claims in source before answering.");
  }
  if (requireDefinition && profile !== "grep") {
    lines.push("Before answering, call jscout definition for every named symbol in your final answer; use the returned source as the verification evidence.");
  }
  lines.push("", task.prompt);
  return lines.join("\n");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const taskSet = JSON.parse(fs.readFileSync(options.tasks, "utf8"));
  let tasks = taskSet.tasks;
  if (options.task) tasks = tasks.filter((task) => task.id === options.task);
  if (tasks.length === 0) throw new Error("no matching tasks");
  const profiles = options.profiles.split(",").map((value) => value.trim()).filter(Boolean);
  const invalidProfiles = profiles.filter(
    (profile) => ![
      "grep",
      "baseline",
      "structural",
      "structural-full",
      "structural-elided",
    ].includes(profile),
  );
  if (invalidProfiles.length > 0) {
    throw new Error(`unknown profiles: ${invalidProfiles.join(", ")}`);
  }
  if (!/^[A-Za-z0-9._-]+$/.test(options.trial)) {
    throw new Error("--trial may contain only letters, numbers, dot, underscore, and hyphen");
  }
  const schema = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../eval/agent-response.schema.json");
  const repository = path.resolve(options.repository);
  const jscout = path.resolve(options.jscout);
  const telemetry = path.resolve(options.telemetry);
  const responses = path.resolve(options.responses);
  const artifacts = path.resolve(options.artifacts);
  const lockPath = `${responses}.lock`;
  let lockFd;
  try {
    lockFd = fs.openSync(lockPath, "wx");
  } catch (error) {
    if (error.code === "EEXIST") {
      throw new Error(`another evaluation writer holds ${lockPath}; wait for it to exit before resuming`);
    }
    throw error;
  }
  const releaseLock = () => {
    if (lockFd === undefined) return;
    fs.closeSync(lockFd);
    lockFd = undefined;
    try { fs.unlinkSync(lockPath); } catch {}
  };
  process.once("exit", releaseLock);
  const resume = options.resume === "true";
  for (const outputPath of [responses, telemetry]) {
    if (!resume && fs.existsSync(outputPath) && fs.statSync(outputPath).size > 0) {
      throw new Error(`refusing to append to non-empty output: ${outputPath}`);
    }
  }
  if (!resume && fs.existsSync(artifacts) && fs.readdirSync(artifacts).length > 0) {
    throw new Error(`refusing to overwrite non-empty artifacts directory: ${artifacts}`);
  }
  fs.mkdirSync(artifacts, { recursive: true });
  const completed = new Set(
    resume && fs.existsSync(responses)
      ? jsonLines(fs.readFileSync(responses, "utf8"))
        .map((row) => `${row.task_id}\0${row.profile}`)
      : [],
  );
  const output = [];
  const disabledSkills = options["disable-skills"]
    ? options["disable-skills"].split(",").map((value) => value.trim()).filter(Boolean)
    : [];
  const disabledMcps = options["disable-mcps"]
    ? options["disable-mcps"].split(",").map((value) => value.trim()).filter(Boolean)
    : [];
  const extraConfig = options["extra-config"] ? JSON.parse(options["extra-config"]) : [];
  if (!Array.isArray(extraConfig) || extraConfig.some((value) => typeof value !== "string")) {
    throw new Error("--extra-config must be a JSON array of Codex key=value strings");
  }

  for (let taskIndex = 0; taskIndex < tasks.length; taskIndex += 1) {
    const task = tasks[taskIndex];
    const orderedProfiles = taskIndex % 2 === 0 ? profiles : [...profiles].reverse();
    for (const profile of orderedProfiles) {
      if (completed.has(`${task.id}\0${profile}`)) {
        process.stderr.write(`[${task.id}] ${profile} (already complete)\n`);
        continue;
      }
      const usesJscout = profile !== "grep";
      const toolProfile = profile.startsWith("structural-") ? "structural" : profile;
      const sourceView = profile === "structural-elided" ? "elided" : "full";
      const session = `${profile}-${task.id}-${options.trial}`;
      const lastMessage = path.join(os.tmpdir(), `jscout-${session}-${process.pid}.json`);
      const args = [
        "exec",
        "--ephemeral",
        "--skip-git-repo-check",
        "--sandbox", "read-only",
        "--cd", repository,
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
      if (options["load-user-config"] !== "true") args.splice(1, 0, "--ignore-user-config");
      if (usesJscout) {
        args.push(
          "--config", `mcp_servers.jscout.command=${JSON.stringify(jscout)}`,
          "--config", `mcp_servers.jscout.args=${JSON.stringify(["mcp", repository, "--profile", toolProfile, "--source-view", sourceView, "--telemetry", telemetry])}`,
          "--config", `mcp_servers.jscout.env.JSCOUT_TASK_ID=${JSON.stringify(task.id)}`,
          "--config", `mcp_servers.jscout.env.JSCOUT_SESSION_ID=${JSON.stringify(session)}`,
          "--config", `mcp_servers.jscout.env.JSCOUT_PROFILE_LABEL=${JSON.stringify(profile)}`,
          "--config", "mcp_servers.jscout.default_tools_approval_mode=\"approve\"",
          "--config", "mcp_servers.jscout.tools.semantic_search.approval_mode=\"approve\"",
          "--config", "mcp_servers.jscout.tools.who_uses.approval_mode=\"approve\"",
          "--config", "mcp_servers.jscout.tools.definition.approval_mode=\"approve\"",
          "--config", "mcp_servers.jscout.tools.file_outline.approval_mode=\"approve\"",
          "--config", "mcp_servers.jscout.tools.events.approval_mode=\"approve\"",
          "--config", "mcp_servers.jscout.tools.neighborhood.approval_mode=\"approve\"",
        );
      }
      for (const name of disabledMcps) args.push("--config", `mcp_servers.${name}.enabled=false`);
      for (const config of extraConfig) args.push("--config", config);
      if (disabledSkills.length > 0) {
        const skillConfig = disabledSkills
          .map((skillPath) => `{ path = ${JSON.stringify(path.resolve(skillPath))}, enabled = false }`)
          .join(", ");
        args.push("--config", `skills.config=[${skillConfig}]`);
      }
      args.push(promptFor(task, {
        profile,
        requireJscout: options["require-jscout"] === "true",
        requireDefinition: options["require-definition"] === "true",
      }));
      process.stderr.write(`[${task.id}] ${profile}\n`);
      const result = await run(options.codex, args, {
        cwd: repository,
        env: { ...process.env, JSCOUT_TASK_ID: task.id, JSCOUT_SESSION_ID: session },
      });
      const events = jsonLines(result.stdout);
      const usage = tokenUsage(events);
      fs.writeFileSync(path.join(artifacts, `${session}.jsonl`), result.stdout);
      fs.writeFileSync(path.join(artifacts, `${session}.stderr.log`), result.stderr);
      let answer = { answer: "", files: [], symbols: [], inspected_files: [] };
      let runnerError = null;
      try {
        answer = JSON.parse(fs.readFileSync(lastMessage, "utf8"));
      } catch (error) {
        runnerError = `unable to parse final response: ${error.message}`;
      }
      if (result.code !== 0) runnerError = `codex exited ${result.code}: ${result.stderr.trim()}`;
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
        ...usage,
        duration_ms: result.duration_ms,
        correct: sameSet(task.gold?.files, answer.files) && sameSet(task.gold?.symbols, answer.symbols),
      };
      if (runnerError) row.runner_error = runnerError;
      output.push(row);
      fs.appendFileSync(responses, `${JSON.stringify(row)}\n`);
      if (fs.existsSync(lastMessage)) fs.unlinkSync(lastMessage);
    }
  }
  process.stdout.write(`${JSON.stringify(output, null, 2)}\n`);
}

main().catch((error) => {
  process.stderr.write(`${error.stack ?? error.message}\n`);
  process.exitCode = 1;
});
