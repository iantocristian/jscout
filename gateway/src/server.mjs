// Protocol dispatcher: hello -> capabilities/complete/cancel/shutdown.
//
// The first implementation runs one completion at a time; request ids are
// still mandatory on every message so cancellation and later concurrency do
// not change the wire protocol.

import process from "node:process";
import { CompletionError, startCompletion } from "./completion.mjs";
import { errorPayload } from "./protocol.mjs";
import {
  RegistryError,
  buildRegistry,
  describeModel,
  parseModelSpec,
  parseOpenAICompatibleProviders,
} from "./registry.mjs";

export const AUTH_FILE_ENV = "JSCOUT_PI_AI_AUTH_FILE";
export const CUSTOM_PROVIDERS_ENV = "JSCOUT_PI_AI_OPENAI_COMPATIBLE_PROVIDERS";
export const OPENAI_BASE_URL_ENV = "JSCOUT_PI_AI_OPENAI_BASE_URL";
const DEFAULT_AUTH_FILE = "~/.pi-ai/auth.json";

export function createGatewayState({ env, versions, credentialStore, exit = process.exit }) {
  return {
    env,
    versions,
    credentialStore,
    exit,
    registry: null,
    registryError: null,
    greeted: false,
    active: null, // { id, controller, timer }
    abortActive(reason) {
      if (this.active) {
        this.active.reason = reason;
        this.active.controller.abort();
      }
    },
  };
}

/// The registry is built lazily on first use so a `hello`/`ready` handshake
/// (and its version report) succeeds even when custom-provider configuration
/// is invalid; the configuration error then surfaces on the first request
/// that needs providers.
function registry(state) {
  if (state.registry) return state.registry;
  if (state.registryError) throw state.registryError;
  try {
    state.registry = buildRegistry({
      authFile: state.env[AUTH_FILE_ENV] ?? DEFAULT_AUTH_FILE,
      customProviders: parseOpenAICompatibleProviders(state.env[CUSTOM_PROVIDERS_ENV], CUSTOM_PROVIDERS_ENV),
      credentialStore: state.credentialStore,
      openAIBaseUrl: state.env[OPENAI_BASE_URL_ENV],
    });
    return state.registry;
  } catch (error) {
    state.registryError = error;
    throw error;
  }
}

export async function handleMessage(state, message, send) {
  const { id, kind } = message;
  try {
    switch (kind) {
      case "hello":
        state.greeted = true;
        send({ id, kind: "ready", versions: { ...state.versions, protocol: 1 } });
        return;
      case "capabilities":
        requireGreeting(state);
        handleCapabilities(state, message, send);
        return;
      case "complete":
        requireGreeting(state);
        await handleComplete(state, message, send);
        return;
      case "cancel":
        requireGreeting(state);
        handleCancel(state, message, send);
        return;
      case "shutdown":
        state.abortActive("shutdown");
        send({ id, kind: "shutdown_result" });
        state.exit(0);
        return;
      default:
        send({ id, kind: "error", error: errorPayload("unknown_kind", `unknown message kind ${JSON.stringify(kind)}`) });
    }
  } catch (error) {
    send({ id, kind: "error", error: toErrorPayload(error) });
  }
}

function requireGreeting(state) {
  if (!state.greeted) {
    throw new CompletionError("protocol", "hello must be the first message");
  }
}

function handleCapabilities(state, message, send) {
  const { models, customProviderIds } = registry(state);
  const response = {
    id: message.id,
    kind: "capabilities_result",
    providers: {
      builtin: models
        .getModels()
        .reduce((providers, model) => providers.add(model.provider), new Set())
        .size,
      custom: [...customProviderIds].sort(),
    },
  };
  if (typeof message.model === "string" && message.model.length > 0) {
    const parsed = parseModelSpec(message.model);
    const model = models.getModel(parsed.provider, parsed.modelId);
    response.model = model ? describeModel(model) : null;
  }
  send(response);
}

async function handleComplete(state, message, send) {
  if (state.active) {
    send({
      id: message.id,
      kind: "error",
      error: errorPayload("busy", `completion ${state.active.id} is already active`),
    });
    return;
  }
  const parsed = parseModelSpec(message.model);
  const controller = new AbortController();
  const active = { id: message.id, controller, reason: null, timer: null };
  state.active = active;
  const timeoutMs = Number.isInteger(message.timeout_ms) && message.timeout_ms > 0 ? message.timeout_ms : null;
  if (timeoutMs) {
    active.timer = setTimeout(() => {
      active.reason = "timeout";
      controller.abort();
    }, timeoutMs);
    active.timer.unref?.();
  }
  try {
    const { started, result } = await startCompletion({
      registry: registry(state),
      parsed,
      request: message,
      signal: controller.signal,
    });
    send({ id: message.id, kind: "started", ...started });
    const submission = await result;
    send({ id: message.id, kind: "result", ...submission });
  } catch (error) {
    if (active.reason === "timeout") {
      send({
        id: message.id,
        kind: "error",
        error: errorPayload("timeout", `completion exceeded ${timeoutMs} ms`, { retryable: true }),
      });
    } else if (active.reason !== null || error?.code === "canceled") {
      send({ id: message.id, kind: "canceled", reason: active.reason ?? "canceled" });
    } else {
      send({ id: message.id, kind: "error", error: toErrorPayload(error) });
    }
  } finally {
    if (active.timer) clearTimeout(active.timer);
    if (state.active === active) state.active = null;
  }
}

function handleCancel(state, message, send) {
  const target = typeof message.target_id === "string" ? message.target_id : "";
  if (state.active && state.active.id === target) {
    state.active.reason = "canceled";
    state.active.controller.abort();
    send({ id: message.id, kind: "cancel_result", target_id: target, active: true });
  } else {
    send({ id: message.id, kind: "cancel_result", target_id: target, active: false });
  }
}

function toErrorPayload(error) {
  if (error instanceof CompletionError) {
    return errorPayload(error.code, error.message, {
      retryable: error.retryable,
      capacity: error.capacity,
    });
  }
  if (error instanceof RegistryError) {
    return errorPayload(error.code, error.message);
  }
  return errorPayload("internal", error?.message ?? "internal gateway failure");
}
