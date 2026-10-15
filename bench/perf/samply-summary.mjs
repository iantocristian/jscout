#!/usr/bin/env node
// Summarize local samply captures without running a symbol server or uploading data.
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gunzipSync } from 'node:zlib';

function readJson(path) {
  const bytes = readFileSync(path);
  return JSON.parse(bytes[0] === 0x1f && bytes[1] === 0x8b ? gunzipSync(bytes) : bytes);
}

function group(name) {
  if (/sqlite3|rusqlite::|libsqlite3_sys::|sqlite_vec/.test(name)) return 'sqlite';
  if (/blake3|sha256|sha2::|ring::digest/.test(name)) return 'hash';
  if (/malloc|calloc|realloc|(?:^|::)free(?:$|\b)|__rust_(?:alloc|dealloc)|alloc::alloc::|szone_|nanov2_/.test(name)) return 'allocator';
  if (/std::fs::|std::sys::.*::fs::|tsr_vfs::|(?:^|_)(?:pread|pwrite|read|write|open|close|stat|fstat|lstat|fstatfs|getattrlist|getdirentries|fsync)(?:64)?(?:$|\$|_)/.test(name)) return 'filesystem';
  if (/(?:^|[< ])tsr_[a-z]+::/.test(name)) return 'tsr';
  if (/(?:^|[< ])jscout::/.test(name)) return 'jscout';
  return 'other';
}

function symbolResolver(profile, symbols) {
  const normalize = (id) => id?.replaceAll('-', '').toLowerCase();
  const libraries = profile.libs.map((lib) => {
    const found = symbols?.data?.find((candidate) => candidate.debug_name === lib.debugName
      && ((lib.codeId && normalize(candidate.code_id) === normalize(lib.codeId))
        || (lib.breakpadId && normalize(candidate.debug_id) === normalize(lib.breakpadId).slice(0, 32))));
    return found && {
      ...found,
      addresses: new Map(found.known_addresses),
    };
  });
  return (thread, index) => {
    const func = thread.frameTable.func[index];
    const resource = thread.funcTable.resource[func];
    const libIndex = thread.resourceTable.lib[resource];
    const library = profile.libs[libIndex]?.name ?? '(no library)';
    const known = libraries[libIndex];
    const entry = known?.symbol_table[known.addresses.get(thread.frameTable.address[index])];
    // samply 0.13.1 interns sidecar entries by native function start, not sampled
    // address. Its stored inline frames can belong to a different instruction
    // in that function; using them would manufacture false callers and leaves.
    const name = entry ? symbols.string_table[entry.symbol]
      : thread.stringArray[thread.funcTable.name[func]] ?? '(unknown)';
    return [{ name, library, group: group(name),
      symbolized: name !== 'UNKNOWN' && name !== '(unknown)' && !/^0x[\da-f]+$/i.test(name) }];
  };
}

const emptyMetric = () => ({ inclusive_samples: 0, self_samples: 0, inclusive_cpu_ms: 0, self_cpu_ms: 0 });

function add(map, key, fields, samples, cpu, self) {
  let row = map.get(key);
  if (!row) {
    row = { ...fields, ...emptyMetric() };
    map.set(key, row);
  }
  row.inclusive_samples += samples;
  row.inclusive_cpu_ms += cpu;
  if (self) {
    row.self_samples += samples;
    row.self_cpu_ms += cpu;
  }
}

export function summarize(profile, symbols, { top = 30, process: processFilter } = {}) {
  if (!Array.isArray(profile.threads) || !Array.isArray(profile.libs)) {
    throw new Error('Expected a processed Firefox/samply profile with threads and libs');
  }
  const cpuUnit = profile.meta?.sampleUnits?.threadCPUDelta;
  const cpuFactor = { 'µs': 0.001, us: 0.001, ns: 0.000001, ms: 1 }[cpuUnit];
  const frame = symbolResolver(profile, symbols);
  const functions = new Map();
  const groups = new Map();
  const stacks = new Map();
  const threads = [];
  for (const thread of profile.threads) {
    if (processFilter && !(thread.processName ?? thread.name).includes(processFilter)) continue;
    const samples = thread.samples;
    if (samples.weightType && samples.weightType !== 'samples') {
      throw new Error(`Unsupported sample weight type: ${samples.weightType}`);
    }
    const frames = Array.from({ length: thread.frameTable.length }, (_, i) => frame(thread, i));
    const cache = new Map();
    const row = { name: thread.name, process: thread.processName, pid: thread.pid, tid: thread.tid,
      sample_rows: samples.length, samples: 0, stack_samples: 0, unsymbolized_leaf_samples: 0,
      cpu_delta_rows: 0, missing_cpu_delta_rows: 0, zero_cpu_delta_rows: 0,
      cpu_ms: 0, unattributed_cpu_ms: 0, excluded_first_cpu_ms: null };
    for (let i = 0; i < samples.length; i += 1) {
      const weight = samples.weight?.[i] ?? 1;
      if (!Number.isFinite(weight) || weight < 0) throw new Error('Invalid sample weight');
      row.samples += weight;
      const delta = samples.threadCPUDelta?.[i];
      const valid = cpuFactor !== undefined && Number.isFinite(delta) && delta >= 0;
      // On macOS the first delta contains CPU time since thread creation, not
      // since capture began. Retain it separately, never charge it to one stack.
      const cpu = valid && i > 0 ? delta * cpuFactor : 0;
      if (i === 0 && valid) row.excluded_first_cpu_ms = delta * cpuFactor;
      if (valid && i > 0) {
        row.cpu_delta_rows += 1;
        if (delta === 0) row.zero_cpu_delta_rows += 1;
      } else if (!valid) row.missing_cpu_delta_rows += 1;
      row.cpu_ms += cpu;
      const stackIndex = samples.stack[i];
      if (stackIndex === null || stackIndex === undefined) {
        row.unattributed_cpu_ms += cpu;
        continue;
      }
      let stack = cache.get(stackIndex);
      if (!stack) {
        const entries = [];
        let cursor = stackIndex;
        while (cursor !== null && cursor !== undefined) {
          if (!Number.isInteger(cursor) || cursor < 0 || cursor >= thread.stackTable.length
            || entries.length > thread.stackTable.length) throw new Error('Invalid stack chain');
          entries.push(frames[thread.stackTable.frame[cursor]]);
          cursor = thread.stackTable.prefix[cursor];
        }
        stack = entries.reverse().flat();
        cache.set(stackIndex, stack);
      }
      if (!stack.length) {
        row.unattributed_cpu_ms += cpu;
        continue;
      }
      row.stack_samples += weight;
      const leaf = stack.at(-1);
      if (!leaf.symbolized) row.unsymbolized_leaf_samples += weight;
      const identity = (entry) => `${entry.library}\0${entry.name}`;
      const unique = new Map(stack.map((entry) => [identity(entry), entry]));
      for (const [key, entry] of unique) {
        add(functions, key, entry, weight, cpu, key === identity(leaf));
      }
      for (const category of new Set(stack.map((entry) => entry.group))) {
        add(groups, category, { group: category }, weight, cpu, category === leaf.group);
      }
      const key = JSON.stringify(stack.map((entry) => [entry.library, entry.name]));
      add(stacks, key, { frames: stack.map((entry) => `${entry.name} [${entry.library}]`) }, weight, cpu, true);
    }
    if (row.cpu_delta_rows === 0) row.cpu_ms = row.unattributed_cpu_ms = null;
    threads.push(row);
  }
  if (!threads.length) throw new Error('No threads matched the requested process');
  const cpuAvailable = threads.some((thread) => thread.cpu_delta_rows > 0);
  const rows = (map, order, limit = top) => [...map.values()].sort((a, b) =>
    b[order] - a[order] || JSON.stringify(a).localeCompare(JSON.stringify(b))).slice(0, limit)
    .map((row) => cpuAvailable ? row : { ...row, inclusive_cpu_ms: null, self_cpu_ms: null });
  return {
    schema: 'jscout.samply-summary.v1',
    sample_units: profile.meta?.sampleUnits ?? null,
    nominal_interval_ms: profile.meta?.interval ?? null,
    cpu_delta_weighting_available: cpuAvailable,
    notes: [
      'Sample counts are stack residency, not CPU time; idle samples can carry large weights.',
      'CPU milliseconds estimate attribution of recorded threadCPUDelta to the sampled stack, not exact function execution time; zero-delta idle samples contribute no CPU.',
      'First CPU delta per thread is excluded: on macOS it includes execution before the first observation. Missing deltas and samples without stacks are reported separately.',
      'Inclusive metrics count each function/group once per stack; caller/callee inclusive shares overlap. Self metrics count only the leaf native symbol.',
      'Sidecar inline frames are deliberately omitted: samply 0.13.1 caches them by native function, not instruction address, so they can describe the wrong inline callsite.',
      'Groups are symbol-name heuristics. Missing symbols and sampling overhead limit attribution; compare unprofiled wall-clock runs separately.',
    ],
    threads,
    totals: {
      samples: threads.reduce((n, t) => n + t.samples, 0),
      stack_samples: threads.reduce((n, t) => n + t.stack_samples, 0),
      cpu_ms: cpuAvailable ? threads.reduce((n, t) => n + (t.cpu_ms ?? 0), 0) : null,
      unattributed_cpu_ms: cpuAvailable ? threads.reduce((n, t) => n + (t.unattributed_cpu_ms ?? 0), 0) : null,
    },
    groups: rows(groups, cpuAvailable ? 'self_cpu_ms' : 'self_samples', groups.size),
    top_functions_self: rows(functions, cpuAvailable ? 'self_cpu_ms' : 'self_samples'),
    top_functions_inclusive: rows(functions, cpuAvailable ? 'inclusive_cpu_ms' : 'inclusive_samples'),
    top_stacks: rows(stacks, cpuAvailable ? 'self_cpu_ms' : 'self_samples'),
  };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    const path = args.shift();
    if (!path || path === '--help') {
      console.log('Usage: node bench/perf/samply-summary.mjs PROFILE[.gz] [--symbols FILE] [--process NAME] [--top N]\nPrints JSON; auto-loads PROFILE-without-.gz.syms.json when present.');
    } else {
      const options = {};
      let symbolsPath = path.replace(/\.gz$/, '') + '.syms.json';
      let explicitSymbols = false;
      while (args.length) {
        const flag = args.shift();
        const value = args.shift();
        if (!value || !['--symbols', '--process', '--top'].includes(flag)) throw new Error(`Invalid option: ${flag}`);
        if (flag === '--symbols') {
          symbolsPath = value;
          explicitSymbols = true;
        }
        if (flag === '--process') options.process = value;
        if (flag === '--top') {
          options.top = Number(value);
          if (!Number.isSafeInteger(options.top) || options.top < 1) throw new Error('--top must be positive');
        }
      }
      const symbols = explicitSymbols || existsSync(symbolsPath) ? readJson(symbolsPath) : undefined;
      console.log(JSON.stringify(summarize(readJson(path), symbols, options), null, 2));
    }
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
