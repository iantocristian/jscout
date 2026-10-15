import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const repository = fileURLToPath(new URL('../', import.meta.url));
const options = { skip: process.platform === 'win32' && 'requires Bash and POSIX utilities' };

function fixture(t) {
  const root = mkdtempSync(join(tmpdir(), 'jscout-pgo-test-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const bin = join(root, 'bin');
  const sysroot = join(root, 'sysroot');
  mkdirSync(bin);
  mkdirSync(join(sysroot, 'lib/rustlib/aarch64-apple-darwin/bin'), { recursive: true });
  const tool = (path, body) => writeFileSync(path, `#!/usr/bin/env bash\nset -eu\n${body}\n`, { mode: 0o755 });
  tool(join(bin, 'rustc'), `case "$*" in
    '-vV') printf 'rustc %s\nhost: aarch64-apple-darwin\nLLVM version: 22.1.6\n' "\${FAKE_RUST_VERSION-test}" ;;
    '--print sysroot') echo '${sysroot}' ;;
    '--print target-cpus') echo 'native - current CPU: apple-m4' ;;
    *) exit 1 ;;
  esac`);
  for (const compiler of ['cc', 'c++']) tool(join(bin, compiler), "echo 'Mock Clang 21'");
  tool(join(bin, 'cargo'), `if [[ "$*" == --version ]]; then echo cargo-test; exit; fi
    if [[ "\${FAKE_FAIL_BUILD-}" == 1 ]]; then echo 'fake cargo failure' >&2; exit 7; fi
    printf '%s\\n' "$*" "$RUSTFLAGS" >> "$FAKE_LOG"
    mkdir -p "$CARGO_TARGET_DIR/aarch64-apple-darwin/release-perf"
    printf '#!/usr/bin/env bash\\necho fake-binary\\n' > "$CARGO_TARGET_DIR/aarch64-apple-darwin/release-perf/jscout"
    chmod +x "$CARGO_TARGET_DIR/aarch64-apple-darwin/release-perf/jscout"`);
  tool(join(sysroot, 'lib/rustlib/aarch64-apple-darwin/bin/llvm-profdata'), `
    if [[ "$1" == show ]]; then echo 'Total functions: 1'; exit; fi
    [[ "$1" == merge && "$2" == --failure-mode=any && "$3" == -o ]]
    output="$4"; shift 4
    for raw in "$@"; do [[ "$(<"$raw")" == GOOD ]] || exit 8; done
    echo profile > "$output"`);
  const env = { PATH: `${bin}:/usr/bin:/bin`, FAKE_LOG: join(root, 'calls.log') };
  const campaign = () => mkdtempSync(join(root, 'campaign-'));
  const run = (mode, directory, extra = {}) => spawnSync('/bin/bash', [
    'scripts/build-pgo.sh', mode, directory,
  ], { cwd: repository, env: { ...env, ...extra }, encoding: 'utf8' });
  return { root, campaign, run, env };
}

function succeeded(result) { assert.equal(result.status, 0, result.stdout + result.stderr); }
function rejected(result, message) {
  assert.notEqual(result.status, 0);
  assert.match(result.stdout + result.stderr, message);
}

test('PGO stages preserve flags/logs and reject missing or reused artifacts', options, (t) => {
  const { campaign, run, env } = fixture(t);
  const root = campaign();
  succeeded(run('baseline', root));
  assert.equal(readFileSync(join(root, 'build-inputs.sha256'), 'utf8'),
    readFileSync(join(root, 'build-baseline-inputs.sha256'), 'utf8'));
  rejected(run('baseline', root), /build output already exists/);
  rejected(run('optimize', root), /run instrument and train/);
  succeeded(run('instrument', root));
  assert.ok(existsSync(join(root, 'profiles/build-inputs.sha256')));
  rejected(run('optimize', root), /no training profiles/);
  writeFileSync(join(root, 'profiles/raw/test.profraw'), '');
  rejected(run('optimize', root), /unexpected or empty/);
  writeFileSync(join(root, 'profiles/raw/test.profraw'), 'GOOD\n');
  succeeded(run('optimize', root));
  const log = readFileSync(join(root, 'build-optimize.log'), 'utf8');
  assert.match(log, /profile-use=.*merged.profdata/);
  assert.match(log, /Total functions: 1/);
  assert.match(log, /Mock Clang 21/);
  rejected(run('optimize', root), /build output already exists/);
  rejected(run('instrument', root), /build output already exists/);
  const calls = readFileSync(env.FAKE_LOG, 'utf8');
  assert.match(calls, /build --locked --profile release-perf --target aarch64-apple-darwin --bin jscout/);
  assert.match(calls, /-C target-cpu=native -C force-frame-pointers=yes -C profile-generate=/);
  assert.match(calls, /-C target-cpu=native -C force-frame-pointers=yes -C profile-use=/);
});

test('PGO preflight rejects ambient overrides, stale profiles and changed identity', options, (t) => {
  const { campaign, run } = fixture(t);
  rejected(run('unknown', campaign()), /usage:/);
  rejected(run('baseline', 'relative/path'), /absolute directory/);
  for (const name of ['RUSTFLAGS', 'CC', 'CFLAGS', 'RUSTC', 'RUSTC_WRAPPER',
    'RUSTC_WORKSPACE_WRAPPER', 'CARGO_BUILD_RUSTC', 'CARGO_BUILD_RUSTC_WRAPPER']) {
    rejected(run('baseline', campaign(), { [name]: 'override' }), new RegExp(`unset ${name}`));
  }
  const changed = campaign();
  succeeded(run('instrument', changed));
  writeFileSync(join(changed, 'profiles/build-inputs.sha256'), 'wrong\n');
  writeFileSync(join(changed, 'profiles/raw/test.profraw'), 'GOOD\n');
  rejected(run('optimize', changed), /build inputs changed/);
  for (const [first, second] of [['baseline', 'instrument'], ['instrument', 'baseline']]) {
    const mismatch = campaign();
    succeeded(run(first, mismatch));
    rejected(run(second, mismatch, { FAKE_RUST_VERSION: 'changed' }), /build inputs changed within this campaign/);
    assert.ok(!existsSync(join(mismatch, `build-${second}`)));
  }
  const stale = campaign();
  mkdirSync(join(stale, 'profiles/raw'), { recursive: true });
  writeFileSync(join(stale, 'profiles/raw/stale.profraw'), 'GOOD\n');
  rejected(run('instrument', stale), /profiles already exist/);
});

test('PGO build and merge failures survive tee and remain in stage logs', options, (t) => {
  const { campaign, run } = fixture(t);
  const failed = campaign();
  assert.equal(run('baseline', failed, { FAKE_FAIL_BUILD: '1' }).status, 7);
  assert.match(readFileSync(join(failed, 'build-baseline.log'), 'utf8'), /fake cargo failure/);
  assert.ok(!existsSync(join(failed, 'profiles/build-inputs.sha256')));
  const invalid = campaign();
  succeeded(run('instrument', invalid));
  writeFileSync(join(invalid, 'profiles/raw/broken.profraw'), 'INVALID\n');
  assert.equal(run('optimize', invalid).status, 8);
  assert.ok(existsSync(join(invalid, 'build-optimize.log')));
  assert.ok(!existsSync(join(invalid, 'profiles/merged.profdata')));
});
