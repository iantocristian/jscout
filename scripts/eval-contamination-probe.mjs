#!/usr/bin/env node

import { spawn } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import process from "node:process";
import { fileURLToPath, pathToFileURL } from "node:url";

const TOOL_ITEM_TYPES = new Set([
  "command_execution",
  "computer_tool_call",
  "custom_tool_call",
  "dynamic_tool_call",
  "file_change",
  "function_call",
  "mcp_tool_call",
  "web_search_call",
]);

function parseArgs(argv) {
  const options = {
    codex: "codex",
    model: "gpt-5.6-terra",
    reasoning: "high",
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
  for (const required of ["tasks", "output", "artifacts"]) {
    if (!options[required]) throw new Error(`--${required} is required`);
  }
  if (!/^[A-Za-z0-9._-]+$/.test(options.trial)) {
    throw new Error("--trial may contain only letters, numbers, dot, underscore, and hyphen");
  }
  return options;
}

function run(command, args, { cwd }) {
  return new Promise((resolve) => {
    const started = Date.now();
    const child = spawn(command, args, {
      cwd,
      env: process.env,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8");
    child.stderr.setEncoding("utf8");
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", (error) => resolve({
      code: null,
      stdout,
      stderr,
      error,
      duration_ms: Date.now() - started,
    }));
    child.on("close", (code) => resolve({
      code,
      stdout,
      stderr,
      error: null,
      duration_ms: Date.now() - started,
    }));
  });
}

export function jsonLines(text) {
  return text.split(/\r?\n/).filter(Boolean).flatMap((line) => {
    try { return [JSON.parse(line)]; } catch { return []; }
  });
}

export function setOverlap(expected = [], actual = []) {
  const gold = new Set(expected);
  const predicted = new Set(actual);
  const matches = [...predicted].filter((value) => gold.has(value));
  return {
    matches: matches.sort(),
    precision: predicted.size === 0 ? (gold.size === 0 ? 1 : 0) : matches.length / predicted.size,
    recall: gold.size === 0 ? 1 : matches.length / gold.size,
  };
}

export function toolKinds(events) {
  const found = new Set();
  const visit = (value) => {
    if (Array.isArray(value)) {
      for (const item of value) visit(item);
      return;
    }
    if (value === null || typeof value !== "object") return;
    if (typeof value.type === "string" && TOOL_ITEM_TYPES.has(value.type)) {
      found.add(value.type);
    }
    for (const nested of Object.values(value)) visit(nested);
  };
  visit(events);
  return [...found].sort();
}

export function classifyProbe({ runnerError, tools, fileOverlap, symbolOverlap }) {
  if (runnerError || tools.length > 0) return "invalid";
  if (fileOverlap.recall === 1 && fileOverlap.matches.length > 0) return "contaminated";
  if (fileOverlap.matches.length > 0 || symbolOverlap.matches.length > 0) return "review";
  return "clean";
}

function promptFor(task, repositoryName) {
  const question = task.contamination_prompt ?? task.prompt;
  return [
    "This is a model-memory contamination probe.",
    "Do not use tools, shell commands, the filesystem, a repository, web search, or external sources.",
    "Answer solely from knowledge already present in the model before this request.",
    "Do not infer paths from naming conventions. If you do not remember, return empty files and symbols and confidence `unknown`.",
    "List only production repository-relative paths and named symbols you genuinely remember.",
    "",
    `In ${repositoryName}, ${question}`,
  ].join("\n");
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const taskPath = path.resolve(options.tasks);
  const outputPath = path.resolve(options.output);
  const artifactPath = path.resolve(options.artifacts);
  const taskSet = JSON.parse(fs.readFileSync(taskPath, "utf8"));
  let tasks = taskSet.tasks;
  if (options.task) tasks = tasks.filter((task) => task.id === options.task);
  if (tasks.length === 0) throw new Error("no matching tasks");

  const resume = options.resume === "true";
  if (!resume && fs.existsSync(outputPath) && fs.statSync(outputPath).size > 0) {
    throw new Error(`refusing to append to non-empty output: ${outputPath}`);
  }
  if (!resume && fs.existsSync(artifactPath) && fs.readdirSync(artifactPath).length > 0) {
    throw new Error(`refusing to overwrite non-empty artifacts directory: ${artifactPath}`);
  }
  fs.mkdirSync(path.dirname(outputPath), { recursive: true });
  fs.mkdirSync(artifactPath, { recursive: true });
  const completed = new Set(
    resume && fs.existsSync(outputPath)
      ? jsonLines(fs.readFileSync(outputPath, "utf8")).map((row) => row.task_id)
      : [],
  );
  const schema = path.resolve(
    path.dirname(fileURLToPath(import.meta.url)),
    "../eval/contamination-probe.schema.json",
  );
  const repositoryName = taskSet.repository?.name ?? "the repository";
  const rows = [];

  for (const task of tasks) {
    if (completed.has(task.id)) {
      process.stderr.write(`[${task.id}] already complete\n`);
      continue;
    }
    const emptyWorkspace = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-contamination-"));
    const session = `contamination-${task.id}-${options.trial}`;
    const lastMessage = path.join(os.tmpdir(), `jscout-${session}-${process.pid}.json`);
    const args = [
      "exec",
      "--ignore-user-config",
      "--ephemeral",
      "--skip-git-repo-check",
      "--sandbox", "read-only",
      "--cd", emptyWorkspace,
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
      promptFor(task, repositoryName),
    ];

    process.stderr.write(`[${task.id}] ${options.model}/${options.reasoning}\n`);
    let result;
    try {
      result = await run(options.codex, args, { cwd: emptyWorkspace });
    } finally {
      fs.rmSync(emptyWorkspace, { recursive: true, force: true });
    }
    const events = jsonLines(result.stdout);
    fs.writeFileSync(path.join(artifactPath, `${session}.jsonl`), result.stdout);
    fs.writeFileSync(path.join(artifactPath, `${session}.stderr.log`), result.stderr);

    let answer = { answer: "", files: [], symbols: [], confidence: "unknown" };
    let runnerError = null;
    try {
      answer = JSON.parse(fs.readFileSync(lastMessage, "utf8"));
    } catch (error) {
      runnerError = `unable to parse final response: ${error.message}`;
    }
    if (result.error) runnerError = result.error.message;
    if (result.code !== 0) runnerError = `codex exited ${result.code}: ${result.stderr.trim()}`;
    if (fs.existsSync(lastMessage)) fs.unlinkSync(lastMessage);

    const tools = toolKinds(events);
    const fileOverlap = setOverlap(task.gold?.files, answer.files);
    const symbolOverlap = setOverlap(task.gold?.symbols, answer.symbols);
    const status = classifyProbe({ runnerError, tools, fileOverlap, symbolOverlap });
    const row = {
      task_id: task.id,
      model: options.model,
      reasoning: options.reasoning,
      confidence: answer.confidence ?? "unknown",
      answer: answer.answer ?? "",
      files: answer.files ?? [],
      symbols: answer.symbols ?? [],
      tool_activity: tools,
      file_overlap: fileOverlap,
      symbol_overlap: symbolOverlap,
      status,
      admitted: status === "clean",
      duration_ms: result.duration_ms,
    };
    if (runnerError) row.runner_error = runnerError;
    rows.push(row);
    fs.appendFileSync(outputPath, `${JSON.stringify(row)}\n`);
  }

  process.stdout.write(`${JSON.stringify(rows, null, 2)}\n`);
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  main().catch((error) => {
    process.stderr.write(`${error.stack ?? error.message}\n`);
    process.exitCode = 1;
  });
}
