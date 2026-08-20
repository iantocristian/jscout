#!/usr/bin/env node

import { appendFileSync } from 'node:fs';
import { createInterface } from 'node:readline';

const mode = process.env.JSCOUT_MCP_PROBE_MODE ?? 'text';
const logFile = process.env.JSCOUT_MCP_PROBE_LOG;
const recordCount = Number.parseInt(process.env.JSCOUT_MCP_PROBE_RECORDS ?? '40', 10);

if (!['text', 'structured'].includes(mode)) {
  throw new Error('JSCOUT_MCP_PROBE_MODE must be text or structured');
}
if (!Number.isInteger(recordCount) || recordCount < 1 || recordCount > 1_000) {
  throw new Error('JSCOUT_MCP_PROBE_RECORDS must be an integer between 1 and 1000');
}

const marker = 'jscout-structured-content-probe-v1';
const canonical = {
  marker,
  records: Array.from({ length: recordCount }, (_, index) => ({
    id: index + 1,
    anchor: `sym:src/workflow-${String(index + 1).padStart(3, '0')}.ts#handler`,
    at: `src/workflow-${String(index + 1).padStart(3, '0')}.ts:${index + 10}`,
    relation: index % 2 === 0 ? 'calls nextStage' : 'used by previousStage',
  })),
};
const canonicalText = JSON.stringify(canonical);

function log(value) {
  if (logFile) {
    appendFileSync(logFile, `${JSON.stringify(value)}\n`);
  }
}

function write(id, result) {
  const response = { jsonrpc: '2.0', id, result };
  log({ direction: 'response', mode, bytes: Buffer.byteLength(JSON.stringify(response)), response });
  process.stdout.write(`${JSON.stringify(response)}\n`);
}

function toolDefinition() {
  return {
    name: 'transport_probe',
    description:
      'Return a deterministic repository-shaped payload. Call exactly once when asked to run the transport probe.',
    inputSchema: { type: 'object', additionalProperties: false },
    annotations: { readOnlyHint: true },
  };
}

const input = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of input) {
  if (!line.trim()) continue;
  const request = JSON.parse(line);
  log({ direction: 'request', mode, request });
  if (request.id == null && request.method?.startsWith('notifications/')) continue;

  switch (request.method) {
    case 'initialize':
      write(request.id, {
        protocolVersion: request.params?.protocolVersion ?? '2025-06-18',
        capabilities: { tools: {} },
        serverInfo: { name: 'jscout-transport-probe', version: '1.0.0' },
        instructions: 'Use transport_probe only when the user explicitly requests the transport probe.',
      });
      break;
    case 'ping':
      write(request.id, {});
      break;
    case 'tools/list':
      write(request.id, { tools: [toolDefinition()] });
      break;
    case 'tools/call': {
      const result = { content: [{ type: 'text', text: canonicalText }] };
      if (mode === 'structured') result.structuredContent = canonical;
      write(request.id, result);
      break;
    }
    default:
      process.stdout.write(
        `${JSON.stringify({
          jsonrpc: '2.0',
          id: request.id ?? null,
          error: { code: -32601, message: `method not found: ${request.method}` },
        })}\n`,
      );
  }
}
