import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import process from "node:process";

// Large prepared repositories include node_modules and build output. Prefer a
// copy-on-write clone so every eval arm gets an isolated tree without copying
// several gigabytes. Fall back to a regular recursive copy on filesystems that
// do not support clones/reflinks.
export function cloneTree(source, destination) {
  const from = path.resolve(source);
  const to = path.resolve(destination);
  if (fs.existsSync(to)) {
    throw new Error(`refusing to clone into existing path: ${to}`);
  }
  fs.mkdirSync(path.dirname(to), { recursive: true });

  try {
    if (process.platform === "darwin") {
      execFileSync("cp", ["-cR", from, to]);
      return "clonefile";
    }
    if (process.platform === "linux") {
      execFileSync("cp", ["-a", "--reflink=auto", from, to]);
      return "reflink-auto";
    }
  } catch {
    if (fs.existsSync(to)) fs.rmSync(to, { recursive: true, force: true });
  }

  fs.cpSync(from, to, { recursive: true });
  return "full-copy";
}
