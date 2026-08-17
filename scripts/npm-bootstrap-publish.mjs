#!/usr/bin/env node
// One-time bootstrap publish of the npm packages from a workstation.
//
//   node scripts/npm-bootstrap-publish.mjs --run-id 123456   # pull CI artifacts
//   node scripts/npm-bootstrap-publish.mjs --from DIR        # already downloaded
//   node scripts/npm-bootstrap-publish.mjs --run-id 123 --dry-run
//
// Trusted publishing cannot perform a package's first publish: the trusted
// publisher is configured per package at npmjs.com/package/<name>/access,
// which requires the package to exist (npm/cli#8544). This script publishes
// the initial version of all five packages interactively, so no npm token is
// ever created. Every release after this one goes through
// .github/workflows/release-npm.yml over OIDC.
//
// npm prompts for the 2FA one-time password on its own; this script never
// sees it and never handles a credential.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const outputRoot = path.join(repoRoot, "target", "npm");

// Each platform package must contain a binary actually built for its target.
// A cross-compile that silently produced the host architecture would install
// cleanly and then fail on every user's machine.
const EXPECTED_ARCHITECTURE = new Map([
  ["darwin-arm64", /Mach-O 64-bit .*arm64/u],
  ["darwin-x64", /Mach-O 64-bit .*x86_64/u],
  ["linux-x64-gnu", /ELF 64-bit LSB .*x86-64/u],
  ["linux-arm64-gnu", /ELF 64-bit LSB .*(aarch64|ARM aarch64)/u],
]);

function die(message) {
  process.stderr.write(`bootstrap: ${message}\n`);
  process.exit(1);
}

function run(command, args, options = {}) {
  return execFileSync(command, args, { encoding: "utf8", ...options });
}

function parseArgs(argv) {
  const options = { runId: null, from: null, dryRun: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--dry-run") options.dryRun = true;
    else if (argument === "--run-id") options.runId = argv[++index] ?? null;
    else if (argument === "--from") options.from = argv[++index] ?? null;
    else die(`unrecognized argument: ${argument}`);
  }
  if (options.runId === null && options.from === null) {
    die("pass --run-id <workflow run id> or --from <directory>");
  }
  if (options.runId !== null && options.from !== null) {
    die("--run-id and --from are mutually exclusive");
  }
  return options;
}

function cargoVersion() {
  const manifest = fs.readFileSync(path.join(repoRoot, "Cargo.toml"), "utf8");
  const version = manifest.match(/^version = "([^"]+)"/mu)?.[1];
  if (!version) die("could not read version from Cargo.toml");
  return version;
}

function collectArtifacts(options) {
  // Clear the publish tree first. A platform directory left behind by an
  // earlier run would otherwise survive into this one and be published with
  // whatever binary it happens to hold.
  fs.rmSync(outputRoot, { recursive: true, force: true });
  fs.mkdirSync(outputRoot, { recursive: true });

  let source;
  if (options.runId !== null) {
    // gh run download nests each artifact under its own name, so stage it
    // outside target/npm and flatten from there rather than downloading
    // straight into the publish tree.
    source = path.join(repoRoot, "target", "npm-artifacts");
    fs.rmSync(source, { recursive: true, force: true });
    fs.mkdirSync(source, { recursive: true });
    process.stderr.write(`bootstrap: downloading artifacts from run ${options.runId}\n`);
    run(
      "gh",
      [
        "run", "download", options.runId,
        "--repo", "iantocristian/jscout",
        "--pattern", "npm-*",
        "--dir", source,
      ],
      { stdio: "inherit" },
    );
  } else {
    source = path.resolve(options.from);
    if (!fs.existsSync(source)) die(`no such directory: ${source}`);
  }

  // Accept either shape: npm-<target>/<platform>/ as gh produces it, or a
  // flat directory of <platform>/ directories.
  for (const entry of fs.readdirSync(source, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const from = path.join(source, entry.name);
    const nested = entry.name.startsWith("npm-")
      ? fs.readdirSync(from, { withFileTypes: true }).filter((d) => d.isDirectory())
      : [];
    if (nested.length > 0) {
      for (const platform of nested) {
        fs.cpSync(path.join(from, platform.name), path.join(outputRoot, platform.name), {
          recursive: true,
        });
      }
    } else {
      fs.cpSync(from, path.join(outputRoot, entry.name), { recursive: true });
    }
  }
}

function restoreExecutableBits() {
  for (const entry of fs.readdirSync(outputRoot, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const binary = path.join(outputRoot, entry.name, "jscout");
    if (fs.existsSync(binary)) fs.chmodSync(binary, 0o755);
  }
}

function preflight(version) {
  const problems = [];
  const platforms = fs
    .readdirSync(outputRoot, { withFileTypes: true })
    .filter((entry) => entry.isDirectory() && entry.name !== "cli")
    .map((entry) => entry.name)
    .sort();

  const wrapperManifest = path.join(outputRoot, "cli", "package.json");
  if (!fs.existsSync(wrapperManifest)) die("target/npm/cli was not assembled");
  const wrapper = JSON.parse(fs.readFileSync(wrapperManifest, "utf8"));

  const declared = Object.keys(wrapper.optionalDependencies ?? {})
    .map((name) => name.replace(/^@jscout\//u, ""))
    .sort();
  const missing = declared.filter((name) => !platforms.includes(name));
  const extra = platforms.filter((name) => !declared.includes(name));
  if (missing.length) problems.push(`wrapper declares but no artifact present: ${missing.join(", ")}`);
  if (extra.length) problems.push(`artifact present but wrapper does not declare: ${extra.join(", ")}`);
  if (wrapper.version !== version) {
    problems.push(`wrapper is ${wrapper.version}, Cargo.toml is ${version}`);
  }

  for (const platform of platforms) {
    const directory = path.join(outputRoot, platform);
    const manifest = JSON.parse(
      fs.readFileSync(path.join(directory, "package.json"), "utf8"),
    );
    if (manifest.version !== version) {
      problems.push(`${manifest.name} is ${manifest.version}, Cargo.toml is ${version}`);
    }

    const binary = path.join(directory, "jscout");
    if (!fs.existsSync(binary)) {
      problems.push(`${manifest.name} has no binary`);
      continue;
    }
    const stat = fs.statSync(binary);
    if ((stat.mode & 0o111) === 0) problems.push(`${manifest.name} binary is not executable`);
    if (stat.size < 1024 * 1024) {
      problems.push(`${manifest.name} binary is only ${stat.size} bytes`);
    }

    const expected = EXPECTED_ARCHITECTURE.get(platform);
    if (!expected) {
      problems.push(`no architecture expectation recorded for ${platform}`);
      continue;
    }
    const described = run("file", ["-b", binary]).trim();
    if (!expected.test(described)) {
      problems.push(`${manifest.name} binary is "${described}", expected ${expected.source}`);
    } else {
      process.stderr.write(`bootstrap: ok ${manifest.name} — ${described}\n`);
    }
  }

  for (const name of [...platforms.map((p) => `@jscout/${p}`), wrapper.name]) {
    try {
      run("npm", ["view", `${name}@${version}`, "version"], { stdio: "pipe" });
      problems.push(`${name}@${version} is already published`);
    } catch {
      // Not published: this is the expected state for a bootstrap.
    }
  }

  if (problems.length) {
    process.stderr.write("bootstrap: preflight failed\n");
    for (const problem of problems) process.stderr.write(`  - ${problem}\n`);
    process.exit(1);
  }
  return { platforms, wrapper };
}

const options = parseArgs(process.argv.slice(2));
const version = cargoVersion();

collectArtifacts(options);
process.stderr.write("bootstrap: assembling the wrapper\n");
run("node", [path.join(repoRoot, "scripts", "npm-package.mjs"), "--wrapper-only"], {
  stdio: "inherit",
});
restoreExecutableBits();

const { platforms, wrapper } = preflight(version);
process.stderr.write(
  `bootstrap: ${platforms.length} platform packages + ${wrapper.name}, all at ${version}\n`,
);

// Platform packages first: the wrapper's optionalDependencies must already
// resolve the moment anyone installs it.
const order = [...platforms.map((p) => path.join(outputRoot, p)), path.join(outputRoot, "cli")];

if (options.dryRun) {
  for (const directory of order) {
    run("npm", ["pack", "--dry-run", directory], { stdio: "inherit" });
  }
  process.stderr.write("bootstrap: dry run only, nothing published\n");
  process.exit(0);
}

// No --provenance: that requires a supported CI runner. Provenance starts
// with the first OIDC release. npm prompts for the OTP on its own.
for (const directory of order) {
  const name = JSON.parse(
    fs.readFileSync(path.join(directory, "package.json"), "utf8"),
  ).name;
  process.stderr.write(`\nbootstrap: publishing ${name}@${version}\n`);
  run("npm", ["publish", directory, "--access", "public"], { stdio: "inherit" });
}

process.stderr.write("\nbootstrap: published. Configure a trusted publisher for each:\n");
for (const directory of order) {
  const name = JSON.parse(
    fs.readFileSync(path.join(directory, "package.json"), "utf8"),
  ).name;
  process.stderr.write(`  https://www.npmjs.com/package/${name}/access\n`);
}
process.stderr.write(
  "\nEach one: repository iantocristian/jscout, workflow release-npm.yml.\n" +
    "After that, releases are `git tag vX.Y.Z` and nothing else.\n",
);
