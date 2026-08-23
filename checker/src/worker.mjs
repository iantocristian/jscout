import crypto from "node:crypto";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { parentPort, workerData } from "node:worker_threads";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";
import { inferredOptions } from "./inferred-options.mjs";
import { PROTOCOL_VERSION } from "./protocol.mjs";

const root = fs.realpathSync(workerData.root);
const bundledRequire = createRequire(import.meta.url);

function loadTypeScript() {
  try {
    const repositoryRequire = createRequire(path.join(root, "package.json"));
    const resolved = repositoryRequire.resolve("typescript");
    return { ts: repositoryRequire(resolved), source: "repository", resolved };
  } catch {
    const resolved = bundledRequire.resolve("typescript");
    return { ts: bundledRequire(resolved), source: "bundled", resolved };
  }
}

const runtime = loadTypeScript();
const ts = runtime.ts;
// Production enrichment dedicates one worker process to one configured
// project. Keeping more than one Program here made peak memory proportional to
// every overlapping tsconfig encountered by a repository-wide run.
let builtProject;
const occurrenceCache = new WeakMap();
let discoveryCache;
const packageCache = new Map();
const INFERRED_ROOT_CAP = 150;
const INFERRED_FAMILIES = new Set(["node-esm", "node-cjs", "bundler-jsx"]);
const ABSENT_INPUT_HASH = "absent:v1";

function runtimeInputs() {
  const files = [runtime.resolved];
  let directory = path.dirname(runtime.resolved);
  while (directory !== path.dirname(directory)) {
    const packageFile = path.join(directory, "package.json");
    if (fs.existsSync(packageFile)) {
      try {
        if (JSON.parse(fs.readFileSync(packageFile, "utf8")).name === "typescript") {
          files.push(packageFile);
          break;
        }
      } catch {
        break;
      }
    }
    directory = path.dirname(directory);
  }
  return files.map((file) => {
    const canonical = fs.realpathSync(file);
    return {
      identity: `typescript:${path.basename(canonical)}`,
      path: canonical,
      source_hash: sourceHash(fs.readFileSync(canonical)),
    };
  });
}

function stable(value) {
  if (Array.isArray(value)) return value.map(stable);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, stable(value[key])]));
  }
  return value;
}

function compareText(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

function safeFailureMessage(failure, limit = 64 * 1024) {
  return String(failure?.message ?? "checker request failed")
    .replaceAll(root, "<repository>")
    .slice(0, limit);
}

function normalizeOption(value) {
  if (Array.isArray(value)) return value.map(normalizeOption);
  if (value && typeof value === "object") {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [key, normalizeOption(item)]));
  }
  if (typeof value === "string" && path.isAbsolute(value)) {
    if (insideRoot(value)) return `repository:${path.relative(root, value).split(path.sep).join("/")}`;
    return `outside:${digestText(value).slice(0, 24)}:${path.basename(value)}`;
  }
  return value;
}

// Canonical facts and agent-visible output stay repo-relative: strip the
// absolute repository prefix from `import("...")` type spellings and mask
// any other machine-absolute path.
function normalizeTypeText(text) {
  return text.replace(/import\("([^"]+)"\)/gu, (_, spec) => {
    if (!path.isAbsolute(spec)) return `import("${spec}")`;
    if (insideRoot(spec)) {
      return `import("${path.relative(root, spec).split(path.sep).join("/")}")`;
    }
    return `import("outside:${path.basename(spec)}")`;
  });
}

function digestText(text) {
  return crypto.createHash("sha256").update(text).digest("hex");
}

function sourceHash(value) {
  const bytes = typeof value === "string" ? new TextEncoder().encode(value) : value;
  return bytesToHex(blake3(bytes));
}

function insideRoot(file) {
  const relative = path.relative(root, file);
  return relative === "" || (!relative.startsWith(`..${path.sep}`) && relative !== ".." && !path.isAbsolute(relative));
}

function relativeIdentity(file) {
  const canonical = fs.realpathSync(file);
  if (insideRoot(canonical)) return path.relative(root, canonical).split(path.sep).join("/");
  return `outside:${digestText(canonical).slice(0, 24)}:${path.basename(canonical)}`;
}

// Parsed project file names are already absolute, normalized configuration
// outputs. Fingerprinting membership must not realpath every file in every
// overlapping project during the configuration-only inventory pass: large
// monorepos can own the same source through hundreds of tsconfigs.
function projectMemberIdentity(file) {
  const absolute = path.resolve(file);
  if (insideRoot(absolute)) return path.relative(root, absolute).split(path.sep).join("/");
  return `outside:${digestText(absolute).slice(0, 24)}:${path.basename(absolute)}`;
}

function resolveQueryFile(relativePath) {
  if (typeof relativePath !== "string" || relativePath.length === 0 || path.isAbsolute(relativePath)) {
    throw coded("outside_root", "query file must be a non-empty repository-relative path");
  }
  const lexical = path.resolve(root, relativePath);
  if (!insideRoot(lexical)) throw coded("outside_root", "query file escapes the repository root");
  let canonical;
  try {
    canonical = fs.realpathSync(lexical);
  } catch {
    throw coded("query_file_missing", "query file does not exist");
  }
  if (!insideRoot(canonical)) throw coded("outside_root", "query file resolves outside the repository root");
  if (!fs.statSync(canonical).isFile()) throw coded("query_file_missing", "query path is not a file");
  return canonical;
}

function coded(code, message) {
  return Object.assign(new Error(message), { code });
}

function walkConfigs(directory, output = []) {
  let entries;
  try {
    entries = fs.readdirSync(directory, { withFileTypes: true });
  } catch {
    return output;
  }
  entries.sort((left, right) => left.name.localeCompare(right.name));
  for (const entry of entries) {
    if (entry.isDirectory()) {
      if ([".git", ".jscout", ".worktrees", "node_modules"].includes(entry.name)) continue;
      walkConfigs(path.join(directory, entry.name), output);
    } else if (entry.isFile() && /^tsconfig(?:\..+)?\.json$/u.test(entry.name)) {
      output.push(path.join(directory, entry.name));
    }
  }
  return output;
}

function configProblem(config, error) {
  return {
    project_id: path.relative(root, config).split(path.sep).join("/"),
    code: "config",
    message: ts.flattenDiagnosticMessageText(error.messageText, " "),
  };
}

function nearestPackageScripts(config) {
  let directory = path.dirname(config);
  while (insideRoot(directory)) {
    const manifest = path.join(directory, "package.json");
    if (fs.existsSync(manifest)) {
      try {
        const scripts = JSON.parse(fs.readFileSync(manifest, "utf8")).scripts;
        return {
          directory,
          scripts: scripts && typeof scripts === "object" ? scripts : {},
        };
      } catch {
        return { directory, scripts: {} };
      }
    }
    if (directory === root) break;
    directory = path.dirname(directory);
  }
  return { directory: root, scripts: {} };
}

function commandReferencesConfig(command, config, packageDirectory) {
  const normalized = command.replaceAll("\\", "/");
  const relative = path.relative(packageDirectory, config).split(path.sep).join("/");
  if (relative.length === 0 || relative === ".." || relative.startsWith("../")) return false;
  const escaped = relative.replace(/[.*+?^${}()|[\]\\]/gu, "\\$&");
  return new RegExp(
    `(?:^|[\\s"'=])(?:\\./)?${escaped}(?=$|[\\s"';&|)])`,
    "u",
  ).test(normalized);
}

function projectPurpose(config, rawConfig, parsed) {
  const basename = path.basename(config).toLowerCase();
  const filenameTokens = basename.replace(/\.json$/u, "").split(/[._-]+/u);
  const reasons = [];
  if (filenameTokens.includes("eslint") || filenameTokens.includes("lint")) {
    reasons.push("tooling-filename");
  }

  const inherited = Array.isArray(rawConfig.extends)
    ? rawConfig.extends
    : rawConfig.extends ? [rawConfig.extends] : [];
  if (inherited.some(
    (value) => typeof value === "string"
      && /(?:^|[._/@-])(?:eslint|lint)(?:$|[._/@-])/iu.test(value),
  )) {
    reasons.push("tooling-extends");
  }

  const packageScripts = nearestPackageScripts(config);
  const scriptReferences = Object.entries(packageScripts.scripts)
    .filter(([name, command]) => {
      if (
        typeof command !== "string"
        || !/(?:^|[:._-])(?:eslint|lint)(?:$|[:._-])/iu.test(name)
      ) {
        return false;
      }
      return commandReferencesConfig(command, config, packageScripts.directory);
    })
    .map(([name]) => name)
    .sort();
  // noEmit is common in legitimate runtime/typecheck projects, so it can only
  // corroborate an explicit tooling script; it never classifies a project by
  // itself. Lint-labelled filenames/extends are already high-confidence.
  const scriptedNoEmit = scriptReferences.length > 0
    && parsed.options.noEmit === true;
  if (scriptedNoEmit) reasons.push(`tooling-script:${scriptReferences.join(",")}`);
  const explicitTooling = reasons.includes("tooling-filename")
    || reasons.includes("tooling-extends");
  return {
    purpose: explicitTooling || scriptedNoEmit ? "tooling" : "general",
    purposeReasons: reasons,
  };
}

function nearestPackage(file) {
  let directory = path.dirname(file);
  const visited = [];
  while (insideRoot(directory)) {
    const cached = packageCache.get(directory);
    if (cached) {
      for (const candidate of visited) packageCache.set(candidate, cached);
      return cached;
    }
    visited.push(directory);
    const manifest = path.join(directory, "package.json");
    if (fs.existsSync(manifest) && fs.statSync(manifest).isFile()) {
      let type = "commonjs";
      try {
        if (JSON.parse(fs.readFileSync(manifest, "utf8")).type === "module") type = "module";
      } catch {
        // TypeScript treats a malformed/absent package type as non-ESM for
        // ordinary .js/.ts roots. The manifest remains a fingerprinted input,
        // so repairing it invalidates the planned scope.
      }
      const record = { directory, manifest, type };
      for (const candidate of visited) packageCache.set(candidate, record);
      return record;
    }
    if (directory === root) break;
    directory = path.dirname(directory);
  }
  const record = { directory: root, manifest: undefined, type: "commonjs" };
  for (const candidate of visited) packageCache.set(candidate, record);
  return record;
}

function inferredFamily(file, packageType) {
  const lower = file.toLowerCase();
  if (lower.endsWith(".jsx") || lower.endsWith(".tsx")) return "bundler-jsx";
  if (lower.endsWith(".mjs") || lower.endsWith(".mts")) return "node-esm";
  if (lower.endsWith(".cjs") || lower.endsWith(".cts")) return "node-cjs";
  return packageType === "module" ? "node-esm" : "node-cjs";
}

function packageIdentity(directory) {
  const relative = path.relative(root, directory).split(path.sep).join("/");
  return relative.length === 0 ? "." : relative;
}

function inferredPartIdentity(top, index) {
  // Keep the root bucket distinct from a literal `root` directory, and encode
  // the bin delimiter so directory names such as `tests-1` cannot collide with
  // a generated second bin for `tests`.
  const label = top === ""
    ? "@root"
    : encodeURIComponent(top).replaceAll("~", "%7E");
  return `${label}~${index + 1}`;
}

function splitEvenly(values, cap = INFERRED_ROOT_CAP) {
  if (values.length <= cap) return [values];
  const partCount = Math.ceil(values.length / cap);
  const minimum = Math.floor(values.length / partCount);
  let extras = values.length % partCount;
  const parts = [];
  let offset = 0;
  for (let index = 0; index < partCount; index += 1) {
    const size = minimum + (extras > 0 ? 1 : 0);
    extras -= extras > 0 ? 1 : 0;
    parts.push(values.slice(offset, offset + size));
    offset += size;
  }
  return parts;
}

function boundedDirectoryUnits(directory, records) {
  const direct = [];
  const children = new Map();
  for (const record of records) {
    const relative = path.relative(directory, record.file);
    const [head, ...tail] = relative.split(path.sep);
    if (tail.length === 0) {
      direct.push(record);
    } else {
      const child = children.get(head) ?? [];
      child.push(record);
      children.set(head, child);
    }
  }
  direct.sort((left, right) => compareText(left.file, right.file));
  const units = direct.length > 0 ? splitEvenly(direct) : [];
  for (const name of [...children.keys()].sort()) {
    const child = children.get(name).sort((left, right) => compareText(left.file, right.file));
    if (child.length <= INFERRED_ROOT_CAP) {
      units.push(child);
    } else {
      units.push(...boundedDirectoryUnits(path.join(directory, name), child));
    }
  }
  return units;
}

function packDirectory(records, directory) {
  const bins = [];
  let current = [];
  for (const unit of boundedDirectoryUnits(directory, records)) {
    if (current.length > 0 && current.length + unit.length > INFERRED_ROOT_CAP) {
      bins.push(current);
      current = [];
    }
    current.push(...unit);
  }
  if (current.length > 0) bins.push(current);
  return bins;
}

function inferredProject(id, family, packageRecord, records) {
  const absentManifests = new Set();
  for (const record of records) {
    let directory = path.dirname(record.file);
    while (insideRoot(directory)) {
      const manifest = path.join(directory, "package.json");
      if (manifest === packageRecord.manifest) break;
      // A dangling symlink is not a TypeScript package boundary, but it is
      // still a lexical directory entry. Do not record it as an absent probe:
      // validation below re-runs nearestPackage and will invalidate the scope
      // if the target later appears and makes the boundary effective.
      if (fs.lstatSync(manifest, { throwIfNoEntry: false }) === undefined) {
        absentManifests.add(manifest);
      }
      if (directory === packageRecord.directory || directory === root) break;
      directory = path.dirname(directory);
    }
  }
  return {
    id,
    config: undefined,
    manifest: packageRecord.manifest,
    absentManifests: [...absentManifests].sort(),
    packageDirectory: packageRecord.directory,
    family,
    options: inferredOptions(ts, family),
    fileNames: records.map((record) => record.file).sort(),
    projectReferences: undefined,
    purpose: "inferred",
    purposeReasons: ["no-configured-owner", `compiler-family:${family}`],
  };
}

function groupedInferredProjects(files) {
  const groups = new Map();
  for (const file of [...new Set(files)].sort()) {
    const packageRecord = nearestPackage(file);
    const family = inferredFamily(file, packageRecord.type);
    const key = `${packageRecord.directory}\0${family}`;
    const group = groups.get(key) ?? { packageRecord, family, records: [] };
    group.records.push({ file });
    groups.set(key, group);
  }

  const projects = [];
  const projectByFile = new Map();
  const orderedGroups = [...groups.values()].sort((left, right) => (
    compareText(
      packageIdentity(left.packageRecord.directory),
      packageIdentity(right.packageRecord.directory),
    ) || compareText(left.family, right.family)
  ));
  for (const group of orderedGroups) {
    const prefix = `inferred:${packageIdentity(group.packageRecord.directory)}#${group.family}`;
    group.records.sort((left, right) => compareText(left.file, right.file));
    const scopes = [];
    if (group.records.length <= INFERRED_ROOT_CAP) {
      scopes.push({ id: prefix, records: group.records });
    } else {
      const byTopDirectory = new Map();
      for (const record of group.records) {
        const relative = path.relative(group.packageRecord.directory, record.file);
        const parts = relative.split(path.sep);
        const top = parts.length === 1 ? "" : parts[0];
        const values = byTopDirectory.get(top) ?? [];
        values.push(record);
        byTopDirectory.set(top, values);
      }
      for (const top of [...byTopDirectory.keys()].sort()) {
        const records = byTopDirectory.get(top);
        const directory = top === ""
          ? group.packageRecord.directory
          : path.join(group.packageRecord.directory, top);
        const bins = packDirectory(records, directory);
        for (const [index, bin] of bins.entries()) {
          const part = inferredPartIdentity(top, index);
          scopes.push({ id: `${prefix}/${part}`, records: bin });
        }
      }
    }
    for (const scope of scopes) {
      const project = inferredProject(scope.id, group.family, group.packageRecord, scope.records);
      projects.push(project);
      for (const record of scope.records) projectByFile.set(record.file, project);
    }
  }
  projects.sort((left, right) => compareText(left.id, right.id));
  return { projects, projectByFile };
}

function inferredProjectFromRequest(projectId, files) {
  if (!projectId.startsWith("inferred:") || !Array.isArray(files) || files.length === 0) {
    throw coded("protocol", "inferred resolve_members requires a non-empty project_files list");
  }
  if (files.length > INFERRED_ROOT_CAP) {
    throw coded("protocol", `inferred project exceeds its ${INFERRED_ROOT_CAP}-root cap`);
  }
  const identity = projectId.slice("inferred:".length);
  const separator = identity.lastIndexOf("#");
  if (separator < 0) throw coded("project_not_found", `invalid inferred project id: ${projectId}`);
  const packageId = identity.slice(0, separator);
  const family = identity.slice(separator + 1).split("/", 1)[0];
  if (!INFERRED_FAMILIES.has(family)) {
    throw coded("project_not_found", `invalid inferred compiler family: ${projectId}`);
  }
  const canonical = [...new Set(files.map(resolveQueryFile))].sort();
  if (canonical.length !== files.length) {
    throw coded("protocol", "inferred project_files must be unique");
  }
  const packageRecords = canonical.map(nearestPackage);
  for (const [index, file] of canonical.entries()) {
    if (
      packageIdentity(packageRecords[index].directory) !== packageId
      || inferredFamily(file, packageRecords[index].type) !== family
    ) {
      throw coded("project_mismatch", `${path.relative(root, file)} is not compatible with ${projectId}`);
    }
  }
  return inferredProject(projectId, family, packageRecords[0], canonical.map((file) => ({ file })));
}

function projectConfigInputs(project) {
  const inputs = project.config ? configInputs(project.config) : [];
  if (project.manifest) {
    inputs.push({
      // Preserve the lexical package boundary as the observed input. Hashing
      // only its real target would miss a symlink retarget while the old target
      // remained unchanged.
      identity: relativeIdentity(project.manifest),
      path: project.manifest,
      source_hash: sourceHash(fs.readFileSync(project.manifest)),
    });
  }
  for (const absent of project.absentManifests ?? []) {
    inputs.push({
      identity: `absent:${path.relative(root, absent).split(path.sep).join("/")}`,
      path: absent,
      source_hash: ABSENT_INPUT_HASH,
    });
  }
  return inputs
    .filter((value, index, all) => all.findIndex((other) => other.path === value.path) === index)
    .sort((left, right) => compareText(left.identity, right.identity));
}

function projectSummary(project) {
  project.membershipFingerprint ??= digestText(
    [...project.fileNames].map((file) => projectMemberIdentity(file)).sort().join("\0"),
  );
  const inputs = projectConfigInputs(project)
    .map(({ identity, source_hash }) => ({ identity, source_hash }));
  // Configured-project fingerprints remain byte-compatible with the previous
  // input-only shape. Inferred projects have no tsconfig, so their effective
  // family options are themselves a configuration input and must invalidate
  // cached enrichment when those semantics change.
  const configIdentity = project.config
    ? inputs
    : stable({ inputs, options: normalizeOption(project.options) });
  project.configFingerprint ??= digestText(JSON.stringify(configIdentity));
  return {
    project_id: project.id,
    file_count: project.fileNames.length,
    purpose: project.purpose ?? "general",
    purpose_reasons: project.purposeReasons ?? [],
    membership_fingerprint: project.membershipFingerprint,
    config_fingerprint: project.configFingerprint,
  };
}

function configuredProjects(force = false) {
  if (!force && discoveryCache) return discoveryCache;
  const projects = [];
  const problems = [];
  const records = walkConfigs(root).map((config) => ({
    config,
    canonical: fs.realpathSync(config),
    read: ts.readConfigFile(config, ts.sys.readFile),
  }));
  const inheritedBases = new Set();
  for (const record of records) {
    if (record.read.error) continue;
    const inherited = Array.isArray(record.read.config.extends)
      ? record.read.config.extends
      : record.read.config.extends ? [record.read.config.extends] : [];
    for (const specifier of inherited) {
      if (typeof specifier !== "string") continue;
      const resolved = resolveExtendedConfig(specifier, record.config);
      if (resolved) inheritedBases.add(fs.realpathSync(resolved));
    }
  }
  for (const { config, canonical, read } of records) {
    if (read.error) {
      problems.push(configProblem(config, read.error));
      continue;
    }
    const explicitlySelectsFiles = read.config.files !== undefined
      || read.config.include !== undefined
      || read.config.references !== undefined;
    if (inheritedBases.has(canonical) && !explicitlySelectsFiles) continue;
    const parsed = ts.parseJsonConfigFileContent(read.config, ts.sys, path.dirname(config), undefined, config);
    if (parsed.errors.length > 0) {
      for (const error of parsed.errors) problems.push(configProblem(config, error));
      continue;
    }
    const purpose = projectPurpose(config, read.config, parsed);
    projects.push({
      id: path.relative(root, config).split(path.sep).join("/"),
      config,
      options: parsed.options,
      fileNames: parsed.fileNames.map((file) => {
        const resolved = path.resolve(file);
        try {
          return fs.realpathSync(resolved);
        } catch {
          return resolved;
        }
      }),
      projectReferences: parsed.projectReferences,
      ...purpose,
    });
  }
  projects.sort((left, right) => left.id.localeCompare(right.id));
  problems.sort((left, right) => left.project_id.localeCompare(right.project_id));
  const ownersByFile = new Map();
  for (const project of projects) {
    for (const file of project.fileNames) {
      const owners = ownersByFile.get(file) ?? [];
      owners.push(project);
      ownersByFile.set(file, owners);
    }
  }
  discoveryCache = { projects, problems, ownersByFile };
  return discoveryCache;
}

function resolveExtendedConfig(specifier, fromConfig) {
  const fromDirectory = path.dirname(fromConfig);
  const candidates = [];
  if (specifier.startsWith(".") || path.isAbsolute(specifier)) {
    const base = path.resolve(fromDirectory, specifier);
    candidates.push(base, `${base}.json`, path.join(base, "tsconfig.json"));
  } else {
    try {
      candidates.push(createRequire(fromConfig).resolve(specifier));
    } catch {
      try {
        candidates.push(createRequire(fromConfig).resolve(`${specifier}/tsconfig.json`));
      } catch {
        return undefined;
      }
    }
  }
  return candidates.find((candidate) => fs.existsSync(candidate) && fs.statSync(candidate).isFile());
}

function configInputs(config, seen = new Set()) {
  let canonical;
  try {
    canonical = fs.realpathSync(config);
  } catch {
    return [];
  }
  if (seen.has(canonical)) return [];
  seen.add(canonical);
  const read = ts.readConfigFile(canonical, ts.sys.readFile);
  const own = [{
    identity: relativeIdentity(canonical),
    path: canonical,
    source_hash: sourceHash(fs.readFileSync(canonical)),
  }];
  if (read.error) return own;
  const inherited = Array.isArray(read.config.extends)
    ? read.config.extends
    : read.config.extends ? [read.config.extends] : [];
  for (const specifier of inherited) {
    if (typeof specifier !== "string") continue;
    const resolved = resolveExtendedConfig(specifier, canonical);
    if (resolved) own.push(...configInputs(resolved, seen));
  }
  return own.sort((left, right) => left.identity.localeCompare(right.identity));
}

function configuredOwnership(queryFile, discovered) {
  const discoveredOwners = discovered.ownersByFile.get(queryFile) ?? [];
  if (discoveredOwners.length > 0) {
    const primary = discoveredOwners.filter((project) => project.purpose !== "tooling");
    if (primary.length > 0) {
      return {
        owners: primary,
        excludedOwners: discoveredOwners.filter((project) => project.purpose === "tooling"),
        toolingFallback: false,
        problems: discovered.problems,
      };
    }
    return {
      owners: discoveredOwners,
      excludedOwners: [],
      toolingFallback: discoveredOwners.some((project) => project.purpose === "tooling"),
      problems: discovered.problems,
    };
  }
  return undefined;
}

function owningProjects(queryFile, force = false) {
  const discovered = configuredProjects(force);
  const configured = configuredOwnership(queryFile, discovered);
  if (configured) return configured;
  const grouped = groupedInferredProjects([queryFile]);
  return {
    owners: grouped.projects,
    excludedOwners: [],
    toolingFallback: false,
    problems: discovered.problems,
  };
}

function projectById(projectId, projectFiles) {
  // `inferred:` is a reserved namespace (configured IDs are tsconfig paths).
  // Resolve it first so each disposable inferred executor does not rescan and
  // parse every repository config before constructing its supplied scope.
  if (projectId.startsWith("inferred:")) {
    return inferredProjectFromRequest(projectId, projectFiles);
  }
  const discovered = configuredProjects();
  const configured = discovered.projects.find((project) => project.id === projectId);
  if (configured) return configured;
  throw coded("project_not_found", `project not found: ${projectId}`);
}

function buildProject(project) {
  const membershipIdentity = [...project.fileNames].sort().join("\0");
  if (builtProject?.project.id === project.id) {
    if (builtProject.membershipIdentity !== membershipIdentity) {
      throw coded("project_mismatch", "project membership changed within one checker worker");
    }
    return builtProject;
  }
  if (builtProject) {
    throw coded("project_switch", "one checker worker may resolve only one configured project");
  }
  // The program gets the EFFECTIVE options: normalizing absolute paths here
  // breaks baseUrl/paths resolution and silently degrades every mapped
  // receiver to `any`. Normalization exists for the fingerprint only.
  const program = ts.createProgram({
    rootNames: project.fileNames,
    options: project.options,
    projectReferences: project.projectReferences,
  });
  const checker = program.getTypeChecker();
  const sourceInputs = [];
  for (const source of program.getSourceFiles()) {
    let canonical;
    try {
      canonical = fs.realpathSync(source.fileName);
    } catch {
      continue;
    }
    sourceInputs.push({
      identity: relativeIdentity(canonical),
      path: canonical,
      source_hash: sourceHash(fs.readFileSync(canonical)),
    });
  }
  sourceInputs.sort((left, right) => left.identity.localeCompare(right.identity));
  const configs = projectConfigInputs(project);
  const compilerInputs = runtimeInputs();
  const inputFiles = [...compilerInputs, ...configs, ...sourceInputs]
    .filter((value, index, all) => all.findIndex((other) => other.path === value.path) === index)
    .sort((left, right) => left.path.localeCompare(right.path));
  const fingerprint = digestText(JSON.stringify(stable({
    protocol: PROTOCOL_VERSION,
    typescript: { version: ts.version, source: runtime.source },
    compiler_inputs: compilerInputs.map(({ identity, source_hash }) => [identity, source_hash]),
    project: project.id,
    configs: configs.map(({ identity, source_hash }) => [identity, source_hash]),
    options: normalizeOption(project.options),
    inputs: sourceInputs.map(({ identity, source_hash }) => [identity, source_hash]),
  })));
  builtProject = {
    project,
    program,
    checker,
    fingerprint,
    inputFiles,
    sourceFiles: new Map(),
    membershipIdentity,
  };
  return builtProject;
}

function byteToUtf16(buffer, offset, label) {
  if (!Number.isInteger(offset) || offset < 0 || offset > buffer.length) {
    throw coded("invalid_span", `${label} is outside the query file`);
  }
  const prefix = buffer.subarray(0, offset);
  const text = prefix.toString("utf8");
  if (Buffer.byteLength(text, "utf8") !== offset) {
    throw coded("invalid_span", `${label} is not a UTF-8 boundary`);
  }
  return text.length;
}

function utf16ToByte(text, offset) {
  return Buffer.byteLength(text.slice(0, offset), "utf8");
}

function requestSpans(buffer, query) {
  const call = [
    byteToUtf16(buffer, query.call_start, "call_start"),
    byteToUtf16(buffer, query.call_end, "call_end"),
  ];
  const receiver = [
    byteToUtf16(buffer, query.receiver_start, "receiver_start"),
    byteToUtf16(buffer, query.receiver_end, "receiver_end"),
  ];
  const property = [
    byteToUtf16(buffer, query.property_start, "property_start"),
    byteToUtf16(buffer, query.property_end, "property_end"),
  ];
  if (!(call[0] <= receiver[0] && receiver[1] <= property[0] && property[1] <= call[1])) {
    throw coded("invalid_span", "receiver/property spans are not contained by the call span");
  }
  return { call, receiver, property };
}

function findMemberCall(tsSource, spans) {
  let occurrences = occurrenceCache.get(tsSource);
  if (occurrences) return occurrences.get(spanKey(spans));
  occurrences = new Map();
  function visit(node) {
    if (ts.isCallExpression(node) && ts.isPropertyAccessExpression(node.expression)) {
      const member = node.expression;
      const occurrenceSpans = {
        call: [node.getStart(tsSource), node.end],
        receiver: [member.expression.getStart(tsSource), member.expression.end],
        property: [member.name.getStart(tsSource), member.name.end],
      };
      occurrences.set(spanKey(occurrenceSpans), { call: node, member });
    }
    ts.forEachChild(node, visit);
  }
  visit(tsSource);
  occurrenceCache.set(tsSource, occurrences);
  return occurrences.get(spanKey(spans));
}

function spanKey(spans) {
  return [...spans.call, ...spans.receiver, ...spans.property].join(":");
}

function degradedType(type) {
  return Boolean(type.flags & (ts.TypeFlags.Any | ts.TypeFlags.Unknown))
    || type.intrinsicName === "error";
}

function symbolDeclarations(checker, receiverType, member) {
  const property = member.name.text;
  const types = receiverType.isUnion() ? receiverType.types : [receiverType];
  const symbols = [];
  for (const type of types) {
    const symbol = checker.getPropertyOfType(type, property) ?? checker.getSymbolAtLocation(member.name);
    if (!symbol) continue;
    const resolved = symbol.flags & ts.SymbolFlags.Alias ? checker.getAliasedSymbol(symbol) : symbol;
    if (!symbols.includes(resolved)) symbols.push(resolved);
  }
  return symbols.flatMap((symbol) => {
    const declarations = symbol.getDeclarations() ?? [];
    const implementation = declarations.find((declaration) => (
      ts.isFunctionLike(declaration)
      && checker.isImplementationOfOverload(declaration) === true
    ));
    return implementation ? [implementation] : declarations;
  });
}

// Provenance of a declaration file relative to the indexed corpus. The Rust
// side can only anchor `repo` declarations; the other contexts let it skip
// hopeless symbol lookups and attribute unmapped declarations in reports
// instead of conflating "vendored type" with "index gap".
function declarationContext(relative) {
  if (relative === null) return "outside";
  if (!relative.includes("node_modules/")) return "repo";
  if (relative.includes("node_modules/@types/")) return "types";
  if (/\/typescript\/lib\/lib\.[^/]+\.d\.ts$/u.test(relative)) return "lib";
  return "vendored";
}

function declarationResult(declaration) {
  const source = declaration.getSourceFile();
  let canonical;
  try {
    canonical = fs.realpathSync(source.fileName);
  } catch {
    return undefined;
  }
  const named = declaration.name ?? declaration;
  const relative = insideRoot(canonical)
    ? path.relative(root, canonical).split(path.sep).join("/")
    : null;
  return {
    file: relative,
    outside_root: relative === null,
    start: utf16ToByte(source.text, named.getStart(source)),
    end: utf16ToByte(source.text, named.end),
    source_hash: sourceHash(source.text),
    context: declarationContext(relative),
  };
}

function planMembers(files, refreshConfig = true) {
  if (!Array.isArray(files)) throw coded("protocol", "plan_members requires files");
  packageCache.clear();
  const unique = [...new Set(files)].sort();
  const discovered = configuredProjects(refreshConfig);
  const planned = unique.map((file) => {
    const queryFile = resolveQueryFile(file);
    return { file, queryFile, decision: configuredOwnership(queryFile, discovered) };
  });
  const inferred = groupedInferredProjects(
    planned.filter((entry) => !entry.decision).map((entry) => entry.queryFile),
  );
  const ownership = planned.map(({ file, queryFile, decision }) => {
    decision ??= {
      owners: [inferred.projectByFile.get(queryFile)],
      excludedOwners: [],
      toolingFallback: false,
    };
    return {
      file,
      project_ids: decision.owners.map((project) => project.id).sort(),
      excluded_project_ids: decision.excludedOwners.map((project) => project.id).sort(),
      tooling_fallback: decision.toolingFallback,
    };
  });
  return {
    typescript: { version: ts.version, source: runtime.source },
    files: ownership,
    projects: [...discovered.projects, ...inferred.projects]
      .sort((left, right) => left.id.localeCompare(right.id))
      .map(projectSummary),
    configuration_problems: discovered.problems,
  };
}

function sourceRecordForQuery(query, buffers) {
  const queryFile = resolveQueryFile(query.file);
  let sourceRecord = buffers.get(queryFile);
  if (!sourceRecord) {
    const buffer = fs.readFileSync(queryFile);
    sourceRecord = { buffer, hash: sourceHash(buffer) };
    buffers.set(queryFile, sourceRecord);
  }
  if (typeof query.indexed_hash !== "string") {
    throw coded("protocol", "resolve_members queries require indexed_hash");
  }
  if (query.indexed_hash !== sourceRecord.hash) {
    throw coded("hash_mismatch", `query source changed after indexing: ${query.file}`);
  }
  return { queryFile, sourceRecord };
}

function resolveInProject(built, query, buffers, prepared = sourceRecordForQuery(query, buffers)) {
  const { queryFile, sourceRecord } = prepared;
  const spans = requestSpans(sourceRecord.buffer, query);
  const source = built.program.getSourceFile(queryFile);
  let answer;
  if (!source) {
    answer = { status: "unknown" };
  } else {
    const occurrence = findMemberCall(source, spans);
    if (!occurrence) {
      throw coded("span_mismatch", `exact indexed member-call occurrence was not found: ${query.file}`);
    }
    const receiverType = built.checker.getTypeAtLocation(occurrence.member.expression);
    if (degradedType(receiverType)) {
      answer = { status: "unknown" };
    } else {
      const declarations = symbolDeclarations(built.checker, receiverType, occurrence.member)
        .map(declarationResult)
        .filter(Boolean)
        .filter((value, index, all) => all.findIndex((other) => JSON.stringify(other) === JSON.stringify(value)) === index)
        .sort((left, right) => (left.file ?? "").localeCompare(right.file ?? "") || left.start - right.start);
      answer = {
        status: declarations.length > 0 ? "resolved" : "unknown",
        receiver_type: normalizeTypeText(built.checker.typeToString(
          receiverType,
          occurrence.member.expression,
          ts.TypeFormatFlags.NoTruncation,
        )),
        declarations,
      };
    }
  }
  return {
    indexed_hash: query.indexed_hash,
    source_hash: sourceRecord.hash,
    answer: {
      project_id: built.project.id,
      ...answer,
      checker_input_fingerprint: built.fingerprint,
    },
  };
}

function failedFileResult(built, query, sourceRecord, failure) {
  const message = safeFailureMessage(failure, 512);
  return {
    indexed_hash: query.indexed_hash,
    source_hash: sourceRecord.hash,
    answer: {
      project_id: built.project.id,
      status: "failed",
      declarations: [],
      checker_input_fingerprint: built.fingerprint,
      error: {
        code: failure?.code ?? "checker_failure",
        message,
      },
    },
  };
}

function resources() {
  const usage = process.memoryUsage();
  return {
    rss_bytes: usage.rss,
    heap_used_bytes: usage.heapUsed,
    heap_total_bytes: usage.heapTotal,
  };
}

function resolveMembers(projectId, projectFiles, queries) {
  if (typeof projectId !== "string" || projectId.length === 0) {
    throw coded("protocol", "resolve_members requires project_id");
  }
  if (!Array.isArray(queries) || queries.length === 0 || queries.length > 512) {
    throw coded("protocol", "resolve_members requires between 1 and 512 queries");
  }
  const project = projectById(projectId, projectFiles);
  if (!project) throw coded("project_not_found", `project not found: ${projectId}`);
  const projectFileSet = new Set(project.fileNames);
  for (const query of queries) {
    const queryFile = resolveQueryFile(query.file);
    // The Rust planner may deliberately promote an owner that this sidecar's
    // coarse purpose heuristic put in excluded_project_ids. At execution time
    // validate actual parsed TypeScript membership, not the heuristic owner
    // preference a second time.
    if (!projectFileSet.has(queryFile)) {
      throw coded("project_mismatch", `${query.file} is not owned by ${projectId}`);
    }
  }
  const built = buildProject(project);
  let results;
  if (projectId.startsWith("inferred:")) {
    results = Array(queries.length);
    const queriesByFile = new Map();
    for (const [index, query] of queries.entries()) {
      const queryFile = resolveQueryFile(query.file);
      const entries = queriesByFile.get(queryFile) ?? [];
      entries.push({ index, query });
      queriesByFile.set(queryFile, entries);
    }
    for (const entries of queriesByFile.values()) {
      // Missing or changed bytes are a project-input race and remain
      // project-atomic. Once source identity is proven, a span/checker fault is
      // attributable to this file and must not discard sibling roots.
      const prepared = entries.map(({ query }) => sourceRecordForQuery(query, built.sourceFiles));
      try {
        for (const [offset, { index, query }] of entries.entries()) {
          results[index] = resolveInProject(built, query, built.sourceFiles, prepared[offset]);
        }
      } catch (failure) {
        // TypeScript checker exceptions are ordinarily plain `Error` objects.
        // Coded filesystem, protocol, and project races remain scope-atomic;
        // an uncoded fault after every source hash was validated is local to
        // this file and is reported as `checker_failure`.
        if (failure?.code !== undefined
          && !["invalid_span", "span_mismatch", "checker_failure"].includes(failure.code)) {
          throw failure;
        }
        for (const [offset, { index, query }] of entries.entries()) {
          results[index] = failedFileResult(built, query, prepared[offset].sourceRecord, failure);
        }
      }
    }
  } else {
    results = queries.map((query) => resolveInProject(built, query, built.sourceFiles));
  }
  const response = {
    project_id: projectId,
    typescript: { version: ts.version, source: runtime.source },
    checker_input_fingerprint: built.fingerprint,
    results,
    resources: resources(),
  };
  if (Buffer.byteLength(JSON.stringify(response)) > 1024 * 1024) {
    throw coded("oversized_batch", "resolve_members response exceeds 1 MiB; retry a smaller batch");
  }
  return response;
}

function validateProject(projectId, fingerprint) {
  if (!builtProject || builtProject.project.id !== projectId) {
    throw coded("project_not_loaded", "validate_project must follow resolve_members in the same worker");
  }
  let inputsValid = true;
  for (const input of builtProject.inputFiles) {
    try {
      if (input.source_hash === ABSENT_INPUT_HASH) {
        fs.lstatSync(input.path);
        inputsValid = false;
      } else if (sourceHash(fs.readFileSync(input.path)) !== input.source_hash) {
        inputsValid = false;
      }
    } catch (failure) {
      if (input.source_hash !== ABSENT_INPUT_HASH || failure?.code !== "ENOENT") {
        inputsValid = false;
      }
    }
  }
  if (builtProject.project.purpose === "inferred") {
    packageCache.clear();
    for (const file of builtProject.project.fileNames) {
      const current = nearestPackage(file);
      if (
        current.directory !== builtProject.project.packageDirectory
        || current.manifest !== builtProject.project.manifest
        || inferredFamily(file, current.type) !== builtProject.project.family
      ) {
        inputsValid = false;
      }
    }
  }
  return {
    project_id: projectId,
    fingerprint: builtProject.fingerprint,
    valid: fingerprint === builtProject.fingerprint && inputsValid,
    inputs: builtProject.inputFiles.map(({ path: inputPath, source_hash }) => ({
      path: inputPath,
      source_hash,
    })),
  };
}

function resolveMember(query) {
  const queryFile = resolveQueryFile(query.file);
  const buffer = fs.readFileSync(queryFile);
  const actualHash = sourceHash(buffer);
  if (typeof query.indexed_hash !== "string") {
    throw coded("protocol", "resolve_member requires indexed_hash");
  }
  if (query.indexed_hash !== actualHash) throw coded("hash_mismatch", "query source changed after indexing");
  const spans = requestSpans(buffer, query);
  const { owners, problems } = owningProjects(queryFile);
  const answers = [];
  for (const project of owners) {
    builtProject = undefined;
    const built = buildProject(project);
    const source = built.program.getSourceFile(queryFile);
    if (!source) {
      answers.push({ project_id: project.id, status: "unknown", checker_input_fingerprint: built.fingerprint });
      continue;
    }
    const occurrence = findMemberCall(source, spans);
    if (!occurrence) throw coded("span_mismatch", "exact indexed member-call occurrence was not found");
    const receiverType = built.checker.getTypeAtLocation(occurrence.member.expression);
    if (degradedType(receiverType)) {
      answers.push({ project_id: project.id, status: "unknown", checker_input_fingerprint: built.fingerprint });
      continue;
    }
    const declarations = symbolDeclarations(built.checker, receiverType, occurrence.member)
      .map(declarationResult)
      .filter(Boolean)
      .filter((value, index, all) => all.findIndex((other) => JSON.stringify(other) === JSON.stringify(value)) === index)
      .sort((left, right) => (left.file ?? "").localeCompare(right.file ?? "") || left.start - right.start);
    answers.push({
      project_id: project.id,
      status: declarations.length > 0 ? "resolved" : "unknown",
      receiver_type: normalizeTypeText(built.checker.typeToString(
        receiverType,
        occurrence.member.expression,
        ts.TypeFormatFlags.NoTruncation,
      )),
      declarations,
      checker_input_fingerprint: built.fingerprint,
    });
  }
  builtProject = undefined;
  return {
    indexed_hash: query.indexed_hash,
    source_hash: actualHash,
    typescript: { version: ts.version, source: runtime.source },
    projects: answers,
    configuration_problems: problems,
  };
}

function validateInputs(entries) {
  builtProject = undefined;
  discoveryCache = undefined;
  const results = [];
  for (const entry of entries) {
    const queryFile = resolveQueryFile(entry.file);
    const { owners } = owningProjects(queryFile);
    const project = owners.find((candidate) => candidate.id === entry.project_id);
    const fingerprint = project ? buildProject(project).fingerprint : null;
    results.push({
      project_id: entry.project_id,
      file: entry.file,
      fingerprint,
      valid: fingerprint === entry.fingerprint,
      inputs: project ? buildProject(project).inputFiles.map(({ path: inputPath, source_hash }) => ({
        path: inputPath,
        source_hash,
      })) : [],
    });
    builtProject = undefined;
  }
  return { valid: results.every((result) => result.valid), results };
}

function capabilities() {
  const discovered = configuredProjects();
  return {
    typescript: { version: ts.version, source: runtime.source },
    projects: discovered.projects.map(projectSummary),
    configuration_problems: discovered.problems,
    question: "resolve_statically_named_member_at_indexed_call_occurrence",
  };
}

parentPort.on("message", (message) => {
  try {
    let payload;
    if (message.kind === "capabilities") {
      payload = { kind: "capabilities_result", capabilities: capabilities() };
    } else if (message.kind === "plan_members") {
      payload = {
        kind: "plan_members_result",
        result: planMembers(message.files ?? [], message.refresh_config !== false),
      };
    } else if (message.kind === "resolve_member") {
      payload = { kind: "resolve_member_result", result: resolveMember(message.query ?? {}) };
    } else if (message.kind === "resolve_members") {
      payload = {
        kind: "resolve_members_result",
        result: resolveMembers(
          message.project_id,
          message.project_files ?? [],
          message.queries ?? [],
        ),
      };
    } else if (message.kind === "validate_inputs") {
      payload = { kind: "validate_inputs_result", result: validateInputs(message.entries ?? []) };
    } else if (message.kind === "validate_project") {
      payload = {
        kind: "validate_project_result",
        result: validateProject(message.project_id, message.fingerprint),
      };
    } else {
      throw coded("unsupported", "unsupported checker worker request");
    }
    parentPort.postMessage({ id: message.id, payload });
  } catch (failure) {
    parentPort.postMessage({
      id: message.id,
      payload: {
        kind: "error",
        error: {
          code: failure?.code ?? "checker_failure",
          message: safeFailureMessage(failure),
        },
      },
    });
  }
});
