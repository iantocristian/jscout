import assert from "node:assert/strict";
import test from "node:test";
import { boundedReceiverType, MAX_RECEIVER_TYPE_BYTES, RECEIVER_TYPE_TRUNCATION } from "../src/receiver-text.mjs";

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
