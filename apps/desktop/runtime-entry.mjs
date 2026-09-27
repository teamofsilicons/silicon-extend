#!/usr/bin/env node
// Silicon Extend's entry for the agent-device runtime it ships. Packaging (macos/build-app.sh,
// linux/build-package.sh) installs this file as bin/agent-device.mjs and keeps agent-device's own
// entry beside it as bin/agent-device-cli.mjs, which runs after one check.
//
// agent-device reuses a running daemon whose version equals its own, and the version stamped on a
// packaged runtime (stamp-runtime.mjs) names its content, not where it is installed. A daemon keeps
// loading code, and the macOS helper, from the location it was started from. Reused from an
// identical copy somewhere else, it fails with "Cannot find module" once that first location is
// moved or deleted: an app dragged from Downloads to Applications, a translocated app, an unpacked
// tarball that was removed. So before agent-device runs, a daemon of this same version that was
// started from another location is stopped (with agent-device's own `daemon stop`), and
// agent-device starts a fresh one from here. extend-runtime-root.json in the daemon's state
// directory records which location the daemon there belongs to.
//
// A daemon of another version is left to agent-device, which replaces an older one and leaves a
// newer one running. Help, --version, `daemon …` and runs against a remote daemon aren't checked.
//
// The record names, for each version, the location its daemon was started from
// ({"root": <latest>, "roots": {"<version>": <location>}}), and it is written before agent-device
// runs: a command killed before it exits (a timeout, SIGKILL) must still leave it, or the next
// command would stop the daemon this location has just started. Keyed by version, a location's
// claim never covers a daemon of another version that agent-device keeps (a newer one).
//
// Flags are read the way agent-device's parser reads them (src/cli/parser/args.ts): only before a
// `--`, after which every token is text (`type -- --state-dir=~/notes` types those words).
import { spawnSync } from 'node:child_process';
import { closeSync, mkdirSync, openSync, readFileSync, realpathSync, renameSync, rmSync, statSync, writeFileSync, writeSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const RECORD = 'extend-runtime-root.json';
// What extend-agent allows `daemon stop` (crates/extend-agent/src/drivers/agent_device.rs).
const STOP_TIMEOUT_MS = 45_000;
// A lock older than this belongs to a command that died while holding it.
const LOCK_STALE_MS = STOP_TIMEOUT_MS + 15_000;
const bin = path.dirname(fileURLToPath(import.meta.url));
const cli = path.join(bin, 'agent-device-cli.mjs');

function readJson(file) {
  try {
    return JSON.parse(readFileSync(file, 'utf8'));
  } catch {
    return undefined;
  }
}

function describe(error) {
  return error instanceof Error ? error.message : String(error);
}

// The tokens agent-device reads as flags: those before the first `--` (its parser, args.ts:64).
function flagTokens(args) {
  const end = args.indexOf('--');
  return end === -1 ? args : args.slice(0, end);
}

// The last `--name value` or `--name=value`, as agent-device's parser reads it.
function flag(args, name) {
  const tokens = flagTokens(args);
  let value;
  for (let i = 0; i < tokens.length; i += 1) {
    if (tokens[i] === name) {
      value = tokens[i + 1];
      i += 1; // the value is never a flag of its own
    } else if (tokens[i].startsWith(`${name}=`)) value = tokens[i].slice(name.length + 1);
  }
  return value;
}

// agent-device's rule (src/daemon-resolution.ts): --state-dir, then AGENT_DEVICE_STATE_DIR, then
// ~/.agent-device for an installed runtime; `~` is the home directory, and a relative path starts
// from the working directory.
function stateDir(args, env) {
  const raw = (flag(args, '--state-dir') ?? env.AGENT_DEVICE_STATE_DIR ?? '').trim();
  const home = env.HOME?.trim() || os.homedir();
  if (!raw) return path.join(home, '.agent-device');
  if (raw === '~') return home;
  return path.resolve(raw.startsWith('~/') ? path.join(home, raw.slice(2)) : raw);
}

function usesLocalDaemon(args, env) {
  if (args.length === 0 || args[0] === 'help' || args[0] === 'daemon') return false;
  if (args.length === 1 && (args[0] === '--version' || args[0] === '-V')) return false;
  if (flagTokens(args).some((arg) => arg === '--help' || arg === '-h')) return false;
  return !env.AGENT_DEVICE_DAEMON_BASE_URL?.trim() && flag(args, '--daemon-base-url') === undefined;
}

// The location the record says a daemon of `version` was started from. A record from before
// versions were recorded ({"root"}) speaks for whatever daemon runs there.
function ownerOf(saved, version) {
  if (!saved || version === undefined) return undefined;
  if (saved.roots && typeof saved.roots === 'object') {
    const owner = saved.roots[version];
    return typeof owner === 'string' ? owner : undefined;
  }
  return typeof saved.root === 'string' ? saved.root : undefined;
}

// Records that a daemon of `version` here is started from `root`, keeping what the record says
// about the daemon running now (`running`), when that is another version.
function record(dir, root, version, saved, running) {
  const roots = {};
  const other = running !== undefined && running !== version ? ownerOf(saved, running) : undefined;
  if (other !== undefined) roots[running] = other;
  roots[version] = root;
  try {
    mkdirSync(dir, { recursive: true });
    const temporary = path.join(dir, `${RECORD}.${process.pid}.tmp`);
    writeFileSync(temporary, `${JSON.stringify({ root, roots })}\n`, { mode: 0o600 });
    renameSync(temporary, path.join(dir, RECORD));
  } catch (error) {
    process.stderr.write(`Silicon Extend couldn't record in ${dir} that agent-device's daemon runs from ${root} (${describe(error)}), so the next command may restart the daemon and end its sessions. Make sure ${dir} is writable.\n`);
  }
}

function stopDaemon(dir, info, recorded, root) {
  const run = spawnSync(process.execPath, [cli, 'daemon', 'stop', '--state-dir', dir, '--json'], {
    env: process.env,
    stdio: ['ignore', 'pipe', 'pipe'],
    encoding: 'utf8',
    timeout: STOP_TIMEOUT_MS,
  });
  const reply = readReply(run.stdout);
  if (run.status === 0 && reply?.success === true) return true;
  const reason = run.error?.message ?? reply?.error?.message
    ?? (`${run.stderr ?? ''}\n${run.stdout ?? ''}`.trim().split('\n').pop() || `exit status ${run.status}`);
  const pid = Number.isInteger(info.pid) ? ` (pid ${info.pid})` : '';
  process.stderr.write(`Silicon Extend: agent-device's daemon${pid} was started from ${recorded ?? 'another location'}, not from ${root}, and stopping it failed: ${reason}. This command reuses it, and it fails with "Cannot find module" if that location was moved or deleted. Stop it with: "${process.execPath}" "${cli}" daemon stop --state-dir "${dir}", then try again.\n`);
  return false;
}

function readReply(stdout) {
  try {
    return JSON.parse(stdout);
  } catch {
    return undefined;
  }
}

function sleep(ms) {
  Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);
}

function lockIsStale(lock) {
  try {
    if (Date.now() - statSync(lock).mtimeMs > LOCK_STALE_MS) return true;
    const pid = Number(readFileSync(lock, 'utf8'));
    if (!Number.isInteger(pid) || pid <= 0) return false; // still being written
    process.kill(pid, 0);
    return false;
  } catch (error) {
    return error?.code === 'ESRCH' || error?.code === 'ENOENT';
  }
}

// Commands from the new location can start together; one lock makes sure only the first stops the
// old daemon, and the rest see its record instead of stopping the daemon it has just started.
function withLock(dir, fn) {
  const lock = path.join(dir, `${RECORD}.lock`);
  const deadline = Date.now() + LOCK_STALE_MS;
  for (;;) {
    try {
      const fd = openSync(lock, 'wx', 0o600);
      writeSync(fd, String(process.pid));
      closeSync(fd);
      break;
    } catch (error) {
      if (error?.code !== 'EEXIST') throw error;
      if (lockIsStale(lock)) rmSync(lock, { force: true });
      else if (Date.now() > deadline) throw new Error(`${lock} is still held by another command after ${LOCK_STALE_MS / 1000} seconds`);
      else sleep(50);
    }
  }
  try {
    return fn();
  } finally {
    rmSync(lock, { force: true });
  }
}

function prepare(args, env) {
  if (!usesLocalDaemon(args, env)) return;
  const root = realpathSync(path.dirname(bin));
  const version = readJson(path.join(root, 'package.json'))?.version;
  if (typeof version !== 'string') return; // agent-device reports its own damaged manifest
  const dir = stateDir(args, env);
  const infoPath = path.join(dir, 'daemon.json');
  const recordPath = path.join(dir, RECORD);
  const info = readJson(infoPath);
  if (typeof info?.baseUrl === 'string' && info.baseUrl) return;
  const running = typeof info?.version === 'string' ? info.version : undefined;
  const saved = readJson(recordPath);
  if (running === version) {
    if (ownerOf(saved, version) === root) return; // the daemon here is this location's
    withLock(dir, () => {
      // Another command from here may have replaced the daemon while this one waited.
      const latest = readJson(recordPath);
      const recorded = ownerOf(latest, version);
      if (recorded === root) return;
      const current = readJson(infoPath);
      // A failed stop leaves the record alone, so the next command tries again.
      if (current?.version === version && !stopDaemon(dir, current, recorded, root)) return;
      record(dir, root, version, latest, current?.version);
    });
  } else if (ownerOf(saved, version) !== root) {
    // No daemon, another build of this release, or another release: agent-device starts one from
    // here or replaces the one running, unless that is a newer release it can reach, which it
    // keeps (and whose entry the record keeps). Either way the next daemon of this version here
    // is this location's, so that is recorded now, before agent-device runs.
    record(dir, root, version, saved, running);
  }
}

try {
  prepare(process.argv.slice(2), process.env);
} catch (error) {
  let dir = '<state directory>';
  try { dir = stateDir(process.argv.slice(2), process.env); } catch { /* keep the placeholder */ }
  process.stderr.write(`Silicon Extend couldn't check where agent-device's daemon was started from (${describe(error)}), so this command may reuse a daemon from a moved or deleted copy of Silicon Extend. If it fails with "Cannot find module", stop the daemon with: "${process.execPath}" "${cli}" daemon stop --state-dir "${dir}", then try again.\n`);
}
await import('./agent-device-cli.mjs');
