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
    async close() {
      await request("shutdown");
      child.stdin.end();
      await once(child, "exit");
    },
  };
}

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
    "tsconfig.json": JSON.stringify({ files: ["main.ts", "ambient.d.ts"] }),
    "nested/tsconfig.app.json": JSON.stringify({ files: ["../main.ts", "../ambient.d.ts"] }),
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
  fs.writeFileSync(path.join(root, "ambient.d.ts"), "declare const ambientVersion: 2;\n");
  const validation = await checker.request("validate_inputs", { entries });
  assert.equal(validation.kind, "validate_inputs_result");
  assert.equal(validation.result.valid, false);
  assert.ok(validation.result.results.every((result) => !result.valid));
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
