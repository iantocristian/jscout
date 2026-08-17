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

function usesGlibc() {
  const report =
    typeof process.report?.getReport === "function"
      ? process.report.getReport()
      : null;
  return typeof report?.header?.glibcVersionRuntime === "string";
}

function platformKey() {
  const { platform, arch } = process;
  if (platform === "darwin" && arch === "arm64") return "darwin-arm64";
  if (platform === "darwin" && arch === "x64") return "darwin-x64";
  if (platform === "linux" && (arch === "x64" || arch === "arm64")) {
    const cpu = arch === "x64" ? "x64" : "arm64";
    return `linux-${cpu}-${usesGlibc() ? "gnu" : "musl"}`;
  }
  return null;
}

function fail(message) {
  process.stderr.write(`jscout: ${message}\n`);
  process.exit(1);
}

const key = platformKey();
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

// Sidecar overrides. An explicit value from the caller always wins.
const env = { ...process.env };
const sidecars = [
  ["JSCOUT_PI_AI_GATEWAY", path.join(packageRoot, "gateway", "src", "main.mjs")],
  ["JSCOUT_CHECKER_SIDECAR", path.join(packageRoot, "checker", "src", "main.mjs")],
];
for (const [variable, entry] of sidecars) {
  if (!env[variable]?.trim() && fs.existsSync(entry)) env[variable] = entry;
}

const child = spawn(binary, process.argv.slice(2), { stdio: "inherit", env });

// Forward terminal signals rather than dying first and orphaning the child.
// The Rust binary installs its own handler and needs to shut down cleanly,
// which matters most for `jscout watch` and the stdio MCP server.
const forwarded = ["SIGINT", "SIGTERM", "SIGHUP"];
for (const signal of forwarded) {
  process.on(signal, () => {
    if (!child.killed) child.kill(signal);
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
