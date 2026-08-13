import crypto from "node:crypto";
import fs from "node:fs";
import { createRequire } from "node:module";
import path from "node:path";
import { parentPort, workerData } from "node:worker_threads";
import { blake3 } from "@noble/hashes/blake3.js";
import { bytesToHex } from "@noble/hashes/utils.js";

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
const programCache = new Map();
const occurrenceCache = new WeakMap();
let discoveryCache;

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
    });
  }
  projects.sort((left, right) => left.id.localeCompare(right.id));
  problems.sort((left, right) => left.project_id.localeCompare(right.project_id));
  discoveryCache = { projects, problems };
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

function owningProjects(queryFile, force = false) {
  const discovered = configuredProjects(force);
  const owners = discovered.projects.filter((project) => project.fileNames.includes(queryFile));
  if (owners.length > 0) return { owners, problems: discovered.problems };
  const relative = path.relative(root, queryFile).split(path.sep).join("/");
  return {
    owners: [{
      id: `inferred:${relative}`,
      config: undefined,
      options: {
        allowJs: true,
        checkJs: false,
        jsx: ts.JsxEmit.Preserve,
        module: ts.ModuleKind.ESNext,
        moduleResolution: ts.ModuleResolutionKind.Bundler,
        target: ts.ScriptTarget.ESNext,
      },
      fileNames: [queryFile],
      projectReferences: undefined,
    }],
    problems: discovered.problems,
  };
}

function buildProject(project, force = false) {
  if (!force && programCache.has(project.id)) return programCache.get(project.id);
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
  const configs = project.config ? configInputs(project.config) : [];
  const compilerInputs = runtimeInputs();
  const inputFiles = [...compilerInputs, ...configs, ...sourceInputs]
    .filter((value, index, all) => all.findIndex((other) => other.path === value.path) === index)
    .sort((left, right) => left.path.localeCompare(right.path));
  const fingerprint = digestText(JSON.stringify(stable({
    protocol: 1,
    typescript: { version: ts.version, source: runtime.source },
    compiler_inputs: compilerInputs.map(({ identity, source_hash }) => [identity, source_hash]),
    project: project.id,
    configs: configs.map(({ identity, source_hash }) => [identity, source_hash]),
    options: normalizeOption(project.options),
    inputs: sourceInputs.map(({ identity, source_hash }) => [identity, source_hash]),
  })));
  const built = { program, checker, fingerprint, inputFiles };
  programCache.set(project.id, built);
  return built;
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
  return symbols.flatMap((symbol) => symbol.getDeclarations() ?? []);
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
  return {
    file: insideRoot(canonical) ? path.relative(root, canonical).split(path.sep).join("/") : null,
    outside_root: !insideRoot(canonical),
    start: utf16ToByte(source.text, named.getStart(source)),
    end: utf16ToByte(source.text, named.end),
    source_hash: sourceHash(source.text),
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
  return {
    indexed_hash: query.indexed_hash,
    source_hash: actualHash,
    typescript: { version: ts.version, source: runtime.source },
    projects: answers,
    configuration_problems: problems,
  };
}

function validateInputs(entries) {
  programCache.clear();
  discoveryCache = undefined;
  const results = [];
  for (const entry of entries) {
    const queryFile = resolveQueryFile(entry.file);
    const { owners } = owningProjects(queryFile);
    const project = owners.find((candidate) => candidate.id === entry.project_id);
    const fingerprint = project ? buildProject(project, true).fingerprint : null;
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
  }
  return { valid: results.every((result) => result.valid), results };
}

function capabilities() {
  const discovered = configuredProjects();
  return {
    typescript: { version: ts.version, source: runtime.source },
    projects: discovered.projects.map((project) => ({
      project_id: project.id,
      file_count: project.fileNames.length,
    })),
    configuration_problems: discovered.problems,
    question: "resolve_statically_named_member_at_indexed_call_occurrence",
  };
}

parentPort.on("message", (message) => {
  try {
    let payload;
    if (message.kind === "capabilities") {
      payload = { kind: "capabilities_result", capabilities: capabilities() };
    } else if (message.kind === "resolve_member") {
      payload = { kind: "resolve_member_result", result: resolveMember(message.query ?? {}) };
    } else if (message.kind === "validate_inputs") {
      payload = { kind: "validate_inputs_result", result: validateInputs(message.entries ?? []) };
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
          message: failure?.message ?? "checker request failed",
        },
      },
    });
  }
});
