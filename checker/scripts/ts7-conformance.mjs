import { spawnSync } from "node:child_process";

// Run the same protocol conformance suite against a real repository-local TS7
// installation, not mocks or a TS5 compatibility shim. Works on Windows too.
const result = spawnSync(process.execPath, ["--test", "test/sidecar.test.mjs"], {
  stdio: "inherit",
  env: { ...process.env, JSCOUT_TEST_TYPESCRIPT: "7" },
});
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
