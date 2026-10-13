// Keep compiler-version differences at the edge of the checker protocol.
// TS7's native API is not the TS5/6 JavaScript compiler API.
import { createRequire } from "node:module";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { inferredOptions } from "./inferred-options.mjs";

const bundledRequire = createRequire(import.meta.url);

export async function loadRuntime(root, onChild = () => {}) {
  const repositoryRequire = createRequire(path.join(root, "package.json"));
  let packageFile;
  let source = "repository";
  try {
    packageFile = repositoryRequire.resolve("typescript/package.json");
  } catch (error) {
    if (error.code !== "MODULE_NOT_FOUND") throw error;
    packageFile = bundledRequire.resolve("typescript/package.json");
    source = "bundled";
  }
  // A broken installed compiler must not silently fall back to another version.
  const require = createRequire(packageFile);
  const resolved = (source === "repository" ? repositoryRequire : bundledRequire).resolve("typescript");
  const ts = bundledRequire(resolved);
  const common = { resolved, source, version: ts.version, files: [resolved, packageFile] };
  if (typeof ts.createProgram === "function" && ts.sys) {
    return {
      ...common,
      syntax: ts,
      readConfig: (file) => ts.readConfigFile(file, ts.sys.readFile),
      diagnosticText: (error) => ts.flattenDiagnosticMessageText(error.messageText, " "),
      parseConfig: (file, raw) => ts.parseJsonConfigFileContent(raw, ts.sys, path.dirname(file), undefined, file),
      inferredOptions: (family) => inferredOptions(ts, family),
      build(project) {
        const program = ts.createProgram({
          rootNames: project.fileNames,
          options: project.options,
          projectReferences: project.projectReferences,
        });
        return {
          program,
          checker: program.getTypeChecker(),
          sourceFileNames: program.getSourceFiles().map((file) => file.fileName),
        };
      },
      unionTypes: (type) => type.isUnion() ? type.types : [type],
      declarations: (symbol) => symbol.getDeclarations() ?? [],
      isImplementation: (checker, declaration) => ts.isFunctionLike(declaration)
        && checker.isImplementationOfOverload(declaration) === true,
      signatureDeclaration: (checker, call) => checker.getResolvedSignature(call)?.getDeclaration(),
      typeToString: (checker, type, node) => checker.typeToString(type, node, ts.TypeFormatFlags.NoTruncation),
    };
  }
  try {
    return await nativeRuntime(root, require, common, onChild);
  } catch (cause) {
    throw Object.assign(new Error(`Cannot load TypeScript ${ts.version} native checker: ${cause.message}`, { cause }), {
      code: "typescript_api_unavailable",
    });
  }
}

async function nativeRuntime(root, require, common, onChild) {
  const importModule = (specifier) => import(pathToFileURL(require.resolve(specifier)).href);
  const [native, ast, { default: getExePath }] = await Promise.all([
    importModule("typescript/unstable/sync"),
    importModule("typescript/unstable/ast"),
    // Use the package's own platform resolver, so the API and binary always
    // come from the same installation (including pnpm and source builds).
    importModule("#getExePath"),
  ]);
  const executable = getExePath();
  // Only JSONC syntax is read by the bundled parser. Project membership,
  // effective options, module resolution and types all come from native TS7.
  const json = bundledRequire("typescript");
  let api;
  let snapshot;
  const virtualConfigs = new Map();
  function close() {
    snapshot?.dispose();
    snapshot = undefined;
    api?.close();
    api = undefined;
    virtualConfigs.clear();
  }
  function session() {
    if (!api) {
      api = new native.API({
        cwd: root,
        tsserverPath: executable,
        // Configured projects use the native filesystem directly; do not add
        // a JS round-trip to every file read just to return "fall through".
        fs: virtualConfigs.size ? { readFile: (file) => virtualConfigs.get(file) } : undefined,
      });
      // The unstable API does not yet expose process ownership. Isolate that
      // dependency here and fail explicitly if it changes. The protocol host
      // must be able to kill this child even while the worker is blocked.
      const child = api.client?.channel?.child;
      if (!child?.pid || typeof child.kill !== "function") {
        close();
        throw new Error("TypeScript native API no longer exposes its compiler process");
      }
      onChild(child);
    }
    return api;
  }
  return {
    ...common,
    files: [...common.files, require.resolve("typescript/unstable/sync"), executable],
    syntax: {
      ...ast,
      TypeFlags: native.TypeFlags,
      SymbolFlags: native.SymbolFlags,
      isMethodSignature: ast.isMethodSignatureDeclaration,
      forEachChild: (node, visit) => node.forEachChild(visit),
    },
    readConfig: (file) => json.readConfigFile(file, json.sys.readFile),
    diagnosticText: (error) => error.text ?? json.flattenDiagnosticMessageText(error.messageText, " "),
    parseConfig(file) {
      return { ...session().parseConfigFile(file), errors: [] };
    },
    inferredOptions: (family) => inferredOptions({
      JsxEmit: { Preserve: "preserve" },
      ScriptTarget: { ESNext: "esnext" },
      ModuleKind: { NodeNext: "nodenext", ESNext: "esnext" },
      ModuleResolutionKind: { NodeNext: "nodenext", Bundler: "bundler" },
    }, family),
    build(project) {
      // Never keep programs for earlier projects alive, or reuse filesystem
      // caches across independently validated generations.
      close();
      const config = project.config ?? path.join(project.packageDirectory, ".jscout-inferred.tsconfig.json");
      if (!project.config) {
        virtualConfigs.set(config, JSON.stringify({ compilerOptions: project.options, files: project.fileNames }));
      }
      snapshot = session().updateSnapshot({ openProjects: [config] });
      const built = snapshot.getProject(config);
      if (!built) throw new Error(`TypeScript did not open project ${project.id}`);
      // 7.0.2's configuration-only parse API omits diagnostics. Retrieve them
      // when opening the selected project, before publishing any answers.
      const errors = built.program.getConfigFileParsingDiagnostics();
      if (errors.length) {
        throw Object.assign(new Error(errors.map((error) => error.text).join("\n")), { code: "config" });
      }
      return {
        program: built.program,
        checker: built.checker,
        sourceFileNames: built.program.getSourceFileNames(),
      };
    },
    unionTypes: (type) => type.isUnionType() ? type.getTypes() : [type],
    declarations: (symbol) => (symbol.declarations ?? []).map((handle) => handle.resolve()),
    isImplementation: (_checker, declaration) => ast.isFunctionLikeDeclaration(declaration)
      && declaration.body !== undefined,
    signatureDeclaration: (checker, call) => checker.getResolvedSignature(call)?.declaration?.resolve(),
    typeToString: (checker, type, node) => checker.typeToString(type, node, native.NodeBuilderFlags.NoTruncation),
    close,
  };
}
