import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
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
import { classifyProviderFailure, requiredToolChoice } from "../src/completion.mjs";
import { createGatewayState, handleMessage } from "../src/server.mjs";
import {
  LineOverflowError,
  MAX_LINE_BYTES,
  errorPayload,
  parseMessage,
  readLines,
} from "../src/protocol.mjs";
import { buildRegistry } from "../src/registry.mjs";

const VERSIONS = { gateway: "0.1.0-test", pi_ai: "0.84.1", node: process.versions.node };

function fauxState({ responses = [], retryPolicy, exit } = {}) {
  const faux = fauxProvider({ provider: "faux", models: [{ id: "faux-model", reasoning: false }] });
  faux.setResponses(responses);
  const models = createModels();
  models.setProvider(faux.provider);
  const state = createGatewayState({
    env: {},
    versions: VERSIONS,
    retryPolicy,
    exit: exit ?? (() => {}),
  });
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
  assert.equal(capabilities.model.billing_path, "api");
  assert.equal(capabilities.model.auth_configured, true);
  assert.equal(capabilities.model.auth_type, "api_key");

  await handleMessage(state, { protocol: 1, id: "cap-2", kind: "capabilities", model: "nope" }, send);
  assert.equal(sent.at(-1).error.code, "invalid_request");
});

test("complete returns started then exactly one submit-tool call", async () => {
  let seenToolChoice;
  const { state } = fauxState({
    responses: [
      (_context, options) => {
        seenToolChoice = options.toolChoice;
        return fauxAssistantMessage(
          [
            fauxThinking("hidden reasoning that must not leak"),
            fauxToolCall("submit_workflow_classification", { ok: true }),
          ],
          { stopReason: "toolUse" },
        );
      },
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
  assert.equal(result.attempts, 1);
  assert.equal(seenToolChoice, "required");
  assert.equal(typeof result.usage.total_tokens, "number");
  assert.ok(!JSON.stringify(result).includes("hidden reasoning"));
  assert.equal(state.active, null);
});

test("text-only and multi-tool responses are protocol failures", async () => {
  const { state, faux } = fauxState({
    retryPolicy: { maxRetries: 0, baseDelayMs: 0, maxDelayMs: 0 },
    responses: [fauxAssistantMessage([fauxText("no tool call")], { stopReason: "stop" })],
  });
  const { sent, send } = collector();
  await greet(state, send);
  await handleMessage(state, completeRequest(), send);
  assert.equal(sent.at(-1).kind, "error");
  assert.equal(sent.at(-1).error.code, "tool_contract");
  assert.equal(sent.at(-1).error.retryable, true);

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

test("tool-contract failures retry and recover within the configured policy", async () => {
  const { state, faux } = fauxState({
    retryPolicy: { maxRetries: 2, baseDelayMs: 0, maxDelayMs: 0 },
    responses: [
      fauxAssistantMessage([fauxText("ordinary text instead")], { stopReason: "stop" }),
      fauxAssistantMessage(
        [fauxToolCall("submit_workflow_classification", { ok: true })],
        { stopReason: "toolUse" },
      ),
    ],
  });
  const { sent, send } = collector();
  await greet(state, send);
  await handleMessage(state, completeRequest(), send);

  assert.equal(sent.at(-1).kind, "result");
  assert.equal(sent.at(-1).attempts, 2);
  assert.equal(faux.state.callCount, 2);
});

test("forced-tool mode is normalized across pi-ai APIs", () => {
  assert.equal(requiredToolChoice("openai-codex-responses"), "required");
  assert.equal(requiredToolChoice("anthropic-messages"), "any");
  assert.equal(requiredToolChoice("google-generative-ai"), "any");
  assert.equal(requiredToolChoice("azure-openai-responses"), undefined);
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

test("transient failures retry within one provider/model and report attempts", async () => {
  const seenModels = [];
  const { state, faux } = fauxState({
    retryPolicy: { maxRetries: 2, baseDelayMs: 0, maxDelayMs: 0 },
    responses: [
      (context, options, providerState, model) => {
        seenModels.push(`${model.provider}:${model.id}`);
        return fauxAssistantMessage([], {
          stopReason: "error",
          errorMessage: "429 overloaded; echoed PROMPT_MARKER and sk-supersecret",
        });
      },
      (context, options, providerState, model) => {
        seenModels.push(`${model.provider}:${model.id}`);
        return fauxAssistantMessage(
          [fauxToolCall("submit_workflow_classification", { ok: true })],
          { stopReason: "toolUse" },
        );
      },
    ],
  });
  const { sent, send } = collector();
  await greet(state, send);
  await handleMessage(state, completeRequest(), send);

  assert.equal(sent.at(-1).kind, "result");
  assert.equal(sent.at(-1).attempts, 2);
  assert.equal(faux.state.callCount, 2);
  assert.deepEqual(seenModels, ["faux:faux-model", "faux:faux-model"]);
  assert.ok(!JSON.stringify(sent).includes("PROMPT_MARKER"));
  assert.ok(!JSON.stringify(sent).includes("sk-supersecret"));
});

test("terminal quota and billing failures are never retried", async () => {
  const { state, faux } = fauxState({
    retryPolicy: { maxRetries: 2, baseDelayMs: 0, maxDelayMs: 0 },
    responses: [
      fauxAssistantMessage([], {
        stopReason: "error",
        errorMessage: "insufficient_quota billing token=supersecret PROMPT_MARKER",
      }),
      fauxAssistantMessage(
        [fauxToolCall("submit_workflow_classification", { ok: true })],
        { stopReason: "toolUse" },
      ),
    ],
  });
  const { sent, send } = collector();
  await greet(state, send);
  await handleMessage(state, completeRequest(), send);

  const failure = sent.at(-1);
  assert.equal(failure.kind, "error");
  assert.equal(failure.error.code, "billing");
  assert.equal(failure.error.retryable, false);
  assert.equal(faux.state.callCount, 1);
  assert.equal(failure.error.message, "provider quota or billing limit was reached");
  assert.ok(!JSON.stringify(failure).includes("PROMPT_MARKER"));
  assert.ok(!JSON.stringify(failure).includes("supersecret"));
});

test("cancellation interrupts retry backoff", async () => {
  const { state, faux } = fauxState({
    retryPolicy: { maxRetries: 2, baseDelayMs: 10_000, maxDelayMs: 10_000 },
    responses: [
      fauxAssistantMessage([], { stopReason: "error", errorMessage: "503 service unavailable" }),
      fauxAssistantMessage(
        [fauxToolCall("submit_workflow_classification", { ok: true })],
        { stopReason: "toolUse" },
      ),
    ],
  });
  const { sent, send } = collector();
  await greet(state, send);
  const completion = handleMessage(state, completeRequest(), send);
  while (faux.state.callCount === 0) await new Promise((resolve) => setTimeout(resolve, 1));
  await new Promise((resolve) => setTimeout(resolve, 10));
  await handleMessage(
    state,
    { protocol: 1, id: "cancel-backoff", kind: "cancel", target_id: "req-1" },
    send,
  );
  await completion;

  assert.equal(faux.state.callCount, 1);
  assert.ok(sent.some((message) => message.kind === "canceled" && message.id === "req-1"));
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

test("error payloads redact credential forms and never expose raw provider failures", () => {
  const payload = errorPayload(
    "provider",
    "Bearer abc.def.ghi api_key=supersecret https://user:pass@example.test/v1?token=secret#fragment sk-supersecret",
  );
  const serialized = JSON.stringify(payload);
  for (const secret of ["abc.def.ghi", "supersecret", "user", "pass", "token=secret", "fragment"]) {
    assert.ok(!serialized.includes(secret), `leaked ${secret}`);
  }

  const classified = classifyProviderFailure("500 response echoed PROMPT_MARKER and sk-supersecret");
  assert.equal(classified.code, "connection");
  assert.equal(classified.message, "provider connection failed");
  assert.ok(!classified.message.includes("PROMPT_MARKER"));

  for (const transient of [
    "rate limit reached for this API key",
    "ResourceExhausted",
    "EAI_AGAIN",
    "stream ended before a terminal response event",
  ]) {
    assert.equal(classifyProviderFailure(transient).retryable, true, transient);
  }
  for (const terminal of [
    "FreeUsageLimitError",
    "usage_limit_reached",
    "available balance is exhausted",
  ]) {
    const failure = classifyProviderFailure(terminal);
    assert.equal(failure.code, "billing", terminal);
    assert.equal(failure.retryable, false, terminal);
  }
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

test("custom providers cannot shadow built-in provider and billing identities", () => {
  assert.throws(
    () =>
      buildRegistry({
        authFile: "/unused",
        credentialStore: { read: async () => undefined, list: async () => [] },
        customProviders: [
          {
            id: "openai-codex",
            name: "shadow",
            baseUrl: "https://example.test/v1",
            models: [
              {
                id: "gpt-5.6-terra",
                name: "shadow",
                input: ["text"],
                reasoning: false,
                contextWindow: 1_000,
                maxTokens: 100,
              },
            ],
          },
        ],
      }),
    /collides with a built-in provider/,
  );
});

test("built-in OpenAI keeps its transport and API-key auth when its base URL is overridden", async () => {
  const registry = buildRegistry({
    authFile: "/unused",
    credentialStore: { read: async () => undefined, list: async () => [] },
    openAIBaseUrl: "https://gateway.example.test/openai/v1/",
  });
  const model = registry.models.getModel("openai", "gpt-5.6-terra");
  assert.ok(model);
  assert.equal(model.api, "openai-responses");
  assert.equal(model.baseUrl, "https://gateway.example.test/openai/v1");
  const auth = await registry.models.getAuth(model, { env: { OPENAI_API_KEY: "test-key" } });
  assert.equal(auth.auth.apiKey, "test-key");
  assert.equal(auth.source, "OPENAI_API_KEY");
});

test("OpenAI base URL environment setting reaches gateway capabilities", async () => {
  const state = createGatewayState({
    env: { JSCOUT_PI_AI_OPENAI_BASE_URL: "https://gateway.example.test/v1/" },
    versions: VERSIONS,
    credentialStore: { read: async () => undefined, list: async () => [] },
  });
  const { sent, send } = collector();
  await greet(state, send);
  await handleMessage(
    state,
    { protocol: 1, id: "cap-openai", kind: "capabilities", model: "openai:gpt-5.6-terra" },
    send,
  );
  assert.equal(sent.at(-1).model.base_url, "https://gateway.example.test/v1");
});

test("built-in OpenAI base URL override rejects unsafe endpoints", () => {
  assert.throws(
    () =>
      buildRegistry({
        authFile: "/unused",
        credentialStore: { read: async () => undefined, list: async () => [] },
        openAIBaseUrl: "https://secret@example.test/v1",
      }),
    /without userinfo, query parameters, or fragments/,
  );
  for (const unsafe of [
    "https://gateway.example.test/v1?api_key=secret",
    "https://gateway.example.test/v1#token",
  ]) {
    assert.throws(
      () =>
        buildRegistry({
          authFile: "/unused",
          credentialStore: { read: async () => undefined, list: async () => [] },
          openAIBaseUrl: unsafe,
        }),
      /without userinfo, query parameters, or fragments/,
    );
  }
});

test("credential read-modify-write is serialized across gateway processes", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "jscout-credentials-"));
  const file = path.join(directory, "auth.json");
  const writer = path.join(import.meta.dirname, "..", "test-fixtures", "credential-writer.mjs");
  await Promise.all([
    run(process.execPath, [writer, file, "provider-a"]),
    run(process.execPath, [writer, file, "provider-b"]),
  ]);
  const credentials = JSON.parse(await readFile(file, "utf8"));
  assert.deepEqual(Object.keys(credentials).sort(), ["provider-a", "provider-b"]);
});

function run(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { stdio: ["ignore", "pipe", "pipe"] });
    let stderr = "";
    child.stderr.setEncoding("utf8");
    child.stderr.on("data", (chunk) => (stderr += chunk));
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) resolve();
      else reject(new Error(`credential writer exited ${code}: ${stderr}`));
    });
  });
}
