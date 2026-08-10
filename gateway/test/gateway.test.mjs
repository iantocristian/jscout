import assert from "node:assert/strict";
import { test } from "node:test";
import { PassThrough } from "node:stream";
import {
  createModels,
  fauxAssistantMessage,
  fauxProvider,
  fauxText,
  fauxThinking,
  fauxToolCall,
} from "@earendil-works/pi-ai";
import { createGatewayState, handleMessage } from "../src/server.mjs";
import { LineOverflowError, MAX_LINE_BYTES, parseMessage, readLines } from "../src/protocol.mjs";

const VERSIONS = { gateway: "0.1.0-test", pi_ai: "0.84.1", node: process.versions.node };

function fauxState({ responses = [], exit } = {}) {
  const faux = fauxProvider({ provider: "faux", models: [{ id: "faux-model", reasoning: false }] });
  faux.setResponses(responses);
  const models = createModels();
  models.setProvider(faux.provider);
  const state = createGatewayState({ env: {}, versions: VERSIONS, exit: exit ?? (() => {}) });
  state.registry = { models, customProviderIds: new Set() };
  return { state, faux };
}

function collector() {
  const sent = [];
  return { sent, send: (message) => sent.push(message) };
}

async function greet(state, send) {
  await handleMessage(state, { protocol: 1, id: "hello-1", kind: "hello" }, send);
}

function completeRequest(overrides = {}) {
  return {
    protocol: 1,
    id: "req-1",
    kind: "complete",
    model: "faux:faux-model",
    reasoning: "provider-default",
    messages: [{ role: "user", content: "classify" }],
    tool: {
      name: "submit_workflow_classification",
      description: "Submit the classification",
      parameters: { type: "object", properties: { ok: { type: "boolean" } }, required: ["ok"] },
    },
    ...overrides,
  };
}

test("hello returns versions and gates every other message", async () => {
  const { state } = fauxState();
  const { sent, send } = collector();
  await handleMessage(state, { protocol: 1, id: "early", kind: "capabilities" }, send);
  assert.equal(sent[0].kind, "error");
  assert.equal(sent[0].error.code, "protocol");

  await greet(state, send);
  const ready = sent[1];
  assert.equal(ready.kind, "ready");
  assert.equal(ready.versions.protocol, 1);
  assert.equal(ready.versions.pi_ai, "0.84.1");
});

test("capabilities describes a known model and rejects malformed specs", async () => {
  const { state } = fauxState();
  const { sent, send } = collector();
  await greet(state, send);
  await handleMessage(
    state,
    { protocol: 1, id: "cap-1", kind: "capabilities", model: "faux:faux-model" },
    send,
  );
  const capabilities = sent.at(-1);
  assert.equal(capabilities.kind, "capabilities_result");
  assert.equal(capabilities.model.provider, "faux");
  assert.equal(capabilities.model.supports_tools, true);
  assert.equal(capabilities.model.supports_service_tier, false);

  await handleMessage(state, { protocol: 1, id: "cap-2", kind: "capabilities", model: "nope" }, send);
  assert.equal(sent.at(-1).error.code, "invalid_request");
});

test("complete returns started then exactly one submit-tool call", async () => {
  const { state } = fauxState({
    responses: [
      fauxAssistantMessage(
        [
          fauxThinking("hidden reasoning that must not leak"),
          fauxToolCall("submit_workflow_classification", { ok: true }),
        ],
        { stopReason: "toolUse" },
      ),
    ],
  });
  const { sent, send } = collector();
  await greet(state, send);
  await handleMessage(state, completeRequest(), send);

  const started = sent[1];
  assert.equal(started.kind, "started");
  assert.equal(started.provider, "faux");
  assert.equal(started.billing_path, "api");
  assert.ok(started.auth_source.length > 0);

  const result = sent[2];
  assert.equal(result.kind, "result");
  assert.deepEqual(result.tool_call, {
    name: "submit_workflow_classification",
    arguments: { ok: true },
  });
  assert.equal(result.stop_reason, "toolUse");
  assert.equal(typeof result.usage.total_tokens, "number");
  assert.ok(!JSON.stringify(result).includes("hidden reasoning"));
  assert.equal(state.active, null);
});

test("text-only and multi-tool responses are protocol failures", async () => {
  const { state, faux } = fauxState({
    responses: [fauxAssistantMessage([fauxText("no tool call")], { stopReason: "stop" })],
  });
  const { sent, send } = collector();
  await greet(state, send);
  await handleMessage(state, completeRequest(), send);
  assert.equal(sent.at(-1).kind, "error");
  assert.equal(sent.at(-1).error.code, "tool_contract");

  faux.setResponses([
    fauxAssistantMessage(
      [
        fauxToolCall("submit_workflow_classification", { ok: true }),
        fauxToolCall("submit_workflow_classification", { ok: false }),
      ],
      { stopReason: "toolUse" },
    ),
  ]);
  await handleMessage(state, completeRequest({ id: "req-2" }), send);
  assert.equal(sent.at(-1).error.code, "tool_contract");

  faux.setResponses([
    fauxAssistantMessage([fauxToolCall("unexpected_tool", { ok: true })], { stopReason: "toolUse" }),
  ]);
  await handleMessage(state, completeRequest({ id: "req-3" }), send);
  assert.equal(sent.at(-1).error.code, "tool_contract");
});

test("a second complete while one is active reports busy", async () => {
  const { state } = fauxState({
    responses: [
      async () => {
        await new Promise((resolve) => setTimeout(resolve, 100));
        return fauxAssistantMessage([fauxToolCall("submit_workflow_classification", { ok: true })], {
          stopReason: "toolUse",
        });
      },
    ],
  });
  const { sent, send } = collector();
  await greet(state, send);
  const first = handleMessage(state, completeRequest(), send);
  const second = handleMessage(state, completeRequest({ id: "req-2" }), send);
  await Promise.all([first, second]);
  const busy = sent.find((message) => message.kind === "error" && message.error.code === "busy");
  assert.ok(busy, "expected a busy error for the overlapping request");
  assert.equal(busy.id, "req-2");
  assert.ok(sent.some((message) => message.kind === "result" && message.id === "req-1"));
});

test("cancel aborts the active completion and acknowledges", async () => {
  const { state } = fauxState({
    responses: [
      (context, options) =>
        new Promise((_resolve, reject) => {
          options?.signal?.addEventListener("abort", () => {
            reject(new DOMException("aborted", "AbortError"));
          });
        }),
    ],
  });
  const { sent, send } = collector();
  await greet(state, send);
  const completion = handleMessage(state, completeRequest(), send);
  await new Promise((resolve) => setTimeout(resolve, 20));
  await handleMessage(
    state,
    { protocol: 1, id: "cancel-1", kind: "cancel", target_id: "req-1" },
    send,
  );
  await completion;
  const acknowledgement = sent.find((message) => message.kind === "cancel_result");
  assert.deepEqual(
    { target: acknowledgement.target_id, active: acknowledgement.active },
    { target: "req-1", active: true },
  );
  const canceled = sent.find((message) => message.kind === "canceled");
  assert.equal(canceled.id, "req-1");
  assert.equal(state.active, null);

  await handleMessage(
    state,
    { protocol: 1, id: "cancel-2", kind: "cancel", target_id: "req-1" },
    send,
  );
  assert.equal(sent.at(-1).active, false);
});

test("timeouts abort the completion with a retryable error", async () => {
  const { state } = fauxState({
    responses: [
      (context, options) =>
        new Promise((_resolve, reject) => {
          options?.signal?.addEventListener("abort", () => {
            reject(new DOMException("aborted", "AbortError"));
          });
        }),
    ],
  });
  const { sent, send } = collector();
  await greet(state, send);
  await handleMessage(state, completeRequest({ timeout_ms: 30 }), send);
  const failure = sent.at(-1);
  assert.equal(failure.kind, "error");
  assert.equal(failure.error.code, "timeout");
  assert.equal(failure.error.retryable, true);
});

test("unknown models, unknown kinds, and unsupported options are stable errors", async () => {
  const { state } = fauxState();
  const { sent, send } = collector();
  await greet(state, send);

  await handleMessage(state, completeRequest({ model: "faux:missing" }), send);
  assert.equal(sent.at(-1).error.code, "unknown_model");

  await handleMessage(state, completeRequest({ id: "r2", reasoning: "sideways" }), send);
  assert.equal(sent.at(-1).error.code, "invalid_request");

  await handleMessage(state, completeRequest({ id: "r3", reasoning: "high" }), send);
  assert.equal(sent.at(-1).error.code, "unsupported_option");

  await handleMessage(
    state,
    completeRequest({ id: "r4", provider_options: { service_tier: "flex" } }),
    send,
  );
  assert.equal(sent.at(-1).error.code, "unsupported_option");

  await handleMessage(state, { protocol: 1, id: "r5", kind: "mystery" }, send);
  assert.equal(sent.at(-1).error.code, "unknown_kind");
});

test("shutdown aborts work and exits through the injected hook", async () => {
  let exitCode = null;
  const { state } = fauxState({ exit: (code) => (exitCode = code) });
  const { sent, send } = collector();
  await greet(state, send);
  await handleMessage(state, { protocol: 1, id: "bye", kind: "shutdown" }, send);
  assert.equal(sent.at(-1).kind, "shutdown_result");
  assert.equal(exitCode, 0);
});

test("parseMessage rejects malformed lines and wrong protocol versions", () => {
  assert.match(parseMessage("not json").error, /valid JSON/);
  assert.match(parseMessage("[]").error, /JSON object/);
  assert.match(parseMessage('{"protocol":2,"id":"x","kind":"hello"}').error, /unsupported protocol/);
  assert.match(parseMessage('{"protocol":1,"kind":"hello"}').error, /id/);
  assert.equal(parseMessage('{"protocol":1,"id":"x","kind":"hello"}').message.kind, "hello");
});

test("readLines splits frames and reports oversized lines without resync", async () => {
  const stream = new PassThrough();
  const lines = [];
  let overflow = null;
  readLines(stream, {
    onLine: (line) => lines.push(line),
    onOverflow: (error) => (overflow = error),
  });
  stream.write('{"a":1}\n{"b":');
  stream.write('2}\n');
  stream.write(Buffer.alloc(MAX_LINE_BYTES + 2, 0x61));
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual(lines, ['{"a":1}', '{"b":2}']);
  assert.ok(overflow instanceof LineOverflowError);
});
