#!/usr/bin/env node

import http from 'node:http';

const DEFAULT_DIMENSIONS = 1_024;
const MAX_DIMENSIONS = 16_384;
const MAX_REQUEST_BYTES = 64 * 1024 * 1024;
const HOST = '127.0.0.1';
const MODEL = 'jscout-bench-embed';
const REVISION = 'dense-unit-vector-v1';

function usage() {
  return 'Usage: node bench/perf/mock-inference.mjs [--port PORT] [--dimensions N]';
}

function integer(value, label, minimum, maximum) {
  if (!/^[0-9]+$/.test(value)) {
    throw new Error(`${label} must be an integer`);
  }
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${label} must be between ${minimum} and ${maximum}`);
  }
  return parsed;
}

function parseArguments(argv) {
  let port = 0;
  let dimensions = DEFAULT_DIMENSIONS;
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--help' || argument === '-h') {
      process.stdout.write(`${usage()}\n`);
      process.exit(0);
    }
    const value = argv[index + 1];
    if (argument === '--port' && value !== undefined) {
      port = integer(value, '--port', 0, 65_535);
      index += 1;
      continue;
    }
    if (argument === '--dimensions' && value !== undefined) {
      dimensions = integer(value, '--dimensions', 1, MAX_DIMENSIONS);
      index += 1;
      continue;
    }
    throw new Error(`unknown or incomplete argument: ${argument}\n${usage()}`);
  }
  return { port, dimensions };
}

let options;
try {
  options = parseArguments(process.argv.slice(2));
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exit(2);
}

const configuration = Object.freeze({
  backend: 'deterministic-fixture',
  dtype: 'float32',
  normalize: true,
  vector_shape: 'dense-prng-unit',
});

const stats = {
  configurationRequests: 0,
  embedRequests: 0,
  texts: 0,
  inputChars: 0,
  requestBytes: 0,
  handlerNs: 0,
};

function resetStats() {
  for (const key of Object.keys(stats)) stats[key] = 0;
}

function hashText(text) {
  let hash = 2_166_136_261;
  for (let index = 0; index < text.length; index += 1) {
    hash = Math.imul(hash ^ text.charCodeAt(index), 16_777_619) >>> 0;
  }
  return hash;
}

function vectorFor(text) {
  let state = hashText(text) || 1;
  const vector = new Array(options.dimensions);
  let squaredNorm = 0;
  for (let index = 0; index < vector.length; index += 1) {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    const value = ((state >>> 0) / 0xffff_ffff) * 2 - 1;
    vector[index] = value;
    squaredNorm += value * value;
  }
  const scale = 1 / Math.sqrt(squaredNorm);
  return vector.map((value) => value * scale);
}

function sendJson(response, status, value, headers = {}) {
  const body = JSON.stringify(value);
  response.writeHead(status, {
    'content-type': 'application/json',
    'content-length': Buffer.byteLength(body),
    ...headers,
  });
  response.end(body);
}

function rejectMethod(response, allowed) {
  sendJson(response, 405, { error: 'method not allowed' }, { allow: allowed });
}

function readJson(request) {
  return new Promise((resolve, reject) => {
    const contentType = request.headers['content-type'] ?? '';
    if (!contentType.toLowerCase().startsWith('application/json')) {
      request.resume();
      reject(Object.assign(new Error('content-type must be application/json'), { status: 415 }));
      return;
    }

    const chunks = [];
    let bytes = 0;
    let tooLarge = false;
    request.on('data', (chunk) => {
      bytes += chunk.length;
      if (bytes > MAX_REQUEST_BYTES) {
        tooLarge = true;
        chunks.length = 0;
      } else if (!tooLarge) {
        chunks.push(chunk);
      }
    });
    request.on('error', reject);
    request.on('end', () => {
      if (tooLarge) {
        reject(Object.assign(new Error('request body is too large'), { status: 413 }));
        return;
      }
      try {
        resolve({ body: JSON.parse(Buffer.concat(chunks).toString('utf8')), bytes });
      } catch {
        reject(Object.assign(new Error('request body is not valid JSON'), { status: 400 }));
      }
    });
  });
}

const server = http.createServer(async (request, response) => {
  const started = process.hrtime.bigint();
  const parsedUrl = new URL(request.url ?? '/', `http://${HOST}`);
  if (parsedUrl.search !== '') {
    sendJson(response, 404, { error: 'not found' });
    return;
  }

  if (parsedUrl.pathname === '/configuration') {
    if (request.method !== 'GET') {
      rejectMethod(response, 'GET');
      return;
    }
    stats.configurationRequests += 1;
    sendJson(response, 200, {
      provider: 'local',
      embedding: {
        model: MODEL,
        dimensions: options.dimensions,
        revision: REVISION,
        configuration,
      },
    });
    stats.handlerNs += Number(process.hrtime.bigint() - started);
    return;
  }

  if (parsedUrl.pathname === '/stats') {
    if (request.method !== 'GET') {
      rejectMethod(response, 'GET');
      return;
    }
    sendJson(response, 200, { ...stats, dimensions: options.dimensions });
    return;
  }

  if (parsedUrl.pathname === '/reset') {
    if (request.method !== 'POST') {
      rejectMethod(response, 'POST');
      return;
    }
    resetStats();
    sendJson(response, 200, { ok: true });
    return;
  }

  if (parsedUrl.pathname !== '/embed') {
    sendJson(response, 404, { error: 'not found' });
    return;
  }
  if (request.method !== 'POST') {
    rejectMethod(response, 'POST');
    return;
  }

  try {
    const { body, bytes } = await readJson(request);
    if (body === null || typeof body !== 'object' || Array.isArray(body)) {
      throw Object.assign(new Error('request body must be an object'), { status: 422 });
    }
    if (!Array.isArray(body.texts) || !body.texts.every((text) => typeof text === 'string')) {
      throw Object.assign(new Error('texts must be an array of strings'), { status: 422 });
    }
    if (body.model !== undefined && body.model !== MODEL) {
      throw Object.assign(new Error(`model must be ${MODEL}`), { status: 422 });
    }

    stats.embedRequests += 1;
    stats.texts += body.texts.length;
    stats.inputChars += body.texts.reduce((sum, text) => sum + text.length, 0);
    stats.requestBytes += bytes;
    sendJson(response, 200, {
      provider: 'local',
      model: MODEL,
      revision: REVISION,
      dimensions: options.dimensions,
      configuration,
      vectors: body.texts.map(vectorFor),
    });
    stats.handlerNs += Number(process.hrtime.bigint() - started);
  } catch (error) {
    sendJson(response, error.status ?? 500, { error: error.message });
  }
});

server.on('error', (error) => {
  process.stderr.write(`mock inference server failed: ${error.message}\n`);
  process.exitCode = 1;
});

server.listen(options.port, HOST, () => {
  const address = server.address();
  const port = typeof address === 'object' && address !== null ? address.port : options.port;
  process.stdout.write(`${JSON.stringify({
    ready: true,
    host: HOST,
    port,
    url: `http://${HOST}:${port}`,
    model: MODEL,
    revision: REVISION,
    dimensions: options.dimensions,
  })}\n`);
});

let closing = false;
function close() {
  if (closing) return;
  closing = true;
  server.close(() => process.exit(0));
  server.closeIdleConnections?.();
  setTimeout(() => process.exit(1), 2_000).unref();
}

process.on('SIGINT', close);
process.on('SIGTERM', close);
