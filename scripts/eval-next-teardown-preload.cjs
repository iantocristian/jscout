'use strict'

// Next's test harness uses `tree-kill` to stop `pnpm next build/start`.
// Codex's macOS workspace-write sandbox denies `pgrep`, so tree-kill sees only
// the pnpm parent. Its descendants keep the stdio pipes open and Next's
// afterAll hook waits until Jest's 120-second timeout.
//
// Give only those lifecycle commands their own process group, tag their Node
// descendants, and answer tree-kill's `pgrep -P` calls from that registry.
// NODE_OPTIONS loads this file only for replay implementation phases; it does
// not modify the snapshot.

const childProcess = require('node:child_process')
const { EventEmitter } = require('node:events')
const path = require('node:path')
const { PassThrough } = require('node:stream')

const originalSpawn = childProcess.spawn
const registry = process.env.JSCOUT_EVAL_PROCESS_REGISTRY
let nextGroupSequence = 0

function appendRegistry(record) {
  if (!registry) return
  try {
    require('node:fs').appendFileSync(
      registry,
      `${JSON.stringify({ timestamp_ms: Date.now(), ...record })}\n`
    )
  } catch {
    // Teardown still has the detached process-group fallback below. The
    // runner preserves the registry so a failed write is diagnosable.
  }
}

appendRegistry({
  kind: 'process',
  pid: process.pid,
  ppid: process.ppid,
  group: process.env.JSCOUT_EVAL_NEXT_GROUP || null,
  argv: process.argv.slice(0, 4),
})

function isNextLifecycleCommand(command, args) {
  const executable = path.basename(String(command))
  return (
    ['pnpm', 'pnpm.js', 'pnpm.cjs', 'yarn', 'npm'].includes(executable) &&
    Array.isArray(args) &&
    args[0] === 'next' &&
    (args[1] === 'build' || args[1] === 'start')
  )
}

function readProcessRecords() {
  if (!registry) return []
  try {
    return require('node:fs')
      .readFileSync(registry, 'utf8')
      .split(/\r?\n/)
      .filter(Boolean)
      .flatMap((line) => {
        try {
          const record = JSON.parse(line)
          return record.kind === 'process' ? [record] : []
        } catch {
          return []
        }
      })
  } catch {
    return []
  }
}

function registeredChildren(parentPid) {
  const records = readProcessRecords()
  const direct = records
    .filter((record) => record.ppid === parentPid)
    .map((record) => record.pid)
  if (direct.length > 0) return [...new Set(direct)]

  // A package-manager shim can insert a non-Node shell that is absent from the
  // registry. For the lifecycle root only, fall back to the other members of
  // its launch token; recursive child queries still terminate normally.
  const target = records.find((record) => record.pid === parentPid)
  if (!target?.group) return []
  const group = records.filter((record) => record.group === target.group)
  if (group[0]?.pid !== parentPid) return []
  return [...new Set(group.slice(1).map((record) => record.pid))]
}

function fakePgrep(parentPid) {
  const child = new EventEmitter()
  child.stdout = new PassThrough()
  child.stderr = new PassThrough()
  const children = registeredChildren(parentPid)
  appendRegistry({
    kind: 'pgrep',
    parent_pid: parentPid,
    registered_children: children,
  })
  queueMicrotask(() => {
    if (children.length > 0) child.stdout.write(`${children.join('\n')}\n`)
    child.stdout.end()
    child.stderr.end()
    child.emit('close', children.length > 0 ? 0 : 1, null)
  })
  return child
}

childProcess.spawn = function patchedSpawn(command, args, options) {
  if (
    path.basename(String(command)) === 'pgrep' &&
    Array.isArray(args) &&
    args[0] === '-P' &&
    Number.isInteger(Number.parseInt(args[1], 10))
  ) {
    return fakePgrep(Number.parseInt(args[1], 10))
  }
  if (!isNextLifecycleCommand(command, args)) {
    return originalSpawn.apply(this, arguments)
  }
  const spawnOptions = options == null ? {} : { ...options }
  spawnOptions.detached = true
  spawnOptions.env = {
    ...process.env,
    ...(spawnOptions.env || {}),
    JSCOUT_EVAL_NEXT_GROUP: [
      process.pid,
      Date.now(),
      nextGroupSequence++,
    ].join('-'),
  }
  return originalSpawn.call(this, command, args, spawnOptions)
}

module.exports = { isNextLifecycleCommand, registeredChildren }
