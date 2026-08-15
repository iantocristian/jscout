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

function prepareArm(repository, parent, workspace, setupCommand, runDir) {
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
});

const PROFILE_BASES = Object.freeze({
  checker: "structural",
  "checker-embed": "checker",
  "checker-scout": "checker",
  "checker-scout-embed": "checker-scout",
});

const PROFILE_INCREMENT = Object.freeze({
  checker: "enrich",
  "checker-embed": "embed",
  "checker-scout": "scout",
  "checker-scout-embed": "embed-product",
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

function prepareJscoutProfile({
  jscout,
  workspace,
  database,
  profile,
  runDir,
  scoutMaxCalls,
  scoutMaxSubjects,
  baseDatabase,
}) {
  const plan = profilePlan(profile);
  const env = {
    ...process.env,
    ...(plan.stages.some((stage) => stage.startsWith("embed"))
      ? { JSCOUT_EMBED_PROVIDER: "local" }
      : {}),
  };
  let stages = plan.stages;
  if (baseDatabase && fs.existsSync(baseDatabase)) {
    copyDatabase(baseDatabase, database);
    stages = [PROFILE_INCREMENT[profile]];
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
        ],
        {
          cwd: workspace,
          env,
          log: path.join(runDir, "jscout-scout.log"),
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

export function promptFor(task, treatment = "control") {
  const contract = [
    "You are implementing a real change in this repository.",
    "",
    `## Story`,
    task.story,
    "",
    "## Contract",
    "- Implement the change directly in the working directory; edit files as needed.",
    "- Investigate enough to cover every place the change genuinely affects.",
    "- You may run package-manager install, build, formatting, and test commands.",
  ];
  if (treatment === "forced") {
    contract.push(
      "- Use jscout exclusively for repository-wide code discovery, symbol lookup,",
      "  reference tracing, and architectural navigation. Do not use grep, rg,",
      "  git grep, find, IDE search, or custom repository-scanning scripts.",
      "- You may directly read files and line ranges identified by jscout, edit",
      "  files, inspect test/build output, and run tests or builds.",
    );
  }
  contract.push(
    "- Work only inside this synthetic one-commit snapshot. Do not inspect other",
    "  filesystem locations, GitHub, upstream Git history/remotes, or web/search",
    "  services. Do not fetch or clone repositories. Network use is limited to",
    "  package-manager access required for declared dependencies.",
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
  fs.mkdirSync(workRoot, { recursive: true });
  const scoutMaxCalls = Number(options["scout-max-calls"] ?? 64);
  if (!Number.isInteger(scoutMaxCalls) || scoutMaxCalls < 1) {
    throw new Error("--scout-max-calls must be a positive integer");
  }
  const scoutMaxSubjects = Number(options["scout-max-subjects"] ?? 512);
  if (!Number.isInteger(scoutMaxSubjects) || scoutMaxSubjects < 1) {
    throw new Error("--scout-max-subjects must be a positive integer");
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
    const gold = path.join(runsRoot, task.id, "gold");
    for (const required of [gold]) {
      if (!fs.existsSync(required)) throw new Error(`missing snapshot directory: ${required}`);
    }
    if (!task.story) throw new Error(`task ${task.id} has no story`);
    const goldTask = JSON.parse(fs.readFileSync(path.join(gold, "task.json"), "utf8"));
    const parent = task.parent ?? goldTask.parent;
    if (!parent) throw new Error(`task ${task.id} has no parent revision`);

    const orderedProfiles = taskIndex % 2 === 0 ? profiles : [...profiles].reverse();
    for (const profile of orderedProfiles) {
      const { usesJscout, stages } = profilePlan(profile);
      const treatments = usesJscout ? requestedTreatments : ["control"];
      const profileDatabase = usesJscout
        ? path.join(artifacts, "prepared-databases", task.id, `${profile}.db`)
        : null;
      for (let treatmentIndex = 0; treatmentIndex < treatments.length; treatmentIndex += 1) {
        const treatment = treatments[treatmentIndex];
        const session = `${profile}-${treatment}-${task.id}-${options.trial}`;
        const runDir = path.join(artifacts, session);
        const workspace = fs.mkdtempSync(path.join(workRoot, "r-"));
        fs.mkdirSync(runDir, { recursive: true });
        fs.writeFileSync(path.join(runDir, "workspace-path.txt"), `${workspace}\n`);
        prepareArm(
          repository,
          parent,
          workspace,
          task.setup_command ?? taskSet.setup_command,
          runDir,
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
          if (treatmentIndex === 0) {
            const baseProfile = PROFILE_BASES[profile];
            const baseDatabase = baseProfile
              ? path.join(artifacts, "prepared-databases", task.id, `${baseProfile}.db`)
              : null;
            prepareJscoutProfile({
              jscout,
              workspace,
              database,
              profile,
              runDir,
              scoutMaxCalls,
              scoutMaxSubjects,
              baseDatabase,
            });
            fs.mkdirSync(path.dirname(profileDatabase), { recursive: true });
            copyDatabase(database, profileDatabase);
          } else {
            copyDatabase(profileDatabase, database);
            fs.writeFileSync(
              path.join(runDir, "jscout-profile-reuse.json"),
              `${JSON.stringify({ source: profileDatabase, stages }, null, 2)}\n`,
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
          "--config", "features.browser_use=false",
          "--config", "features.computer_use=false",
          "--config", "features.plugins=false",
          "--config", "tools.web_search=false",
          "--config", "sandbox_workspace_write.network_access=true",
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
        args.push(promptFor(task, treatment));

        const eventsPath = path.join(runDir, "events.jsonl");
        const stderrPath = path.join(runDir, "stderr.log");
        process.stderr.write(`[${task.id}] ${profile}/${treatment} — live events: ${eventsPath}\n`);
        const result = await run(options.codex, args, {
          cwd: workspace,
          eventsPath,
          stderrPath,
          timeoutMs: Number(options["run-timeout"] ?? 1800) * 1000,
        });

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
          ], { encoding: "utf8" });
        } catch (error) {
          runnerError = runnerError ?? `grading failed: ${`${error.stderr ?? error.message}`.slice(0, 300)}`;
        }
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
        };
        if (runnerError) row.runner_error = runnerError;
        fs.appendFileSync(responses, `${JSON.stringify(row)}\n`);
        const keepWorkspace = options["keep-workspaces"] === "true";
        fs.writeFileSync(
          path.join(runDir, "workspace.json"),
          `${JSON.stringify({ path: workspace, kept: keepWorkspace }, null, 2)}\n`,
        );
        if (!keepWorkspace) fs.rmSync(workspace, { recursive: true, force: true });
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
