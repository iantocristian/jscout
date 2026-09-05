import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { once } from "node:events";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import readline from "node:readline";
import test from "node:test";
import { fileURLToPath } from "node:url";

const sourceLauncher = fileURLToPath(new URL("../bin/jscout.mjs", import.meta.url));

function platformKey() {
  if (process.platform === "darwin" && process.arch === "arm64") return "darwin-arm64";
  if (process.platform === "darwin" && process.arch === "x64") return "darwin-x64";
  if (process.platform === "linux" && ["x64", "arm64"].includes(process.arch)) {
    const version = process.report.getReport()?.header?.glibcVersionRuntime;
    return `linux-${process.arch}-${typeof version === "string" ? "gnu" : "musl"}`;
  }
  throw new Error(`unsupported test platform ${process.platform}-${process.arch}`);
}

function temporaryLauncher() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "jscout-launcher-test-"));
  const launcher = path.join(root, "cli", "bin", "jscout.mjs");
  fs.mkdirSync(path.dirname(launcher), { recursive: true });
  fs.copyFileSync(sourceLauncher, launcher);
  return { root, launcher };
}

function withTimeout(promise, message) {
  let timeout;
  const expired = new Promise((_, reject) => {
    timeout = setTimeout(() => reject(new Error(message)), 5_000);
  });
  return Promise.race([promise, expired]).finally(() => clearTimeout(timeout));
}

function installFakePlatformBinary(root, source) {
  const packageRoot = path.join(root, "cli", "node_modules", "@jscout", platformKey());
  fs.mkdirSync(packageRoot, { recursive: true });
  fs.writeFileSync(
    path.join(packageRoot, "package.json"),
    `${JSON.stringify({ name: `@jscout/${platformKey()}`, version: "0.0.0" })}\n`,
  );
  const binary = path.join(packageRoot, "jscout");
  fs.writeFileSync(binary, source);
  fs.chmodSync(binary, 0o755);
  return binary;
}

test("passes bundled sidecars without impersonating legacy user overrides", (context) => {
  const { root, launcher } = temporaryLauncher();
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  installFakePlatformBinary(
    root,
    `#!/usr/bin/env node
process.stdout.write(JSON.stringify({
  bundledGateway: process.env.JSCOUT_BUNDLED_GATEWAY,
  bundledChecker: process.env.JSCOUT_BUNDLED_CHECKER,
  legacyGateway: process.env.JSCOUT_PI_AI_GATEWAY,
}));
`,
  );
  for (const entry of ["gateway/src/main.mjs", "checker/src/main.mjs"]) {
    const file = path.join(root, "cli", entry);
    fs.mkdirSync(path.dirname(file), { recursive: true });
    fs.writeFileSync(file, "");
  }

  const clean = spawnSync(process.execPath, [launcher], {
    encoding: "utf8",
    env: {
      ...process.env,
      JSCOUT_PI_AI_GATEWAY: "",
      JSCOUT_CHECKER_SIDECAR: "",
      JSCOUT_BUNDLED_GATEWAY: "",
      JSCOUT_BUNDLED_CHECKER: "",
    },
  });
  assert.equal(clean.status, 0, clean.stderr);
  const cleanEnvironment = JSON.parse(clean.stdout);
  assert.match(cleanEnvironment.bundledGateway, /gateway\/src\/main\.mjs$/);
  assert.match(cleanEnvironment.bundledChecker, /checker\/src\/main\.mjs$/);
  assert.equal(cleanEnvironment.legacyGateway, "");

  const overridden = spawnSync(process.execPath, [launcher], {
    encoding: "utf8",
    env: {
      ...process.env,
      JSCOUT_PI_AI_GATEWAY: "/operator/gateway.mjs",
      JSCOUT_BUNDLED_GATEWAY: "",
    },
  });
  assert.equal(overridden.status, 0, overridden.stderr);
  const overriddenEnvironment = JSON.parse(overridden.stdout);
  assert.equal(overriddenEnvironment.legacyGateway, "/operator/gateway.mjs");
  assert.equal(overriddenEnvironment.bundledGateway, "");
});

test("identifies this installed launcher, Node, and inference bundle", (context) => {
  const { root, launcher } = temporaryLauncher();
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));
  installFakePlatformBinary(
    root,
    `#!/usr/bin/env node
process.stdout.write(JSON.stringify({
  launcher: process.env.JSCOUT_BUNDLED_LAUNCHER,
  node: process.env.JSCOUT_BUNDLED_NODE,
  inference: process.env.JSCOUT_BUNDLED_INFERENCE_PROJECT,
}));
`,
  );
  const result = spawnSync(process.execPath, [launcher], {
    encoding: "utf8",
    env: {
      ...process.env,
      JSCOUT_BUNDLED_LAUNCHER: "/stale/install/launcher.mjs",
      JSCOUT_BUNDLED_NODE: "/stale/install/node",
      JSCOUT_BUNDLED_INFERENCE_PROJECT: "/stale/install/inference",
    },
  });
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), {
    launcher: fs.realpathSync(launcher),
    node: process.execPath,
    inference: path.join(fs.realpathSync(root), "cli", "inference"),
  });
});

test("forwards repeated SIGINT while the child is still running", async (context) => {
  const { root, launcher } = temporaryLauncher();
  context.after(() => fs.rmSync(root, { recursive: true, force: true }));

  installFakePlatformBinary(
    root,
    `#!/usr/bin/env node
let interrupts = 0;
process.on("SIGINT", () => {
  interrupts += 1;
  process.stdout.write(\`interrupt:\${interrupts}\\n\`);
  if (interrupts === 2) process.exit(130);
});
process.stdout.write("ready\\n");
setInterval(() => {}, 1_000);
`,
  );

  const child = spawn(process.execPath, [launcher], {
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  context.after(() => {
    if (child.exitCode === null && child.signalCode === null) {
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch (error) {
        if (error.code !== "ESRCH") throw error;
      }
    }
  });

  let stderr = "";
  child.stderr.setEncoding("utf8");
  child.stderr.on("data", (chunk) => {
    stderr += chunk;
  });
  const lines = readline.createInterface({ input: child.stdout });
  const iterator = lines[Symbol.asyncIterator]();
  const nextLine = async () => {
    const result = await withTimeout(iterator.next(), `launcher output timed out: ${stderr}`);
    assert.equal(result.done, false);
    return result.value;
  };
  const exit = once(child, "exit");

  assert.equal(await nextLine(), "ready");
  assert.equal(child.kill("SIGINT"), true);
  assert.equal(await nextLine(), "interrupt:1");
  assert.equal(child.kill("SIGINT"), true);
  assert.equal(await nextLine(), "interrupt:2");
  assert.deepEqual(await withTimeout(exit, `launcher did not exit: ${stderr}`), [130, null]);
});

test(
  "rejects a GNU/Linux runtime below the packaged glibc floor",
  { skip: process.platform !== "linux" },
  (context) => {
    const { root, launcher } = temporaryLauncher();
    context.after(() => fs.rmSync(root, { recursive: true, force: true }));
    const preload = path.join(root, "glibc-2.30.mjs");
    fs.writeFileSync(
      preload,
      'process.report.getReport = () => ({ header: { glibcVersionRuntime: "2.30" } });\n',
    );

    const result = spawnSync(process.execPath, ["--import", preload, launcher], {
      encoding: "utf8",
    });
    assert.equal(result.status, 1);
    assert.match(
      result.stderr,
      /prebuilt GNU\/Linux binary requires glibc >= 2\.31; running 2\.30/,
    );
  },
);
