import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { spawn } from "node:child_process";
import { once } from "node:events";
import readline from "node:readline";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";

const sidecar = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../src/main.mjs");
const bundledTypeScript = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../node_modules/typescript");

function sourceHash(text) {
  return bytesToHex(blake3(new TextEncoder().encode(text)));
}

function fixture(files) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-checker-"));
  for (const [relative, content] of Object.entries(files)) {
    const target = path.join(root, relative);
    fs.mkdirSync(path.dirname(target), { recursive: true });
    fs.writeFileSync(target, content);
  }
  return root;
}

function client(root) {
  const child = spawn(process.execPath, [sidecar, root], { stdio: ["pipe", "pipe", "pipe"] });
  const pending = new Map();
  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  const lines = readline.createInterface({ input: child.stdout, crlfDelay: Infinity });
  lines.on("line", (line) => {
    const message = JSON.parse(line);
    pending.get(message.id)?.(message);
    pending.delete(message.id);
  });
  let sequence = 0;
  const request = (kind, body = {}) => new Promise((resolve) => {
    const id = `t${sequence += 1}`;
    pending.set(id, resolve);
    child.stdin.write(`${JSON.stringify({ protocol: 2, id, kind, ...body })}\n`);
  });
  return {
    child,
    request,
    stderr: () => stderr,
    async close() {
      await request("shutdown");
      child.stdin.end();
      await once(child, "exit");
    },
  };
}

test("prints and returns the actual Node worker error", async () => {
  const root = fixture({ "main.ts": "export const value = 1;\n" });
  const checker = client(root);
  await checker.request("hello");
  fs.rmSync(root, { recursive: true, force: true });

  const response = await checker.request("capabilities");
  assert.equal(response.kind, "error");
  assert.equal(response.error.code, "checker_crash");
  assert.match(response.error.message, /ENOENT/u);
  assert.match(response.error.message, /realpath/u);
  await checker.close();

  assert.match(checker.stderr(), /worker error during capabilities/);
  assert.match(checker.stderr(), /ENOENT/u);
  assert.match(checker.stderr(), /realpath/u);
});

function queryFor(source, call, receiver, property) {
  const callStart = Buffer.byteLength(source.slice(0, source.lastIndexOf(call)));
  const receiverStart = callStart;
  const propertyCharacterStart = source.lastIndexOf(call) + call.lastIndexOf(property);
  return {
    file: "main.ts",
    indexed_hash: sourceHash(source),
    call_start: callStart,
    call_end: callStart + Buffer.byteLength(call),
    receiver_start: receiverStart,
    receiver_end: receiverStart + Buffer.byteLength(receiver),
    property_start: Buffer.byteLength(source.slice(0, propertyCharacterStart)),
    property_end: Buffer.byteLength(source.slice(0, propertyCharacterStart + property.length)),
  };
}

test("resolves nested, this-qualified, and optional member occurrences", async (context) => {
  const source = [
    "class CardTable { insert(): void {} }",
    "class Service { run(): void {} }",
    "declare const dbs: { wave: { card: CardTable } };",
    "dbs.wave.card.insert()",
    "dbs.wave.card?.insert()",
    "class Runner { constructor(private service: Service) {} go() { this.service.run() } }",
    "",
  ].join("\n");
  const root = fixture({
    "main.ts": source,
    "tsconfig.json": JSON.stringify({ compilerOptions: { strict: true }, files: ["main.ts"] }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  const hello = await checker.request("hello");
  assert.equal(hello.kind, "ready");

  for (const [call, receiver, property] of [
    ["dbs.wave.card.insert()", "dbs.wave.card", "insert"],
    ["dbs.wave.card?.insert()", "dbs.wave.card", "insert"],
    ["this.service.run()", "this.service", "run"],
  ]) {
    const message = await checker.request("resolve_member", {
      query: queryFor(source, call, receiver, property),
    });
    assert.equal(message.kind, "resolve_member_result");
    assert.equal(message.result.projects.length, 1);
    assert.equal(message.result.projects[0].status, "resolved");
    assert.equal(message.result.projects[0].declarations[0].file, "main.ts");
  }
  await checker.close();
});

test("plans ownership without a Program and resolves a bounded project batch", async (context) => {
  const source = [
    "class Alpha { save(): void {} }",
    "class Beta { run(): void {} }",
    "declare const alpha: Alpha; declare const beta: Beta;",
    "alpha.save()",
    "beta.run()",
    "",
  ].join("\n");
  const root = fixture({
    "main.ts": source,
    "tsconfig.json": JSON.stringify({ compilerOptions: { strict: true }, files: ["main.ts"] }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", { files: ["main.ts", "main.ts"] });
  assert.equal(plan.kind, "plan_members_result");
  assert.deepEqual(plan.result.files, [{
    file: "main.ts",
    project_ids: ["tsconfig.json"],
    excluded_project_ids: [],
    tooling_fallback: false,
  }]);

  const resolved = await checker.request("resolve_members", {
    project_id: "tsconfig.json",
    queries: [
      queryFor(source, "alpha.save()", "alpha", "save"),
      queryFor(source, "beta.run()", "beta", "run"),
    ],
  });
  assert.equal(resolved.kind, "resolve_members_result");
  assert.equal(resolved.result.results.length, 2);
  assert.ok(resolved.result.results.every((item) => item.answer.status === "resolved"));
  assert.ok(resolved.result.results.every((item) =>
    item.answer.declarations.every((declaration) => declaration.context === "repo")));
  assert.ok(resolved.result.resources.rss_bytes > 0);

  const validation = await checker.request("validate_project", {
    project_id: "tsconfig.json",
    fingerprint: resolved.result.checker_input_fingerprint,
  });
  assert.equal(validation.kind, "validate_project_result");
  assert.equal(validation.result.valid, true);
  assert.ok(validation.result.inputs.some((input) => input.path.endsWith("main.ts")));
  await checker.close();
});

test("reports a file outside configured projects as inferred ownership", async (context) => {
  const root = fixture({
    "configured.ts": "export const configured = true;\n",
    "orphan.mjs": "export function run(value) { value.call(); }\n",
    "tsconfig.json": JSON.stringify({ files: ["configured.ts"] }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", { files: ["orphan.mjs"] });
  assert.deepEqual(plan.result.files, [{
    file: "orphan.mjs",
    project_ids: ["inferred:orphan.mjs"],
    excluded_project_ids: [],
    tooling_fallback: false,
  }]);
  await checker.close();
});

test("excludes tooling config ownership only when a non-tooling owner remains", async (context) => {
  const shared = "declare const shared: { run(): void };\nshared.run()\n";
  const lintOnly = "declare const lintOnly: { run(): void };\nlintOnly.run()\n";
  const root = fixture({
    "main.ts": shared,
    "lint-only.ts": lintOnly,
    "tsconfig.json": JSON.stringify({ include: ["main.ts"] }),
    "tsconfig.eslint.json": JSON.stringify({
      extends: "./tsconfig.json",
      compilerOptions: { allowJs: true },
      include: ["*.ts"],
    }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", { files: ["main.ts", "lint-only.ts"] });
  assert.equal(plan.kind, "plan_members_result");
  assert.deepEqual(plan.result.files, [
    {
      file: "lint-only.ts",
      project_ids: ["tsconfig.eslint.json"],
      excluded_project_ids: [],
      tooling_fallback: true,
    },
    {
      file: "main.ts",
      project_ids: ["tsconfig.json"],
      excluded_project_ids: ["tsconfig.eslint.json"],
      tooling_fallback: false,
    },
  ]);
  assert.deepEqual(
    plan.result.projects.map(({ project_id, purpose, purpose_reasons }) => ({
      project_id,
      purpose,
      purpose_reasons,
    })),
    [
      {
        project_id: "tsconfig.eslint.json",
        purpose: "tooling",
        purpose_reasons: ["tooling-filename"],
      },
      { project_id: "tsconfig.json", purpose: "general", purpose_reasons: [] },
    ],
  );

  const fallback = await checker.request("resolve_members", {
    project_id: "tsconfig.eslint.json",
    queries: [
      {
        ...queryFor(lintOnly, "lintOnly.run()", "lintOnly", "run"),
        file: "lint-only.ts",
      },
      // Rust repository policy may promote this heuristically excluded owner.
      // It is still a real TypeScript member and must remain executable.
      queryFor(shared, "shared.run()", "shared", "run"),
    ],
  });
  assert.equal(fallback.kind, "resolve_members_result", JSON.stringify(fallback));
  assert.equal(fallback.result.results.length, 2);
  assert.ok(fallback.result.results.every((result) => result.answer.status === "resolved"));
  await checker.close();
});

test("does not classify noEmit as tooling without an independent lint signal", async (context) => {
  const root = fixture({
    "main.ts": "export const value = 1;\n",
    "tsconfig.json": JSON.stringify({ files: ["main.ts"] }),
    "tsconfig.check.json": JSON.stringify({
      compilerOptions: { noEmit: true },
      files: ["main.ts"],
    }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", { files: ["main.ts"] });
  assert.equal(plan.kind, "plan_members_result");
  assert.deepEqual(plan.result.files[0], {
    file: "main.ts",
    project_ids: ["tsconfig.check.json", "tsconfig.json"],
    excluded_project_ids: [],
    tooling_fallback: false,
  });
  assert.ok(plan.result.projects.every((project) => project.purpose === "general"));
  await checker.close();
});

test("uses an explicit lint script to corroborate a noEmit tooling config", async (context) => {
  const root = fixture({
    "package.json": JSON.stringify({
      scripts: { "lint:types": "tsc -p tsconfig.check.json" },
    }),
    "main.ts": "export const value = 1;\n",
    "tsconfig.json": JSON.stringify({ files: ["main.ts"] }),
    "tsconfig.check.json": JSON.stringify({
      compilerOptions: { noEmit: true },
      files: ["main.ts"],
    }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", { files: ["main.ts"] });
  assert.deepEqual(plan.result.files[0], {
    file: "main.ts",
    project_ids: ["tsconfig.json"],
    excluded_project_ids: ["tsconfig.check.json"],
    tooling_fallback: false,
  });
  const tooling = plan.result.projects.find(
    (project) => project.project_id === "tsconfig.check.json",
  );
  assert.equal(tooling.purpose, "tooling");
  assert.deepEqual(tooling.purpose_reasons, ["tooling-script:lint:types"]);
  await checker.close();
});

test("matches lint scripts to the exact package-relative config path", async (context) => {
  const root = fixture({
    "package.json": JSON.stringify({
      scripts: { "lint:types": "tsc -p packages/one/tsconfig.json" },
    }),
    "packages/one/main.ts": "export const one = 1;\n",
    "packages/one/tsconfig.json": JSON.stringify({
      compilerOptions: { noEmit: true },
      files: ["main.ts"],
    }),
    "packages/two/main.ts": "export const two = 2;\n",
    "packages/two/tsconfig.json": JSON.stringify({
      compilerOptions: { noEmit: true },
      files: ["main.ts"],
    }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", {
    files: ["packages/one/main.ts", "packages/two/main.ts"],
  });
  const purposes = Object.fromEntries(plan.result.projects.map((project) => [
    project.project_id,
    { purpose: project.purpose, reasons: project.purpose_reasons },
  ]));
  assert.deepEqual(purposes["packages/one/tsconfig.json"], {
    purpose: "tooling",
    reasons: ["tooling-script:lint:types"],
  });
  assert.deepEqual(purposes["packages/two/tsconfig.json"], {
    purpose: "general",
    reasons: [],
  });
  await checker.close();
});

test("executes a noEmit main config promoted above an overlapping dist config", async (context) => {
  const source = "declare const agent: { run(): void };\nagent.run()\n";
  const root = fixture({
    "package.json": JSON.stringify({
      scripts: { "lint:types": "tsc -p tsconfig.json" },
    }),
    "src/core/agent/agent.ts": source,
    "test/example.ts": "export const test = true;\n",
    "vitest.config.ts": "export default {};\n",
    "tsconfig.json": JSON.stringify({
      compilerOptions: { noEmit: true },
      include: ["./src", "./test", "vitest.config.ts"],
      exclude: ["dist", "node_modules"],
    }),
    "tsconfig.dist.json": JSON.stringify({
      compilerOptions: { noEmit: false },
      include: ["./src"],
      exclude: ["dist", "node_modules"],
    }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", {
    files: ["src/core/agent/agent.ts"],
  });
  assert.deepEqual(plan.result.files[0], {
    file: "src/core/agent/agent.ts",
    project_ids: ["tsconfig.dist.json"],
    excluded_project_ids: ["tsconfig.json"],
    tooling_fallback: false,
  });

  // Repository reconnaissance may promote the excluded main config. Parsed
  // TypeScript membership, not the bootstrap preference, is authoritative.
  const promoted = await checker.request("resolve_members", {
    project_id: "tsconfig.json",
    queries: [{
      ...queryFor(source, "agent.run()", "agent", "run"),
      file: "src/core/agent/agent.ts",
    }],
  });
  assert.equal(promoted.kind, "resolve_members_result", JSON.stringify(promoted));
  assert.equal(promoted.result.results[0].answer.status, "resolved");
  await checker.close();
});

test("every planner-selected owner accepts a file shared by several configs", async (context) => {
  const source = "declare const shared: { run(): void };\nshared.run()\n";
  const root = fixture({
    "main.ts": source,
    "tsconfig.json": JSON.stringify({ files: ["main.ts"] }),
    "tsconfig.browser.json": JSON.stringify({ files: ["main.ts"] }),
    "tsconfig.server.json": JSON.stringify({ files: ["main.ts"] }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const planner = client(root);
  context.after(() => planner.child.kill());
  await planner.request("hello");
  const plan = await planner.request("plan_members", { files: ["main.ts"] });
  await planner.close();

  assert.deepEqual(plan.result.files[0].project_ids, [
    "tsconfig.browser.json",
    "tsconfig.json",
    "tsconfig.server.json",
  ]);
  assert.deepEqual(plan.result.files[0].excluded_project_ids, []);
  for (const projectId of plan.result.files[0].project_ids) {
    const resolver = client(root);
    context.after(() => resolver.child.kill());
    await resolver.request("hello");
    const result = await resolver.request("resolve_members", {
      project_id: projectId,
      queries: [queryFor(source, "shared.run()", "shared", "run")],
    });
    assert.equal(result.kind, "resolve_members_result", `${projectId}: ${JSON.stringify(result)}`);
    await resolver.close();
  }
});

test("keeps every tooling owner when no general project owns the file", async (context) => {
  const root = fixture({
    "main.ts": "export const value = 1;\n",
    "tsconfig.eslint.json": JSON.stringify({ files: ["main.ts"] }),
    "tsconfig.lint.json": JSON.stringify({ files: ["main.ts"] }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", { files: ["main.ts"] });
  assert.deepEqual(plan.result.files[0], {
    file: "main.ts",
    project_ids: ["tsconfig.eslint.json", "tsconfig.lint.json"],
    excluded_project_ids: [],
    tooling_fallback: true,
  });
  await checker.close();
});

test("keeps solution-style project references out of leaf ownership", async (context) => {
  const appSource = "declare const app: { run(): void };\napp.run()\n";
  const libSource = "declare const lib: { run(): void };\nlib.run()\n";
  const root = fixture({
    "tsconfig.json": JSON.stringify({
      files: [],
      references: [{ path: "./packages/app" }, { path: "./packages/lib" }],
    }),
    "packages/app/src/app.ts": appSource,
    "packages/app/tsconfig.json": JSON.stringify({
      compilerOptions: { composite: true },
      include: ["src"],
      references: [{ path: "../lib" }],
    }),
    "packages/lib/src/lib.ts": libSource,
    "packages/lib/tsconfig.json": JSON.stringify({
      compilerOptions: { composite: true },
      include: ["src"],
    }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", {
    files: ["packages/app/src/app.ts", "packages/lib/src/lib.ts"],
  });
  assert.deepEqual(plan.result.files, [
    {
      file: "packages/app/src/app.ts",
      project_ids: ["packages/app/tsconfig.json"],
      excluded_project_ids: [],
      tooling_fallback: false,
    },
    {
      file: "packages/lib/src/lib.ts",
      project_ids: ["packages/lib/tsconfig.json"],
      excluded_project_ids: [],
      tooling_fallback: false,
    },
  ]);
  assert.ok(plan.result.projects.some((project) => (
    project.project_id === "tsconfig.json" && project.file_count === 0
  )));
  await checker.close();
});

test("reports a config that becomes malformed after planning as project_not_found", async (context) => {
  const root = fixture({
    "main.ts": "export const value = 1;\n",
    "tsconfig.json": JSON.stringify({ files: ["main.ts"] }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const planner = client(root);
  context.after(() => planner.child.kill());
  await planner.request("hello");
  const plan = await planner.request("plan_members", { files: ["main.ts"] });
  assert.deepEqual(plan.result.files[0].project_ids, ["tsconfig.json"]);
  await planner.close();

  fs.writeFileSync(path.join(root, "tsconfig.json"), "{");
  const resolver = client(root);
  context.after(() => resolver.child.kill());
  await resolver.request("hello");
  const result = await resolver.request("resolve_members", {
    project_id: "tsconfig.json",
    queries: [{
      file: "main.ts",
      indexed_hash: sourceHash("export const value = 1;\n"),
      call_start: 0,
      call_end: 1,
      receiver_start: 0,
      receiver_end: 1,
      property_start: 0,
      property_end: 1,
    }],
  });
  assert.equal(result.kind, "error");
  assert.equal(result.error.code, "project_not_found");
  await resolver.close();
});

test("discovers paired configs independently across workspace members", async (context) => {
  const source = (name) => `declare const ${name}: { run(): void };\n${name}.run()\n`;
  const root = fixture({
    "package.json": JSON.stringify({ private: true, workspaces: ["packages/*"] }),
    "packages/alpha/package.json": JSON.stringify({
      name: "alpha",
      scripts: { "lint:types": "tsc -p tsconfig.json" },
    }),
    "packages/alpha/src/agent.ts": source("alpha"),
    "packages/alpha/tsconfig.json": JSON.stringify({
      compilerOptions: { noEmit: true }, include: ["src"],
    }),
    "packages/alpha/tsconfig.dist.json": JSON.stringify({ include: ["src"] }),
    "packages/beta/package.json": JSON.stringify({
      name: "beta",
      scripts: { "lint:types": "tsc -p tsconfig.json" },
    }),
    "packages/beta/src/agent.ts": source("beta"),
    "packages/beta/tsconfig.json": JSON.stringify({
      compilerOptions: { noEmit: true }, include: ["src"],
    }),
    "packages/beta/tsconfig.dist.json": JSON.stringify({ include: ["src"] }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", {
    files: ["packages/alpha/src/agent.ts", "packages/beta/src/agent.ts"],
  });
  for (const ownership of plan.result.files) {
    const member = ownership.file.split("/")[1];
    assert.deepEqual(ownership.project_ids, [`packages/${member}/tsconfig.dist.json`]);
    assert.deepEqual(ownership.excluded_project_ids, [`packages/${member}/tsconfig.json`]);
    assert.equal(ownership.tooling_fallback, false);
  }
  await checker.close();
});

test("does not borrow a lint signal from a neighboring compound command", async (context) => {
  const root = fixture({
    "package.json": JSON.stringify({
      scripts: { check: "eslint . && tsc -p tsconfig.app.json" },
    }),
    "main.ts": "export const value = 1;\n",
    "tsconfig.json": JSON.stringify({ files: ["main.ts"] }),
    "tsconfig.app.json": JSON.stringify({
      compilerOptions: { noEmit: true },
      files: ["main.ts"],
    }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const plan = await checker.request("plan_members", { files: ["main.ts"] });
  assert.deepEqual(plan.result.files[0], {
    file: "main.ts",
    project_ids: ["tsconfig.app.json", "tsconfig.json"],
    excluded_project_ids: [],
    tooling_fallback: false,
  });
  assert.ok(plan.result.projects.every((project) => project.purpose === "general"));
  await checker.close();
});

// The program must be built from the EFFECTIVE compiler options. Normalizing
// absolute paths before `ts.createProgram` (normalization belongs to the
// fingerprint) silently degraded every receiver reached through a `paths`
// mapping to `any`, disabling enrichment for the monorepo shapes G10 targets.
test("resolves receivers through a baseUrl/paths mapping and keeps type text repo-relative", async (context) => {
  const source = [
    'import { makeCard } from "@lib/tables";',
    "const card = makeCard();",
    "card.insert()",
    "",
  ].join("\n");
  const root = fixture({
    "main.ts": source,
    "src/lib/tables.ts": [
      "export class CardTable { insert(): void {} }",
      "export function makeCard(): CardTable { return new CardTable() }",
      "",
    ].join("\n"),
    "tsconfig.json": JSON.stringify({
      compilerOptions: { strict: true, baseUrl: ".", paths: { "@lib/*": ["src/lib/*"] } },
      files: ["main.ts", "src/lib/tables.ts"],
    }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const message = await checker.request("resolve_member", {
    query: queryFor(source, "card.insert()", "card", "insert"),
  });
  assert.equal(message.kind, "resolve_member_result");
  const [answer] = message.result.projects;
  assert.equal(answer.status, "resolved", "a mapped receiver must not degrade to any/unknown");
  assert.equal(answer.declarations.length, 1);
  assert.equal(answer.declarations[0].file, "src/lib/tables.ts");
  assert.ok(
    !answer.receiver_type.includes(root),
    `receiver type leaked a machine-absolute path: ${answer.receiver_type}`,
  );

  // The fingerprint is what normalization exists for, and it still round-trips.
  const validation = await checker.request("validate_inputs", {
    entries: [{ file: "main.ts", project_id: "tsconfig.json", fingerprint: answer.checker_input_fingerprint }],
  });
  assert.equal(validation.result.valid, true);
  assert.ok(validation.result.results[0].inputs.length > 0);
  await checker.close();
});

test("keeps overlapping projects visible and invalidates changed checker inputs", async (context) => {
  const source = [
    "interface Alpha { save(): void }",
    "declare const alpha: Alpha;",
    "alpha.save()",
    "",
  ].join("\n");
  const root = fixture({
    "main.ts": source,
    "ambient.d.ts": "declare const ambientVersion: 1;\n",
    "tsconfig.base.json": JSON.stringify({ compilerOptions: { strict: true } }),
    "tsconfig.json": JSON.stringify({ extends: "./tsconfig.base.json", files: ["main.ts", "ambient.d.ts"] }),
    "nested/tsconfig.app.json": JSON.stringify({ extends: "../tsconfig.base.json", files: ["../main.ts", "../ambient.d.ts"] }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");
  const resolved = await checker.request("resolve_member", {
    query: queryFor(source, "alpha.save()", "alpha", "save"),
  });
  assert.equal(resolved.result.projects.length, 2);
  assert.deepEqual(
    resolved.result.projects.map((project) => project.project_id),
    ["nested/tsconfig.app.json", "tsconfig.json"],
  );
  const entries = resolved.result.projects.map((project) => ({
    file: "main.ts",
    project_id: project.project_id,
    fingerprint: project.checker_input_fingerprint,
  }));
  fs.writeFileSync(path.join(root, "tsconfig.base.json"), JSON.stringify({ compilerOptions: { strict: true }, display: "changed" }));
  const configValidation = await checker.request("validate_inputs", { entries });
  assert.equal(configValidation.kind, "validate_inputs_result");
  assert.equal(configValidation.result.valid, false);
  assert.ok(configValidation.result.results.every((result) => !result.valid));

  const refreshed = await checker.request("resolve_member", {
    query: queryFor(source, "alpha.save()", "alpha", "save"),
  });
  const refreshedEntries = refreshed.result.projects.map((project) => ({
    file: "main.ts",
    project_id: project.project_id,
    fingerprint: project.checker_input_fingerprint,
  }));
  fs.writeFileSync(path.join(root, "ambient.d.ts"), "declare const ambientVersion: 2;\n");
  const ambientValidation = await checker.request("validate_inputs", { entries: refreshedEntries });
  assert.equal(ambientValidation.result.valid, false);
  assert.ok(ambientValidation.result.results.every((result) => !result.valid));
  await checker.close();
});

test("keeps receiver identity, inheritance, overrides, and overload declarations distinct", async (context) => {
  const source = [
    "class Alpha { save(): void {} }",
    "class Beta { save(): void {} }",
    "class Base { run(): void {} }",
    "class Inherited extends Base {}",
    "class Override extends Base { run(): void {} }",
    "interface Overloaded { execute(value: string): void; execute(value: number): void }",
    "declare const alpha: Alpha; declare const beta: Beta;",
    "declare const inherited: Inherited; declare const overridden: Override;",
    "declare const overloaded: Overloaded;",
    "const intentionallyBroken: string = 123;",
    "alpha.save()",
    "beta.save()",
    "inherited.run()",
    "overridden.run()",
    "overloaded.execute('x')",
    "",
  ].join("\n");
  const root = fixture({
    "main.ts": source,
    "tsconfig.json": JSON.stringify({ compilerOptions: { strict: true }, files: ["main.ts"] }),
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const answers = {};
  for (const [call, receiver, property] of [
    ["alpha.save()", "alpha", "save"],
    ["beta.save()", "beta", "save"],
    ["inherited.run()", "inherited", "run"],
    ["overridden.run()", "overridden", "run"],
    ["overloaded.execute('x')", "overloaded", "execute"],
  ]) {
    const response = await checker.request("resolve_member", {
      query: queryFor(source, call, receiver, property),
    });
    assert.equal(response.kind, "resolve_member_result");
    assert.equal(response.result.projects[0].status, "resolved");
    answers[call] = response.result.projects[0];
  }
  assert.notEqual(
    answers["alpha.save()"].declarations[0].start,
    answers["beta.save()"].declarations[0].start,
  );
  assert.notEqual(
    answers["inherited.run()"].declarations[0].start,
    answers["overridden.run()"].declarations[0].start,
  );
  assert.equal(answers["overloaded.execute('x')"].declarations.length, 2);
  assert.ok(!("diagnostics" in answers["alpha.save()"]));
  await checker.close();
});

test("labels declaration provenance for vendored types and outside libraries", async (context) => {
  const source = 'import { helper } from "thing";\nhelper.go();\nconst list = [1];\nlist.push(2);\n';
  const root = fixture({
    "main.ts": source,
    "tsconfig.json": JSON.stringify({ compilerOptions: { moduleResolution: "node" }, files: ["main.ts"] }),
    "node_modules/@types/thing/package.json": JSON.stringify({ name: "@types/thing", types: "index.d.ts" }),
    "node_modules/@types/thing/index.d.ts":
      "export declare const helper: { go(): void };\n",
  });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");

  const vendored = await checker.request("resolve_member", {
    query: queryFor(source, "helper.go()", "helper", "go"),
  });
  assert.equal(vendored.result.projects[0].status, "resolved");
  assert.deepEqual(
    vendored.result.projects[0].declarations.map((declaration) => declaration.context),
    ["types"],
  );

  // Array.prototype.push lives in the TypeScript lib, which sits outside a
  // fixture root; the context still attributes it instead of a bare null file.
  const builtin = await checker.request("resolve_member", {
    query: queryFor(source, "list.push(2)", "list", "push"),
  });
  assert.equal(builtin.result.projects[0].status, "resolved");
  const contexts = builtin.result.projects[0].declarations.map((declaration) => declaration.context);
  assert.ok(contexts.length > 0);
  assert.ok(contexts.every((value) => value === "outside" || value === "lib"), JSON.stringify(contexts));
  await checker.close();
});

test("rejects traversal and source-hash drift with stable codes", async (context) => {
  const source = "declare const value: { run(): void };\nvalue.run()\n";
  const root = fixture({ "main.ts": source });
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");
  const traversal = await checker.request("resolve_member", {
    query: { ...queryFor(source, "value.run()", "value", "run"), file: "../outside.ts" },
  });
  assert.equal(traversal.error.code, "outside_root");
  const drift = await checker.request("resolve_member", {
    query: { ...queryFor(source, "value.run()", "value", "run"), indexed_hash: "stale" },
  });
  assert.equal(drift.error.code, "hash_mismatch");
  await checker.close();
});

test("prefers a repository TypeScript installation over the bundled fallback", async (context) => {
  const root = fixture({
    "package.json": JSON.stringify({ private: true }),
    "main.ts": "export const value = 1;\n",
    "tsconfig.json": JSON.stringify({ files: ["main.ts"] }),
  });
  fs.mkdirSync(path.join(root, "node_modules"), { recursive: true });
  fs.symlinkSync(
    bundledTypeScript,
    path.join(root, "node_modules/typescript"),
    process.platform === "win32" ? "junction" : "dir",
  );
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  const checker = client(root);
  context.after(() => checker.child.kill());
  await checker.request("hello");
  const capabilities = await checker.request("capabilities");
  assert.equal(capabilities.capabilities.typescript.source, "repository");
  assert.equal(capabilities.capabilities.typescript.version, "5.9.3");
  await checker.close();
});
