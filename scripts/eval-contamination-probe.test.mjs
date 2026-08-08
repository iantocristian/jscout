import assert from "node:assert/strict";
import test from "node:test";

import {
  classifyProbe,
  setOverlap,
  toolKinds,
} from "./eval-contamination-probe.mjs";

test("setOverlap reports exact set overlap", () => {
  assert.deepEqual(setOverlap(["a", "b"], ["b", "c"]), {
    matches: ["b"],
    precision: 0.5,
    recall: 0.5,
  });
});

test("toolKinds finds nested Codex tool items", () => {
  assert.deepEqual(
    toolKinds([
      { type: "item.completed", item: { type: "agent_message" } },
      { type: "item.completed", item: { type: "command_execution" } },
      { type: "item.completed", item: { type: "mcp_tool_call" } },
    ]),
    ["command_execution", "mcp_tool_call"],
  );
});

test("probe classification rejects tools and full remembered localization", () => {
  const none = setOverlap(["a", "b"], []);
  const partial = setOverlap(["a", "b"], ["a"]);
  const full = setOverlap(["a", "b"], ["a", "b", "extra"]);
  assert.equal(classifyProbe({ runnerError: null, tools: [], fileOverlap: none, symbolOverlap: none }), "clean");
  assert.equal(classifyProbe({ runnerError: null, tools: [], fileOverlap: partial, symbolOverlap: none }), "review");
  assert.equal(classifyProbe({ runnerError: null, tools: [], fileOverlap: full, symbolOverlap: none }), "contaminated");
  assert.equal(classifyProbe({ runnerError: null, tools: ["command_execution"], fileOverlap: none, symbolOverlap: none }), "invalid");
});
