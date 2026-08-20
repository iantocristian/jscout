// Provider/model registry for the jscout gateway.
//
// One Models collection carries every built-in pi-ai provider (including the
// ChatGPT-plan Codex provider, whose OAuth credentials live in the auth file)
// plus validated custom OpenAI-compatible providers. Reduced from
// raggazzi-ingestion-eval/lib/pi.mjs, which this gateway owns going forward.

import fs from "node:fs/promises";
import { randomUUID } from "node:crypto";
import os from "node:os";
import path from "node:path";
import { setTimeout as sleep } from "node:timers/promises";
import { createProvider, hasApi } from "@earendil-works/pi-ai";
import { openAICompletionsApi } from "@earendil-works/pi-ai/api/openai-completions.lazy";
import { builtinModels } from "@earendil-works/pi-ai/providers/all";

export const REASONING_EFFORTS = Object.freeze([
  "none",
  "minimal",
  "low",
  "medium",
  "high",
  "xhigh",
  "max",
  "provider-default",
]);

const GOOGLE_PROVIDERS = new Set(["google", "google-vertex"]);
const GOOGLE_THINKING_LEVELS = Object.freeze({
  minimal: "MINIMAL",
  low: "LOW",
  medium: "MEDIUM",
  high: "HIGH",
});
const SERVICE_TIER_APIS = new Set(["openai-responses", "openai-codex-responses"]);
const CREDENTIAL_LOCK_STALE_MS = 60_000;
const CREDENTIAL_LOCK_HEARTBEAT_MS = 5_000;

export class RegistryError extends Error {
  constructor(code, message) {
    super(message);
    this.name = "RegistryError";
    this.code = code;
  }
}

export function parseModelSpec(value) {
  const spec = String(value ?? "").trim();
  const separator = spec.indexOf(":");
  if (separator <= 0 || separator === spec.length - 1) {
    throw new RegistryError(
      "invalid_request",
      `model must use provider:model form; received ${JSON.stringify(spec)}`,
    );
  }
  return { spec, provider: spec.slice(0, separator), modelId: spec.slice(separator + 1) };
}

/// Map a normalized reasoning effort onto the provider's transport, or reject
/// it. Rejection is deliberate: silently dropping a reasoning request would
/// change model behavior without telling the caller.
export function reasoningOptions(provider, value) {
  const effort = String(value ?? "provider-default").trim().toLowerCase();
  if (!REASONING_EFFORTS.includes(effort)) {
    throw new RegistryError(
      "invalid_request",
      `unsupported reasoning effort ${JSON.stringify(value)}; expected ${REASONING_EFFORTS.join(", ")}`,
    );
  }
  if (effort === "provider-default") return {};
  if (provider === "openai" || provider === "openai-codex") {
    return { reasoningEffort: effort };
  }
  if (GOOGLE_PROVIDERS.has(provider)) {
    const level = GOOGLE_THINKING_LEVELS[effort];
    if (!level) {
      throw new RegistryError(
        "unsupported_option",
        `reasoning effort ${effort} is not supported by ${provider}; use minimal, low, medium, high, or provider-default`,
      );
    }
    return { thinking: { enabled: true, level } };
  }
  throw new RegistryError(
    "unsupported_option",
    `reasoning effort is not wired for provider ${provider}; use provider-default`,
  );
}

export function serviceTierOptions(model, serviceTier) {
  if (serviceTier === undefined || serviceTier === null) return {};
  if (!SERVICE_TIER_APIS.has(model.api)) {
    throw new RegistryError(
      "unsupported_option",
      `service tier is not supported by ${model.provider} (${model.api})`,
    );
  }
  return { serviceTier: String(serviceTier) };
}

export function billingPath(provider, customProviderIds) {
  if (provider === "openai-codex") return "plan";
  if (customProviderIds.has(provider)) return "custom";
  return "api";
}

export function resolveAuthPath(value) {
  const authFile = String(value ?? "").trim();
  if (!authFile) throw new RegistryError("invalid_request", "auth file must not be empty");
  if (authFile === "~") return os.homedir();
  if (authFile.startsWith("~/")) return path.join(os.homedir(), authFile.slice(2));
  return path.resolve(authFile);
}

export function parseOpenAICompatibleProviders(value, envName) {
  if (value === undefined || value === null || value === "") return [];
  let document = value;
  if (typeof value === "string") {
    try {
      document = JSON.parse(value);
    } catch (error) {
      throw new RegistryError("invalid_request", `${envName} must contain valid JSON: ${error.message}`);
    }
  }
  if (!Array.isArray(document)) {
    throw new RegistryError("invalid_request", `${envName} must be a JSON array`);
  }
  const providers = document.map((provider, providerIndex) => {
    const prefix = `${envName}[${providerIndex}]`;
    if (!isPlainObject(provider)) throw new RegistryError("invalid_request", `${prefix} must be an object`);
    const id = requiredText(provider.id, `${prefix}.id`);
    const baseUrl = normalizeBaseUrl(provider.baseUrl, `${prefix}.baseUrl`);
    if (!Array.isArray(provider.models) || provider.models.length === 0) {
      throw new RegistryError("invalid_request", `${prefix}.models must be a non-empty array`);
    }
    const apiKeyEnv = provider.apiKeyEnv === undefined
      ? null
      : requiredText(provider.apiKeyEnv, `${prefix}.apiKeyEnv`);
    const models = provider.models.map((model, modelIndex) => {
      const modelPrefix = `${prefix}.models[${modelIndex}]`;
      if (!isPlainObject(model)) throw new RegistryError("invalid_request", `${modelPrefix} must be an object`);
      const modelId = requiredText(model.id, `${modelPrefix}.id`);
      return {
        id: modelId,
        name: requiredText(model.name ?? modelId, `${modelPrefix}.name`),
        input: ["text"],
        reasoning: model.reasoning === true,
        contextWindow: positiveInteger(model.contextWindow ?? 131072, `${modelPrefix}.contextWindow`),
        maxTokens: positiveInteger(model.maxTokens ?? 32768, `${modelPrefix}.maxTokens`),
      };
    });
    rejectDuplicateIds(models, `${prefix}.models`);
    return { id, name: requiredText(provider.name ?? id, `${prefix}.name`), baseUrl, apiKeyEnv, models };
  });
  rejectDuplicateIds(providers, envName);
  return providers;
}

/// Assemble the collection: full built-in catalog over the configured
/// credential store, then custom OpenAI-compatible providers on top.
export function buildRegistry({ authFile, customProviders = [], credentialStore, openAIBaseUrl, env = {} }) {
  const credentials = credentialStore ?? new JsonCredentialStore(resolveAuthPath(authFile));
  const models = builtinModels({ credentials });
  overrideProviderBaseUrl(models, "openai", openAIBaseUrl, "JSCOUT_PI_AI_OPENAI_BASE_URL");
  const builtinProviderIds = new Set(models.getProviders().map((provider) => provider.id));
  const customProviderIds = new Set();
  for (const provider of customProviders) {
    if (builtinProviderIds.has(provider.id)) {
      throw new RegistryError(
        "invalid_request",
        `custom provider id ${provider.id} collides with a built-in provider`,
      );
    }
    customProviderIds.add(provider.id);
    models.setProvider(
      createProvider({
        id: provider.id,
        name: provider.name,
        baseUrl: provider.baseUrl,
        auth: {
          apiKey: {
            name: provider.apiKeyEnv ? `${provider.name} (${provider.apiKeyEnv})` : `${provider.name} (keyless)`,
            resolve: async () => {
              if (!provider.apiKeyEnv) {
                // The OpenAI SDK insists on a non-empty client key even when
                // the local server does not authenticate.
                return { auth: { apiKey: "pi-ai-keyless" }, source: "keyless" };
              }
              const apiKey = String(env[provider.apiKeyEnv] ?? "").trim();
              if (!apiKey) {
                throw new RegistryError("auth", `custom provider ${provider.id} requires ${provider.apiKeyEnv}`);
              }
              return { auth: { apiKey }, source: provider.apiKeyEnv };
            },
          },
        },
        models: provider.models.map((model) => ({
          ...model,
          api: "openai-completions",
          provider: provider.id,
          baseUrl: provider.baseUrl,
          cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 },
          compat: {
            supportsStore: false,
            supportsDeveloperRole: false,
            supportsReasoningEffort: false,
            maxTokensField: "max_tokens",
            supportsStrictMode: false,
            supportsLongCacheRetention: false,
          },
        })),
        api: openAICompletionsApi(),
      }),
    );
  }
  return { models, customProviderIds };
}

export function describeModel(model) {
  return {
    provider: model.provider,
    model: model.id,
    api: model.api,
    base_url: model.baseUrl ?? null,
    context_window: model.contextWindow ?? null,
    max_tokens: model.maxTokens ?? null,
    reasoning: model.reasoning === true,
    supports_service_tier: SERVICE_TIER_APIS.has(model.api),
    supports_tools: true,
  };
}

export { hasApi };

function overrideProviderBaseUrl(models, providerId, value, envName) {
  if (value === undefined || value === null || value === "") return;
  const baseUrl = normalizeBaseUrl(value, envName);
  const provider = models.getProvider(providerId);
  if (!provider) {
    throw new RegistryError("configuration", `built-in provider ${providerId} is unavailable`);
  }
  models.setProvider({
    ...provider,
    baseUrl,
    getModels: () => provider.getModels().map((model) => ({ ...model, baseUrl })),
  });
}

function normalizeBaseUrl(value, name) {
  const text = requiredText(value, name);
  let url;
  try {
    url = new URL(text);
  } catch {
    throw new RegistryError("invalid_request", `${name} must be an absolute HTTP(S) URL`);
  }
  if (
    !["http:", "https:"].includes(url.protocol) ||
    url.username ||
    url.password ||
    url.search ||
    url.hash
  ) {
    throw new RegistryError(
      "invalid_request",
      `${name} must be an absolute HTTP(S) URL without userinfo, query parameters, or fragments`,
    );
  }
  return url.toString().replace(/\/$/u, "");
}

function rejectDuplicateIds(values, name) {
  const ids = new Set();
  for (const value of values) {
    if (ids.has(value.id)) throw new RegistryError("invalid_request", `${name} contains duplicate id ${value.id}`);
    ids.add(value.id);
  }
}

function positiveInteger(value, name) {
  if (!Number.isInteger(value) || value < 1) {
    throw new RegistryError("invalid_request", `${name} must be a positive integer`);
  }
  return value;
}

function requiredText(value, name) {
  const text = String(value ?? "").trim();
  if (!text) throw new RegistryError("invalid_request", `${name} is required`);
  return text;
}

function isPlainObject(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

// One-time adaptation of the BVB/raggazzi credential store; owned here.
// The lock covers the complete read-modify-write operation. Atomic rename
// protects readers from torn JSON; it does not by itself prevent two gateway
// processes from overwriting each other's provider entries.
export class JsonCredentialStore {
  constructor(filePath) {
    this.filePath = filePath;
    this.lockPath = `${filePath}.lock`;
  }

  async read(providerId) {
    return (await this.readAll())[providerId];
  }

  async list() {
    const credentials = await this.readAll();
    return Object.entries(credentials).map(([providerId, credential]) => ({
      providerId,
      type: credential.type,
    }));
  }

  async modify(providerId, fn, options = {}) {
    return this.withWriteLock(async () => {
      const credentials = await this.readAll();
      const next = await fn(credentials[providerId]);
      if (next !== undefined) {
        credentials[providerId] = next;
        await this.writeAll(credentials);
      }
      return next ?? credentials[providerId];
    }, options.signal);
  }

  async delete(providerId, options = {}) {
    await this.withWriteLock(async () => {
      const credentials = await this.readAll();
      delete credentials[providerId];
      await this.writeAll(credentials);
    }, options.signal);
  }

  async readAll() {
    try {
      return JSON.parse(await fs.readFile(this.filePath, "utf8"));
    } catch (error) {
      if (error?.code === "ENOENT") return {};
      throw error;
    }
  }

  async writeAll(credentials) {
    await fs.mkdir(path.dirname(this.filePath), { recursive: true });
    const temporaryPath = `${this.filePath}.${process.pid}.${randomUUID()}.tmp`;
    await fs.writeFile(temporaryPath, `${JSON.stringify(credentials, null, 2)}\n`, {
      encoding: "utf8",
      mode: 0o600,
    });
    await fs.rename(temporaryPath, this.filePath);
  }

  async withWriteLock(operation, signal) {
    const release = await this.acquireWriteLock(signal);
    try {
      return await operation();
    } finally {
      await release();
    }
  }

  async acquireWriteLock(signal) {
    await fs.mkdir(path.dirname(this.filePath), { recursive: true });
    const token = `${process.pid}:${randomUUID()}`;
    const ownerPath = path.join(this.lockPath, "owner");
    for (;;) {
      if (signal?.aborted) throw signal.reason ?? new DOMException("aborted", "AbortError");
      try {
        await fs.mkdir(this.lockPath, { mode: 0o700 });
        await fs.writeFile(ownerPath, token, { encoding: "utf8", mode: 0o600 });
        const heartbeat = setInterval(() => {
          const now = new Date();
          void fs.utimes(this.lockPath, now, now).catch(() => {});
        }, CREDENTIAL_LOCK_HEARTBEAT_MS);
        heartbeat.unref?.();
        return async () => {
          clearInterval(heartbeat);
          const owner = await fs.readFile(ownerPath, "utf8").catch(() => null);
          if (owner === token) await fs.rm(this.lockPath, { recursive: true, force: true });
        };
      } catch (error) {
        if (error?.code !== "EEXIST") {
          await fs.rm(this.lockPath, { recursive: true, force: true }).catch(() => {});
          throw error;
        }
      }

      const stat = await fs.stat(this.lockPath).catch(() => null);
      if (stat && Date.now() - stat.mtimeMs > CREDENTIAL_LOCK_STALE_MS) {
        const stalePath = `${this.lockPath}.stale.${process.pid}.${randomUUID()}`;
        try {
          await fs.rename(this.lockPath, stalePath);
          await fs.rm(stalePath, { recursive: true, force: true });
          continue;
        } catch (error) {
          if (error?.code !== "ENOENT") throw error;
        }
      }
      await sleep(25 + Math.floor(Math.random() * 25), undefined, { signal });
    }
  }
}
