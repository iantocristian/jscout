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
    child.stdin.write(`${JSON.stringify({ protocol: 1, id, kind, ...body })}\n`);
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

test("prints the actual Node worker error while keeping a stable protocol error", async () => {
  const root = fixture({ "main.ts": "export const value = 1;\n" });
  const checker = client(root);
  await checker.request("hello");
  fs.rmSync(root, { recursive: true, force: true });

  const response = await checker.request("capabilities");
  assert.equal(response.kind, "error");
  assert.equal(response.error.code, "checker_crash");
  assert.equal(response.error.message, "checker worker failed");
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
