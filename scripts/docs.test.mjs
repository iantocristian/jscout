import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = fileURLToPath(new URL("../", import.meta.url));
const guides = [
  "README.md",
  "npm/cli/README.md",
  "docs/installation.md",
  "docs/configuration.md",
  "docs/mcp.md",
  "docs/inference.md",
  "docs/commands.md",
  "docs/documentation.md",
  "docs/advanced.md",
];

function markdown(file) {
  return readFileSync(path.resolve(root, file), "utf8");
}

function withoutFences(text) {
  return text.replace(/^```[^\n]*\n[\s\S]*?^```[ \t]*$/gm, "");
}

function headingIds(text) {
  const seen = new Map();
  return new Set([...withoutFences(text).matchAll(/^#{1,6} (.+)$/gm)].map(([, title]) => {
    const base = title.toLowerCase().replace(/[^\p{L}\p{N}_ -]/gu, "").replace(/ /g, "-");
    const count = seen.get(base) ?? 0;
    seen.set(base, count + 1);
    return count ? `${base}-${count}` : base;
  }));
}

for (const file of guides) {
  test(`${file}: local documentation links resolve`, () => {
    for (const [, raw] of withoutFences(markdown(file)).matchAll(/\[[^\]\n]*\]\(([^)\s]+)\)/g)) {
      if (/^[a-z][a-z\d+.-]*:/i.test(raw)) continue;
      const [relative, fragment] = raw.split("#", 2);
      const destination = relative ? path.resolve(root, path.dirname(file), decodeURIComponent(relative)) : path.resolve(root, file);
      assert.ok(existsSync(destination), `${file}: missing link target ${raw}`);
      if (fragment && destination.endsWith(".md")) {
        assert.ok(headingIds(readFileSync(destination, "utf8")).has(decodeURIComponent(fragment)),
          `${file}: missing heading ${raw}`);
      }
    }
  });

  test(`${file}: JSON examples parse`, () => {
    for (const [, body] of markdown(file).matchAll(/^```json\n([\s\S]*?)^```[ \t]*$/gm)) {
      assert.doesNotThrow(() => JSON.parse(body), `${file}: invalid JSON example`);
    }
  });
}
