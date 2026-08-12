// One structured completion: resolve model and auth, run pi-ai complete()
// with an abort signal, and enforce the submit-tool contract. The gateway
// never executes tools and never returns hidden reasoning.

import { RegistryError, billingPath, reasoningOptions, serviceTierOptions } from "./registry.mjs";

export class CompletionError extends Error {
  constructor(code, message, { retryable = false, capacity = false } = {}) {
    super(message);
    this.name = "CompletionError";
    this.code = code;
    this.retryable = retryable;
    this.capacity = capacity;
  }
}

const ROLES = new Set(["user", "assistant"]);
export const DEFAULT_RETRY_POLICY = Object.freeze({
  maxRetries: 2,
  baseDelayMs: 500,
  maxDelayMs: 2_000,
});

export function validateCompleteRequest(request) {
  const tool = request.tool;
  if (
    tool === null ||
    typeof tool !== "object" ||
    typeof tool.name !== "string" ||
    tool.name.length === 0 ||
    typeof tool.description !== "string" ||
    tool.parameters === null ||
    typeof tool.parameters !== "object"
  ) {
    throw new CompletionError(
      "invalid_request",
      "complete.tool must provide name, description, and a JSON-schema parameters object",
    );
  }
  if (!Array.isArray(request.messages) || request.messages.length === 0) {
    throw new CompletionError("invalid_request", "complete.messages must be a non-empty array");
  }
  for (const [index, message] of request.messages.entries()) {
    if (
      message === null ||
      typeof message !== "object" ||
      !ROLES.has(message.role) ||
      typeof message.content !== "string"
    ) {
      throw new CompletionError(
        "invalid_request",
        `complete.messages[${index}] must be {role: user|assistant, content: string}`,
      );
    }
  }
}

function toContext(request, model) {
  return {
    systemPrompt: typeof request.system === "string" && request.system.length > 0 ? request.system : undefined,
    messages: request.messages.map((message) =>
      message.role === "user"
        ? { role: "user", content: message.content, timestamp: Date.now() }
        : {
            role: "assistant",
            content: [{ type: "text", text: message.content }],
            api: model.api,
            provider: model.provider,
            model: model.id,
            usage: emptyUsage(),
            stopReason: "stop",
            timestamp: Date.now(),
          },
    ),
    tools: [
      {
        name: request.tool.name,
        description: request.tool.description,
        parameters: request.tool.parameters,
      },
    ],
  };
}

function emptyUsage() {
  return {
    input: 0,
    output: 0,
    cacheRead: 0,
    cacheWrite: 0,
    totalTokens: 0,
    cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, total: 0 },
  };
}

export function normalizeUsage(usage) {
  const cost = usage?.cost ?? {};
  return {
    input_tokens: numberOrZero(usage?.input),
    output_tokens: numberOrZero(usage?.output),
    reasoning_tokens: usage?.reasoning === undefined ? null : numberOrZero(usage.reasoning),
    cache_read_tokens: numberOrZero(usage?.cacheRead),
    cache_write_tokens: numberOrZero(usage?.cacheWrite),
    total_tokens: numberOrZero(usage?.totalTokens),
    cost_total: numberOrZero(cost.total),
  };
}

function numberOrZero(value) {
  return typeof value === "number" && Number.isFinite(value) ? value : 0;
}

/// Enforce the structured-output contract: the final message must contain
/// exactly one call of the declared submit tool with object arguments.
/// Text alongside the call is tolerated and dropped; hidden reasoning is
/// never forwarded.
export function extractSubmission(message, toolName) {
  if (message.stopReason === "aborted") {
    throw new CompletionError("canceled", "request aborted");
  }
  if (message.stopReason === "error") {
    throw classifyProviderFailure(message.errorMessage || "provider reported an error");
  }
  const toolCalls = message.content.filter((block) => block.type === "toolCall");
  if (toolCalls.length === 0) {
    throw new CompletionError(
      "tool_contract",
      `model returned no ${toolName} call (stop reason ${message.stopReason})`,
    );
  }
  if (toolCalls.length > 1) {
    throw new CompletionError("tool_contract", `model returned ${toolCalls.length} tool calls; expected one`);
  }
  const [call] = toolCalls;
  if (call.name !== toolName) {
    throw new CompletionError("tool_contract", `model called unknown tool ${JSON.stringify(call.name)}`);
  }
  let args = call.arguments;
  if (typeof args === "string") {
    try {
      args = JSON.parse(args);
    } catch {
      throw new CompletionError("tool_contract", "tool arguments are not valid JSON");
    }
  }
  if (args === null || typeof args !== "object" || Array.isArray(args)) {
    throw new CompletionError("tool_contract", "tool arguments must be a JSON object");
  }
  return {
    tool_call: { name: call.name, arguments: args },
    stop_reason: message.stopReason,
    usage: normalizeUsage(message.usage),
    response_model: message.responseModel ?? null,
  };
}

/// Classify provider/transport failures into stable retryability categories.
/// Auth, schema, and invalid-request failures must never be retried.
export function classifyProviderFailure(message) {
  const text = String(message ?? "provider request failed");
  const lower = text.toLowerCase();
  if (
    lower.includes("gousagelimiterror") ||
    lower.includes("freeusagelimiterror") ||
    lower.includes("usage_limit_reached") ||
    lower.includes("usage not included") ||
    lower.includes("insufficient_quota") ||
    lower.includes("usage limit") ||
    lower.includes("quota exceeded") ||
    lower.includes("monthly quota") ||
    lower.includes("billing") ||
    lower.includes("payment required") ||
    lower.includes("credit balance") ||
    lower.includes("available balance") ||
    lower.includes("out of budget")
  ) {
    return new CompletionError("billing", "provider quota or billing limit was reached");
  }
  if (
    /(^|\D)429(\D|$)/.test(text) ||
    /rate.?limit/iu.test(text) ||
    lower.includes("too many requests") ||
    lower.includes("overloaded") ||
    lower.includes("capacity") ||
    lower.includes("resourceexhausted") ||
    lower.includes("resource exhausted")
  ) {
    return new CompletionError("capacity", "provider is temporarily capacity-limited", {
      retryable: true,
      capacity: true,
    });
  }
  if (
    /(^|\D)(401|403)(\D|$)/.test(text) ||
    lower.includes("unauthorized") ||
    lower.includes("invalid api key") ||
    lower.includes("incorrect api key") ||
    lower.includes("invalid credential") ||
    lower.includes("authentication failed")
  ) {
    return new CompletionError("auth", "provider authentication failed");
  }
  if (lower.includes("context") && (lower.includes("length") || lower.includes("window") || lower.includes("too long"))) {
    return new CompletionError("context_limit", "request exceeds the provider context limit");
  }
  if (
    /(^|\D)(408|409|500|502|503|504|524)(\D|$)/.test(text) ||
    lower.includes("timeout") ||
    lower.includes("timed out") ||
    lower.includes("econn") ||
    lower.includes("socket") ||
    lower.includes("network") ||
    lower.includes("fetch failed") ||
    lower.includes("service unavailable") ||
    lower.includes("server error") ||
    lower.includes("internal error") ||
    lower.includes("provider returned error") ||
    lower.includes("connection refused") ||
    lower.includes("connection lost") ||
    lower.includes("other side closed") ||
    lower.includes("getaddrinfo") ||
    lower.includes("enotfound") ||
    lower.includes("eai_again") ||
    lower.includes("upstream connect") ||
    lower.includes("reset before headers") ||
    lower.includes("ended without") ||
    lower.includes("stream ended before") ||
    lower.includes("did not get a response") ||
    lower.includes("retry delay") ||
    lower.includes("retry your request") ||
    lower.includes("websocket closed") ||
    lower.includes("websocket error") ||
    lower.includes("terminated")
  ) {
    return new CompletionError("connection", "provider connection failed", { retryable: true });
  }
  return new CompletionError("provider", "provider request failed");
}

function retryPolicy(value) {
  const configured = value ?? DEFAULT_RETRY_POLICY;
  return {
    maxRetries: nonNegativeInteger(configured.maxRetries, "maxRetries"),
    baseDelayMs: nonNegativeInteger(configured.baseDelayMs, "baseDelayMs"),
    maxDelayMs: nonNegativeInteger(configured.maxDelayMs, "maxDelayMs"),
  };
}

function nonNegativeInteger(value, name) {
  if (!Number.isInteger(value) || value < 0) {
    throw new CompletionError("configuration", `retry policy ${name} must be a non-negative integer`);
  }
  return value;
}

function retryDelayMs(policy, retryIndex) {
  return Math.min(policy.baseDelayMs * 2 ** retryIndex, policy.maxDelayMs);
}

function abortableDelay(delayMs, signal) {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(new CompletionError("canceled", "request aborted"));
      return;
    }
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, delayMs);
    const onAbort = () => {
      clearTimeout(timer);
      reject(new CompletionError("canceled", "request aborted"));
    };
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

async function completeWithRetry({ models, model, context, options, toolName, signal, policy }) {
  for (let attempt = 0; ; attempt += 1) {
    try {
      const message = await models.complete(model, context, options);
      return { ...extractSubmission(message, toolName), attempts: attempt + 1 };
    } catch (error) {
      let classified = error;
      if (!(error instanceof CompletionError) && !(error instanceof RegistryError)) {
        classified = signal.aborted || error?.name === "AbortError"
          ? new CompletionError("canceled", "request aborted")
          : classifyProviderFailure(error?.message);
      }
      if (!(classified instanceof CompletionError) || !classified.retryable || attempt >= policy.maxRetries) {
        throw classified;
      }
      await abortableDelay(retryDelayMs(policy, attempt), signal);
    }
  }
}

/// Run one completion. Returns {started, result}; the caller emits `started`
/// before awaiting the result promise so Rust can observe the resolved
/// provider/billing path before any network wait.
export async function startCompletion({ registry, parsed, request, signal, retry: configuredRetry }) {
  const { models, customProviderIds } = registry;
  const model = models.getModel(parsed.provider, parsed.modelId);
  if (!model) {
    throw new CompletionError("unknown_model", `unknown pi-ai model ${parsed.provider}:${parsed.modelId}`);
  }
  validateCompleteRequest(request);
  const options = {
    ...reasoningOptions(parsed.provider, request.reasoning),
    ...serviceTierOptions(model, request.provider_options?.service_tier),
    signal,
    // The gateway owns the one visible retry policy. Disable adapter/SDK
    // retries so attempts cannot multiply invisibly underneath it.
    maxRetries: 0,
  };
  if (Number.isInteger(request.max_tokens) && request.max_tokens > 0) {
    options.maxTokens = request.max_tokens;
  }
  if (typeof request.session_id === "string" && request.session_id.length > 0) {
    options.sessionId = request.session_id;
  }
  if (typeof request.cache_retention === "string" && request.cache_retention.length > 0) {
    options.cacheRetention = request.cache_retention;
  }

  const auth = await models.getAuth(model, { signal }).catch(() => {
    throw new CompletionError("auth", "provider authentication could not be resolved");
  });
  if (!auth) {
    throw new CompletionError("auth", `provider ${parsed.provider} has no configured credentials`);
  }

  const started = {
    provider: model.provider,
    model: model.id,
    api: model.api,
    base_url: model.baseUrl ?? null,
    billing_path: billingPath(parsed.provider, customProviderIds),
    // Category only ("OAuth", "OPENAI_API_KEY", ...), never a credential value.
    auth_source: auth.source ?? "configured",
  };

  const result = completeWithRetry({
    models,
    model,
    context: toContext(request, model),
    options,
    toolName: request.tool.name,
    signal,
    policy: retryPolicy(configuredRetry),
  });
  return { started, result };
}
