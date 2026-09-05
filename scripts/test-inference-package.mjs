#!/usr/bin/env node
// Exercise inference discovery through an installed archive binary or npm
// launcher, outside the source checkout and without downloading Python/models.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

const entry = fs.realpathSync(process.argv[2]);
const npm = entry.endsWith(".mjs");
const command = npm ? process.execPath : entry;
const prefix = npm ? [entry] : [];
const packageRoot = npm ? path.dirname(path.dirname(entry)) : path.dirname(entry);
const expectedProject = path.join(packageRoot, "inference");
const temporary = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), "jscout-inference-package-")));

try {
  assert.deepEqual(fs.readdirSync(expectedProject).sort(), ["pyproject.toml", "service.py", "uv.lock"]);
  const repository = path.join(temporary, "repository");
  fs.mkdirSync(repository);
  const uv = path.join(temporary, "uv");
  fs.writeFileSync(uv, `#!${process.execPath}
process.stdout.write(JSON.stringify({
  args: process.argv.slice(2),
  environment: process.env.UV_PROJECT_ENVIRONMENT,
}));
`);
  fs.chmodSync(uv, 0o755);
  const configure = (extra = "") => fs.writeFileSync(
    path.join(repository, ".jscout.toml"),
    `version = 1\n[inference]\nuv = ${JSON.stringify(uv)}\n${extra}`,
  );
  const run = (args = [], environment = "") => {
    const result = spawnSync(command, [...prefix, "inference", "serve", ...args], {
      cwd: repository,
      encoding: "utf8",
      env: {
        ...process.env,
        JSCOUT_BUNDLED_INFERENCE_PROJECT: "",
        JSCOUT_INFERENCE_PROJECT: "",
        XDG_CACHE_HOME: path.join(temporary, "cache"),
        UV_PROJECT_ENVIRONMENT: environment,
      },
    });
    assert.equal(result.status, 0, result.stderr);
    return JSON.parse(result.stdout);
  };

  configure();
  const bundled = run();
  assert.deepEqual(bundled.args, ["run", "--project", expectedProject, "--locked", "python", path.join(expectedProject, "service.py")]);
  assert.match(bundled.environment, /\/jscout\/inference\/[a-f0-9]{64}$/);
  assert.ok(bundled.environment.startsWith(path.join(temporary, "cache")));
  const customEnvironment = path.join(temporary, "custom-environment");
  assert.equal(run([], customEnvironment).environment, customEnvironment);

  const customProject = path.join(temporary, "custom-project");
  fs.mkdirSync(customProject);
  fs.writeFileSync(path.join(customProject, "pyproject.toml"), "[project]\n");
  fs.writeFileSync(path.join(customProject, "service.py"), "");
  const expectedCustomArgs = ["run", "--project", customProject, "python", path.join(customProject, "service.py")];
  const explicit = run(["--project", customProject]);
  assert.deepEqual(explicit.args, expectedCustomArgs);
  assert.equal(explicit.environment, "");
  configure(`project = ${JSON.stringify(customProject)}\n`);
  assert.deepEqual(run().args, expectedCustomArgs);
  process.stdout.write(`inference package smoke passed: ${entry}\n`);
} finally {
  fs.rmSync(temporary, { recursive: true, force: true });
}
