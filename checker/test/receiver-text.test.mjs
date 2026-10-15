import assert from "node:assert/strict";
import test from "node:test";
import path from "node:path";
import { boundedReceiverType, normalizeReceiverType, MAX_RECEIVER_TYPE_BYTES, RECEIVER_TYPE_TRUNCATION } from "../src/receiver-text.mjs";

test("receiver descriptions preserve short text and truncate at UTF-8 boundaries", () => {
  for (const text of ["", "ShortType", "x".repeat(MAX_RECEIVER_TYPE_BYTES), "😀".repeat(MAX_RECEIVER_TYPE_BYTES / 4)]) {
    assert.equal(boundedReceiverType(text), text);
  }
  for (const text of ["x".repeat(MAX_RECEIVER_TYPE_BYTES + 1), "😀".repeat(2048), `${"x".repeat(MAX_RECEIVER_TYPE_BYTES - 1)}é`]) {
    const display = boundedReceiverType(text);
    assert.ok(Buffer.byteLength(display) <= MAX_RECEIVER_TYPE_BYTES);
    assert.ok(display.endsWith(RECEIVER_TYPE_TRUNCATION));
    assert.ok(text.startsWith(display.slice(0, -RECEIVER_TYPE_TRUNCATION.length)));
    assert.ok(!display.includes("\uFFFD"));
    assert.equal(boundedReceiverType(display), display);
  }
});

test("normalization repairs compiler-cut surrogates before JSON serialization", () => {
  const root = path.resolve("fixture-root");
  for (const text of ["value: \"\ud83d...", "\ude80 tail", "😀\ud83d...", "x".repeat(5000) + "\ud83d..."]) {
    const normalized = normalizeReceiverType(root, text);
    assert.ok(normalized.isWellFormed());
    assert.ok(Buffer.byteLength(normalized) <= MAX_RECEIVER_TYPE_BYTES);
    assert.ok(!/\\u[dD][89a-fA-F][0-9a-fA-F]{2}/u.test(JSON.stringify(normalized)));
  }
  assert.equal(normalizeReceiverType(root, "value: \"\ud83d..."), "value: \"�...");
  assert.equal(normalizeReceiverType(root, "valid 🚀"), "valid 🚀");
});

test("normalization hides compiler-cut absolute import paths", () => {
  const root = path.resolve("jscout-type-normalization", "repo");
  const inside = path.join(root, "src/module").split(path.sep).join("/");
  const outside = path.join(`${root}-other`, "module").split(path.sep).join("/");
  for (const [specifier, expected] of [[inside, "src/module"], [outside, "outside:module"], ["relative/module", "relative/module"]]) {
    for (const suffix of ["...", '"', '").Thing']) {
      assert.equal(normalizeReceiverType(root, `import("${specifier}${suffix}`), `import("${expected}${suffix}`);
    }
  }
  assert.equal(normalizeReceiverType(root, 'import("'), 'import("');
  assert.equal(normalizeReceiverType(root, `import("relative").A | import("${outside}...`), 'import("relative").A | import("outside:module...');
  const fragment = path.dirname(root).split(path.sep).join("/");
  assert.equal(normalizeReceiverType(root, `import("${fragment}...`), 'import("outside:jscout-type-normalization...');
});
