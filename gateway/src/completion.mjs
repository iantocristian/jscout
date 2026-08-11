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
    throw new CompletionError("canceled", message.errorMessage || "request aborted");
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
  if (/(^|\D)(401|403)(\D|$)/.test(text) || lower.includes("unauthorized") || lower.includes("api key") || lower.includes("credential")) {
    return new CompletionError("auth", text);
  }
  if (/(^|\D)429(\D|$)/.test(text) || lower.includes("rate limit") || lower.includes("overloaded") || lower.includes("capacity") || lower.includes("quota")) {
    return new CompletionError("capacity", text, { retryable: true, capacity: true });
  }
  if (lower.includes("context") && (lower.includes("length") || lower.includes("window") || lower.includes("too long"))) {
    return new CompletionError("context_limit", text);
  }
  if (/(^|\D)(500|502|503|504)(\D|$)/.test(text) || lower.includes("timeout") || lower.includes("timed out") || lower.includes("econn") || lower.includes("socket") || lower.includes("network") || lower.includes("fetch failed")) {
    return new CompletionError("connection", text, { retryable: true });
  }
  return new CompletionError("provider", text);
}

/// Run one completion. Returns {started, result}; the caller emits `started`
/// before awaiting the result promise so Rust can observe the resolved
/// provider/billing path before any network wait.
export async function startCompletion({ registry, parsed, request, signal }) {
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

  const auth = await models.getAuth(model).catch((error) => {
    throw new CompletionError("auth", `auth resolution failed: ${error.message}`);
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

  const result = models
    .complete(model, toContext(request, model), options)
    .then((message) => extractSubmission(message, request.tool.name))
    .catch((error) => {
      if (error instanceof CompletionError || error instanceof RegistryError) throw error;
      if (signal.aborted || error?.name === "AbortError") {
        throw new CompletionError("canceled", "request aborted");
      }
      throw classifyProviderFailure(error?.message);
    });
  return { started, result };
}
