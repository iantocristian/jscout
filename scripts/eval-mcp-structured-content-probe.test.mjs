import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const script = path.join(path.dirname(fileURLToPath(import.meta.url)), 'eval-mcp-structured-content-probe.mjs');
const input = [
  { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-11-25' } },
  { jsonrpc: '2.0', method: 'notifications/initialized' },
  { jsonrpc: '2.0', id: 2, method: 'tools/call', params: { name: 'transport_probe', arguments: {} } },
]
  .map(JSON.stringify)
  .join('\n');

function run(mode) {
  const result = spawnSync(process.execPath, [script], {
    encoding: 'utf8',
    input,
    env: {
      ...process.env,
      JSCOUT_MCP_PROBE_MODE: mode,
      JSCOUT_MCP_PROBE_RECORDS: '3',
    },
  });
  assert.equal(result.status, 0, result.stderr);
  return result.stdout.trim().split('\n').map(JSON.parse);
}

test('text probe returns only the JSON-text fallback', () => {
  const responses = run('text');
  const result = responses.at(-1).result;
  assert.equal(result.structuredContent, undefined);
  const canonical = JSON.parse(result.content[0].text);
  assert.equal(canonical.marker, 'jscout-structured-content-probe-v1');
  assert.equal(canonical.records.length, 3);
});

test('structured probe returns a fact-equal native value and fallback', () => {
  const responses = run('structured');
  const result = responses.at(-1).result;
  assert.deepEqual(result.structuredContent, JSON.parse(result.content[0].text));
  assert.equal(result.structuredContent.records.length, 3);
});
