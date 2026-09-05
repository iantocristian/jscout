#!/usr/bin/env node
// Execute the documented onboarding path through a built archive or npm launcher.
// Everything lives in a temporary repository; never touches real client settings.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const entry = fs.realpathSync(process.argv[2]);
const npm = entry.endsWith(".mjs");
const command = npm ? process.execPath : entry;
const prefix = npm ? [entry] : [];
const temporary = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "jscout-setup-package-")));
const env = { HOME: temporary, PATH: "/usr/bin:/bin", TMPDIR: temporary };

function invoke(root, args) {
  const result = spawnSync(command, [...prefix, ...args], {
    cwd: root, env, encoding: "utf8", timeout: 30_000,
  });
  assert.equal(result.status, 0, `${result.error ?? ""}\n${result.stderr}\n${result.stdout}`);
  return result.stdout;
}

try {
  for (const client of ["codex", "claude"]) {
    const root = path.join(temporary, `${client} repository $cash 'quoted'`);
    fs.mkdirSync(root);
    fs.writeFileSync(path.join(root, "greeting.ts"), "export function greeting(name: string) { return `Hello ${name}`; }\n");
    fs.writeFileSync(path.join(root, "README.md"), "# Greeting\n\nReturn a friendly salutation.\n");
    const args = ["setup", ".", "--client", client];
    const preview = invoke(root, [...args, "--print-config"]);
    assert.ok(preview.includes("jscout"));
    assert.equal(fs.readdirSync(root).length, 2, "print-config must not create files");
    assert.match(invoke(root, args), /verified MCP initialization and 7 tools/);
    const configPath = path.join(root, client === "codex" ? ".codex/config.toml" : ".mcp.json");
    const registered = fs.readFileSync(configPath, "utf8");
    invoke(root, args);
    assert.equal(fs.readFileSync(configPath, "utf8"), registered, "rerun must preserve registration");
    assert.equal(fs.readFileSync(path.join(root, ".jscout.toml"), "utf8"), "version = 1\n");
    const code = JSON.parse(invoke(root, ["search", ".", "greeting", "--lexical-only", "--json"]));
    assert.equal(code.hits[0].symbol, "greeting");
    const docs = JSON.parse(invoke(root, ["docs", "search", ".", "salutation", "--lexical-only", "--json"]));
    assert.equal(docs.hits[0].path, "README.md");

    if (client === "claude") {
      // Refresh a removed installation through the actual packaged launcher.
      const outdated = JSON.parse(registered);
      outdated.mcpServers.jscout.command = "/removed-installation/jscout";
      outdated.mcpServers.jscout.args = ["mcp", root];
      outdated.mcpServers.jscout.env = { LANG: "C" };
      fs.writeFileSync(configPath, JSON.stringify(outdated));
      assert.match(invoke(root, [...args, "--replace"]), /verified MCP initialization and 7 tools/);
      // Launch the actual saved registration with no inherited JSCOUT_BUNDLED_*
      // or auth environment. This catches registering npm's bare platform binary.
      const server = JSON.parse(fs.readFileSync(configPath, "utf8")).mcpServers.jscout;
      assert.equal(server.command, command);
      assert.deepEqual(server.args, [...prefix, "mcp", root]);
      assert.deepEqual(server.env, { LANG: "C" });
      const requests = [
        { jsonrpc: "2.0", id: 1, method: "initialize", params: { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "package-smoke", version: "1" } } },
        { jsonrpc: "2.0", method: "notifications/initialized" },
        { jsonrpc: "2.0", id: 2, method: "tools/list", params: {} },
      ];
      const result = spawnSync(server.command, server.args, {
        cwd: temporary, env, encoding: "utf8", timeout: 15_000,
        input: requests.map((request) => JSON.stringify(request)).join("\n") + "\n",
      });
      assert.equal(result.status, 0, result.stderr);
      const responses = result.stdout.trim().split("\n").map((line) => JSON.parse(line));
      assert.equal(responses.find(({ id }) => id === 2).result.tools.length, 7);
    }
  }
  process.stdout.write(`setup package smoke passed: ${entry}\n`);
} finally {
  fs.rmSync(temporary, { recursive: true, force: true });
}
