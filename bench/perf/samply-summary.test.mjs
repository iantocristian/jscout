import assert from 'node:assert/strict';
import test from 'node:test';
import { summarize } from './samply-summary.mjs';

function fixture() {
  return {
    meta: { interval: 2, sampleUnits: { threadCPUDelta: 'µs', time: 'ms' } },
    libs: [{ name: 'jscout', debugName: 'jscout', codeId: 'ABC' }],
    threads: [{ name: 'main', processName: 'jscout', pid: '1', tid: '2',
      frameTable: { length: 4, func: [0, 1, 2, 3], address: [100, 110, 120, 130] },
      funcTable: { name: [0, 1, 2, 3], resource: [0, 0, 0, 0] },
      resourceTable: { lib: [0] },
      stringArray: ['jscout::main', 'tsr_checker::resolve', 'sqlite3_step', 'read'],
      stackTable: { length: 5, prefix: [null, 0, 1, 0, 1], frame: [0, 1, 1, 2, 3] },
      samples: { length: 5, weightType: 'samples', stack: [0, 2, 3, 4, null],
        weight: [1, 1, 1, 40, 1], threadCPUDelta: [10000, 1000, 500, 0, 250] },
    }],
  };
}

test('CPU deltas exclude startup and idle weights; recursive inclusive counts are not doubled', () => {
  const summary = summarize(fixture());
  assert.equal(summary.totals.samples, 44);
  assert.equal(summary.totals.cpu_ms, 1.75);
  assert.equal(summary.totals.unattributed_cpu_ms, 0.25);
  assert.equal(summary.threads[0].excluded_first_cpu_ms, 10);
  const tsr = summary.groups.find((row) => row.group === 'tsr');
  assert.equal(tsr.inclusive_cpu_ms, 1);
  assert.equal(tsr.self_cpu_ms, 1);
  const read = summary.top_functions_self.find((row) => row.name === 'read');
  assert.equal(read.self_samples, 40);
  assert.equal(read.self_cpu_ms, 0);
  assert.equal(summary.top_functions_self[0].name, 'tsr_checker::resolve');
  assert.equal(summary.top_functions_inclusive[0].name, 'jscout::main');
});

test('presymbolication uses library identity and ignores function-cached inline callsites', () => {
  const profile = fixture();
  profile.threads[0].stringArray[1] = '0x6e';
  const symbols = { string_table: ['tsr_checker::outer', 'blake3::hash'], data: [{
    debug_name: 'jscout', code_id: 'ABC', known_addresses: [[110, 0]],
    symbol_table: [{ rva: 100, symbol: 0, frames: [{ function: 1 }, { function: 0 }] }],
  }] };
  const summary = summarize(profile, symbols);
  assert.equal(summary.top_functions_self[0].name, 'tsr_checker::outer');
  assert.equal(summary.groups.find((row) => row.group === 'hash'), undefined);
  assert.equal(summary.groups.find((row) => row.group === 'tsr').self_cpu_ms, 1);
  symbols.data[0].code_id = 'different';
  const mismatch = summarize(profile, symbols);
  assert.equal(mismatch.top_functions_self[0].name, '0x6e');
  assert.equal(mismatch.threads[0].unsymbolized_leaf_samples, 1);
});

test('missing or unknown CPU units are not converted into estimated CPU time', () => {
  for (const unit of [undefined, 'unknown']) {
    const profile = fixture();
    profile.meta.sampleUnits.threadCPUDelta = unit;
    const summary = summarize(profile);
    assert.equal(summary.cpu_delta_weighting_available, false);
    assert.equal(summary.totals.cpu_ms, null);
    assert.equal(summary.top_functions_self[0].name, 'read');
    assert.equal(summary.top_functions_self[0].self_cpu_ms, null);
  }
});

test('macOS directory enumeration and filesystem status syscalls are grouped as filesystem', () => {
  for (const name of ['__getdirentries64', 'fstatfs']) {
    const profile = fixture();
    profile.threads[0].stringArray[1] = name;
    const summary = summarize(profile);
    assert.equal(summary.top_functions_self[0].group, 'filesystem');
  }
});

test('process filtering, delta unit conversion, and malformed captures fail explicitly', () => {
  const profile = fixture();
  profile.meta.sampleUnits.threadCPUDelta = 'ns';
  assert.equal(summarize(profile, undefined, { process: 'jscout' }).totals.cpu_ms, 0.00175);
  assert.throws(() => summarize(profile, undefined, { process: 'other' }), /No threads/);
  profile.threads[0].samples.weightType = 'bytes';
  assert.throws(() => summarize(profile), /weight type/);
  profile.threads[0].samples.weightType = 'samples';
  profile.threads[0].stackTable.prefix[1] = 1;
  assert.throws(() => summarize(profile), /stack chain/);
});
