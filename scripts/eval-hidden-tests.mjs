import fs from "node:fs";
import path from "node:path";

// Hidden tests are exact post-change files, not a context-sensitive patch.
// Agents commonly add nearby tests of their own; replacing the test files in
// the grader's throwaway probe keeps those edits from making the oracle fail
// to apply. Production files are never present in this overlay.
export function overlayHiddenTests(goldDir, workspaceDir) {
  const source = path.join(goldDir, "gold-tests");
  if (!fs.existsSync(source)) return false;
  fs.cpSync(source, workspaceDir, { recursive: true, force: true });
  return true;
}
