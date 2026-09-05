import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repoRoot = fileURLToPath(new URL("../../../", import.meta.url));

test("npm ships locked inference sources without environments or install hooks", (context) => {
  const cache = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-package-test-"));
  context.after(() => fs.rmSync(cache, { recursive: true, force: true }));
  execFileSync(process.execPath, ["scripts/npm-package.mjs", "--wrapper-only"], {
    cwd: repoRoot,
    stdio: "pipe",
  });
  const wrapper = path.join(repoRoot, "target", "npm", "cli");
  const [packed] = JSON.parse(execFileSync("npm", [
    "pack", "--dry-run", "--ignore-scripts", "--json", "--cache", cache, wrapper,
  ], { cwd: cache, encoding: "utf8" }));
  assert.deepEqual(packed.files.map(({ path: name }) => name).filter((name) => name.startsWith("inference/")).sort(), [
    "inference/pyproject.toml",
    "inference/service.py",
    "inference/uv.lock",
  ]);
  const manifest = JSON.parse(fs.readFileSync(path.join(wrapper, "package.json"), "utf8"));
  for (const hook of ["preinstall", "install", "postinstall"]) {
    assert.equal(manifest.scripts?.[hook], undefined);
  }
  for (const file of ["pyproject.toml", "service.py", "uv.lock"]) {
    assert.deepEqual(fs.readFileSync(path.join(wrapper, "inference", file)), fs.readFileSync(path.join(repoRoot, "inference", file)));
  }
});
