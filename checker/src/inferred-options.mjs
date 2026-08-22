export function inferredOptions(ts, family) {
  const shared = {
    allowJs: true,
    checkJs: false,
    jsx: ts.JsxEmit.Preserve,
    target: ts.ScriptTarget.ESNext,
  };
  switch (family) {
    case "node-esm":
    case "node-cjs":
      // NodeNext chooses ESM or CommonJS per extension and nearest package
      // `type`, while retaining modern package exports/imports resolution.
      return {
        ...shared,
        module: ts.ModuleKind.NodeNext,
        moduleResolution: ts.ModuleResolutionKind.NodeNext,
      };
    case "bundler-jsx":
      return {
        ...shared,
        module: ts.ModuleKind.ESNext,
        moduleResolution: ts.ModuleResolutionKind.Bundler,
      };
    default:
      throw new Error(`unsupported inferred compiler family: ${family}`);
  }
}
