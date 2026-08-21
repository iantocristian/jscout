#!/usr/bin/env node

import { createInterface } from 'node:readline';

import { SCOUT_CARD_ANCHOR } from './ai-pipe-fixture.mjs';

const PROTOCOL = 1;
const TOOL = 'submit_symbol_card';
const PROVIDER = 'benchmark';
const MODEL = 'deterministic-card-v1';

const submission = {
  purpose: {
    text: 'Evaluates whether a broker order intent satisfies deterministic trading-risk policy and returns the resulting approval decision.',
    evidence: [{ start_line: 29, end_line: 63 }],
  },
  incomplete_reason: null,
};

function send(value) {
  process.stdout.write(`${JSON.stringify({ protocol: PROTOCOL, ...value })}\n`);
}

function fail(request, message) {
  send({ id: request?.id ?? 'unknown', kind: 'error', code: 'benchmark_protocol', message });
  process.exitCode = 1;
}

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  let request;
  try {
    request = JSON.parse(line);
  } catch {
    fail(null, 'request is not valid JSON');
    break;
  }
  if (request.protocol !== PROTOCOL || typeof request.id !== 'string') {
    fail(request, 'unexpected protocol envelope');
    break;
  }
  if (request.kind === 'hello') {
    send({
      id: request.id,
      kind: 'ready',
      versions: { gateway: 'benchmark', pi_ai: 'benchmark', node: process.versions.node, protocol: PROTOCOL },
    });
    continue;
  }
  if (request.kind === 'capabilities') {
    send({
      id: request.id,
      kind: 'capabilities_result',
      providers: { builtin: 0, custom: ['benchmark'] },
      model: {
        provider: PROVIDER,
        model: MODEL,
        api: 'responses',
        base_url: 'https://benchmark.invalid',
        context_window: 400_000,
        max_tokens: 32_000,
        reasoning: true,
        supports_service_tier: true,
        supports_tools: true,
        billing_path: 'api',
        auth_configured: true,
        auth_type: 'fixture',
        auth_source: 'benchmark',
      },
    });
    continue;
  }
  if (request.kind === 'complete') {
    if (request.tool?.name !== TOOL || !JSON.stringify(request).includes(SCOUT_CARD_ANCHOR)) {
      fail(request, `expected ${TOOL} request for ${SCOUT_CARD_ANCHOR}`);
      break;
    }
    send({
      id: request.id,
      kind: 'started',
      provider: PROVIDER,
      model: MODEL,
      api: 'responses',
      base_url: 'https://benchmark.invalid',
      billing_path: 'api',
      auth_source: 'benchmark',
    });
    send({
      id: request.id,
      kind: 'result',
      tool_call: { name: TOOL, arguments: submission },
      stop_reason: 'toolUse',
      usage: {
        input_tokens: 1,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        total_tokens: 2,
        cost_total: 0,
      },
      attempts: 1,
      response_model: MODEL,
    });
    continue;
  }
  if (request.kind === 'shutdown') {
    send({ id: request.id, kind: 'shutdown_result' });
    process.exit(0);
  }
  fail(request, `unexpected request kind: ${request.kind}`);
  break;
}
