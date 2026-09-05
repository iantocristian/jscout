#!/usr/bin/env node
// Launcher for the platform-specific jscout binary.
//
// The Rust binary normally discovers its Node sidecars beside its own
// executable (src/checker/mod.rs, src/llm/config.rs). Under npm the binary
// lives in a separate platform package, so that lookup would find nothing.
// This launcher points the documented overrides at the sidecars shipped in
// this package instead, then hands the process over.

import { spawn } from "node:child_process";
import { createRequire } from "node:module";
import { fileURLToPath } from "node:url";
import fs from "node:fs";
import path from "node:path";

const require = createRequire(import.meta.url);
const packageRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);
const MINIMUM_GLIBC = [2, 31];

function glibcVersion() {
  if (typeof process.report?.getReport !== "function") return null;
  try {
    const version = process.report.getReport()?.header?.glibcVersionRuntime;
    return typeof version === "string" ? version : null;
  } catch {
    return null;
  }
}

function supportsGlibc(version) {
  const [major, minor] = version
    .split(".", 2)
    .map((part) => Number.parseInt(part, 10));
  if (!Number.isInteger(major) || !Number.isInteger(minor)) return false;
  const [minimumMajor, minimumMinor] = MINIMUM_GLIBC;
  return major > minimumMajor || (major === minimumMajor && minor >= minimumMinor);
}

function platformKey(runtimeGlibc) {
  const { platform, arch } = process;
  if (platform === "darwin" && arch === "arm64") return "darwin-arm64";
  if (platform === "darwin" && arch === "x64") return "darwin-x64";
  if (platform === "linux" && (arch === "x64" || arch === "arm64")) {
    const cpu = arch === "x64" ? "x64" : "arm64";
    return `linux-${cpu}-${runtimeGlibc === null ? "musl" : "gnu"}`;
  }
  return null;
}

function fail(message) {
  process.stderr.write(`jscout: ${message}\n`);
  process.exit(1);
}

const runtimeGlibc = process.platform === "linux" ? glibcVersion() : null;
if (runtimeGlibc !== null && !supportsGlibc(runtimeGlibc)) {
  fail(
    `the prebuilt GNU/Linux binary requires glibc >= ${MINIMUM_GLIBC.join(".")}; ` +
      `running ${runtimeGlibc}. Build from source on this host instead`,
  );
}

const key = platformKey(runtimeGlibc);
if (key === null) {
  fail(
    `unsupported platform ${process.platform}-${process.arch}; ` +
      `build from source with \`cargo install --git https://github.com/iantocristian/jscout jscout\``,
  );
}

const platformPackage = `@jscout/${key}`;
let binary;
try {
  // Resolve through package.json: the binary itself is not a module, and
  // platform packages deliberately declare no `exports` map.
  const manifest = require.resolve(`${platformPackage}/package.json`);
  binary = path.join(
    path.dirname(manifest),
    process.platform === "win32" ? "jscout.exe" : "jscout",
  );
} catch {
  fail(
    `missing optional dependency ${platformPackage}. ` +
      `Reinstall without --no-optional, or check that your installer kept ` +
      `platform-matched optional dependencies.`,
  );
}

if (!fs.existsSync(binary)) {
  fail(`${platformPackage} is installed but contains no binary at ${binary}`);
}

// Bundled sidecar discovery transport. Repository config and the documented
// legacy override variables remain authoritative in the Rust process.
const env = { ...process.env };
// Setup uses this pair to register the installed wrapper, not the private
// platform binary. Absolute Node and launcher paths also work with GUI PATHs.
env.JSCOUT_BUNDLED_LAUNCHER = fileURLToPath(import.meta.url);
env.JSCOUT_BUNDLED_NODE = process.execPath;
// This is discovery, not a user override: --project and inference.project
// still take precedence in Rust. Always identify this wrapper's own bundle.
env.JSCOUT_BUNDLED_INFERENCE_PROJECT = path.join(packageRoot, "inference");
const sidecars = [
  [
    "JSCOUT_PI_AI_GATEWAY",
    "JSCOUT_BUNDLED_GATEWAY",
    path.join(packageRoot, "gateway", "src", "main.mjs"),
  ],
  [
    "JSCOUT_CHECKER_SIDECAR",
    "JSCOUT_BUNDLED_CHECKER",
    path.join(packageRoot, "checker", "src", "main.mjs"),
  ],
];
for (const [override, bundled, entry] of sidecars) {
  if (!env[override]?.trim() && !env[bundled]?.trim() && fs.existsSync(entry)) {
    env[bundled] = entry;
  }
}

const child = spawn(binary, process.argv.slice(2), { stdio: "inherit", env });

// Forward terminal signals rather than dying first and orphaning the child.
// The Rust binary installs its own handler and needs to shut down cleanly,
// which matters most for `jscout watch` and the stdio MCP server.
const forwarded = ["SIGINT", "SIGTERM", "SIGHUP"];
for (const signal of forwarded) {
  process.on(signal, () => {
    if (child.exitCode === null && child.signalCode === null) child.kill(signal);
  });
}

child.on("error", (error) => {
  fail(`could not start ${binary}: ${error.message}`);
});

child.on("exit", (code, signal) => {
  if (signal !== null) {
    // Re-raise so the parent's exit status reflects the child's. Removing our
    // forwarder restores Node's default disposition for the signal.
    process.removeAllListeners(signal);
    process.kill(process.pid, signal);
    return;
  }
  process.exit(code ?? 0);
});
