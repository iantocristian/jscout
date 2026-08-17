#!/usr/bin/env node
// Assemble the npm publish tree under target/npm/.
//
//   node scripts/npm-package.mjs                    # host platform + wrapper
//   node scripts/npm-package.mjs --target TRIPLE    # cross-built platform + wrapper
//   node scripts/npm-package.mjs --platform-only    # platform alone (CI matrix job)
//   node scripts/npm-package.mjs --wrapper-only     # wrapper alone (CI publish job)
//
// Unlike scripts/package-release.sh, the wrapper vendors no node_modules: the
// sidecar dependencies are declared in npm/cli/package.json and resolved by
// the installer. Cargo.toml is the single source of version truth.

import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

const TARGETS = new Map([
  ["aarch64-apple-darwin", { key: "darwin-arm64", os: "darwin", cpu: "arm64" }],
  ["x86_64-apple-darwin", { key: "darwin-x64", os: "darwin", cpu: "x64" }],
  [
    "x86_64-unknown-linux-gnu",
    { key: "linux-x64-gnu", os: "linux", cpu: "x64", libc: "glibc" },
  ],
  [
    "aarch64-unknown-linux-gnu",
    { key: "linux-arm64-gnu", os: "linux", cpu: "arm64", libc: "glibc" },
  ],
]);

function die(message) {
  process.stderr.write(`npm-package: ${message}\n`);
  process.exit(1);
}

function parseArgs(argv) {
  const options = { target: null, wrapperOnly: false, platformOnly: false };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--wrapper-only") options.wrapperOnly = true;
    else if (argument === "--platform-only") options.platformOnly = true;
    else if (argument === "--target") options.target = argv[++index] ?? null;
    else if (argument.startsWith("--target=")) options.target = argument.slice(9);
    else die(`unrecognized argument: ${argument}`);
  }
  if (options.wrapperOnly && options.platformOnly) {
    die("--wrapper-only and --platform-only are mutually exclusive");
  }
  if (options.target === null && !options.wrapperOnly) {
    options.target = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
      .split("\n")
      .find((line) => line.startsWith("host:"))
      ?.slice(5)
      .trim();
  }
  return options;
}

function cargoVersion() {
  const manifest = fs.readFileSync(path.join(repoRoot, "Cargo.toml"), "utf8");
  const version = manifest.match(/^version = "([^"]+)"/mu)?.[1];
  if (!version) die("could not read version from Cargo.toml");
  return version;
}

function writeJson(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function copyInto(from, to) {
  fs.mkdirSync(path.dirname(to), { recursive: true });
  fs.cpSync(from, to, { recursive: true });
}

function buildPlatformPackage(target, version, outputRoot) {
  const descriptor = TARGETS.get(target);
  if (!descriptor) {
    die(
      `unsupported target ${target}; known targets: ${[...TARGETS.keys()].join(", ")}`,
    );
  }

  const host = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
    .split("\n")
    .find((line) => line.startsWith("host:"))
    ?.slice(5)
    .trim();
  // `cargo build --target X` writes to target/X/release even when X is the
  // host, so prefer that path and fall back to the plain host layout.
  const candidates = [path.join(repoRoot, "target", target, "release", "jscout")];
  if (target === host) candidates.push(path.join(repoRoot, "target", "release", "jscout"));
  const binary = candidates.find((candidate) => fs.existsSync(candidate));
  if (!binary) {
    die(
      `no release binary for ${target}; looked in ${candidates.join(", ")}. ` +
        `Run \`cargo build --locked --release --target ${target}\` first`,
    );
  }

  const stage = path.join(outputRoot, descriptor.key);
  fs.rmSync(stage, { recursive: true, force: true });
  fs.mkdirSync(stage, { recursive: true });

  fs.copyFileSync(binary, path.join(stage, "jscout"));
  fs.chmodSync(path.join(stage, "jscout"), 0o755);
  for (const license of ["LICENSE-MIT", "LICENSE-APACHE"]) {
    fs.copyFileSync(path.join(repoRoot, license), path.join(stage, license));
  }

  const manifest = {
    name: `@jscout/${descriptor.key}`,
    version,
    description: `jscout binary for ${descriptor.key}`,
    repository: {
      type: "git",
      url: "git+https://github.com/iantocristian/jscout.git",
    },
    license: "MIT OR Apache-2.0",
    os: [descriptor.os],
    cpu: [descriptor.cpu],
    ...(descriptor.libc ? { libc: [descriptor.libc] } : {}),
    files: ["jscout", "LICENSE-MIT", "LICENSE-APACHE"],
    preferUnplugged: true,
  };
  writeJson(path.join(stage, "package.json"), manifest);

  const bytes = fs.statSync(path.join(stage, "jscout")).size;
  process.stderr.write(
    `npm-package: ${manifest.name}@${version} (${(bytes / 1024 / 1024).toFixed(1)} MiB)\n`,
  );
  return stage;
}

function buildWrapperPackage(version, outputRoot) {
  const source = path.join(repoRoot, "npm", "cli");
  const stage = path.join(outputRoot, "cli");
  fs.rmSync(stage, { recursive: true, force: true });
  fs.mkdirSync(stage, { recursive: true });

  const manifest = JSON.parse(
    fs.readFileSync(path.join(source, "package.json"), "utf8"),
  );
  if (manifest.version !== version) {
    process.stderr.write(
      `npm-package: warning: npm/cli/package.json says ${manifest.version}, ` +
        `Cargo.toml says ${version}; using ${version}\n`,
    );
  }
  manifest.version = version;
  for (const name of Object.keys(manifest.optionalDependencies ?? {})) {
    manifest.optionalDependencies[name] = version;
  }
  writeJson(path.join(stage, "package.json"), manifest);

  copyInto(path.join(source, "bin"), path.join(stage, "bin"));
  fs.chmodSync(path.join(stage, "bin", "jscout.mjs"), 0o755);
  fs.copyFileSync(
    path.join(source, "README.md"),
    path.join(stage, "README.md"),
  );
  for (const license of ["LICENSE-MIT", "LICENSE-APACHE"]) {
    fs.copyFileSync(path.join(repoRoot, license), path.join(stage, license));
  }

  // Sidecar sources plus their own manifests: gateway/src/main.mjs reads
  // ../package.json for the version it reports. Dependencies are declared in
  // the wrapper manifest and resolved by the installer, so unlike
  // package-release.sh no node_modules tree is vendored here.
  for (const sidecar of ["gateway", "checker"]) {
    copyInto(
      path.join(repoRoot, sidecar, "src"),
      path.join(stage, sidecar, "src"),
    );
    fs.copyFileSync(
      path.join(repoRoot, sidecar, "package.json"),
      path.join(stage, sidecar, "package.json"),
    );
  }

  for (const [sidecar, expected] of [
    ["gateway", ["@earendil-works/pi-ai"]],
    ["checker", ["typescript", "@noble/hashes"]],
  ]) {
    const declared = JSON.parse(
      fs.readFileSync(path.join(repoRoot, sidecar, "package.json"), "utf8"),
    ).dependencies;
    for (const name of expected) {
      if (declared[name] !== manifest.dependencies[name]) {
        die(
          `${sidecar}/package.json pins ${name}@${declared[name]} but ` +
            `npm/cli/package.json pins ${name}@${manifest.dependencies[name]}`,
        );
      }
    }
  }

  process.stderr.write(`npm-package: ${manifest.name}@${version}\n`);
  return stage;
}

const options = parseArgs(process.argv.slice(2));
const version = cargoVersion();
const outputRoot = path.join(repoRoot, "target", "npm");
fs.mkdirSync(outputRoot, { recursive: true });

if (!options.wrapperOnly) buildPlatformPackage(options.target, version, outputRoot);
if (!options.platformOnly) buildWrapperPackage(version, outputRoot);
process.stdout.write(`${outputRoot}\n`);
