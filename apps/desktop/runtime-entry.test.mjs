// Checks for runtime-entry.mjs, Extend's entry in front of the packaged agent-device CLI, run
// against a stand-in for agent-device that logs every call and keeps a fake daemon.json.
// runtime-update-e2e.mjs runs the same cases against the real agent-device daemon.
import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, renameSync, rmSync, statSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const VERSION = '0.21.15+extend.1111111111111111111111111111111111111111111111111111111111111111';
const RECORD = 'extend-runtime-root.json';
// How extend-agent runs a node entry: arguments over stdin (crates/extend-agent/src/drivers/agent_device.rs).
const ARGS_FROM_STDIN = "import{pathToFileURL}from'node:url';let s='';process.stdin.setEncoding('utf8');for await(const c of process.stdin)s+=c;process.argv.push(...JSON.parse(s));await import(pathToFileURL(process.argv[1]).href);";

// Logs each call, and keeps a daemon.json that says which location "started" it. Like agent-device,
// it replaces a daemon of another version unless that daemon's release is newer.
const FAKE_CLI = String.raw`
import { appendFileSync, existsSync, mkdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
const root = realpathSync(path.join(path.dirname(fileURLToPath(import.meta.url)), '..'));
const version = JSON.parse(readFileSync(path.join(root, 'package.json'), 'utf8')).version;
const args = process.argv.slice(2);
// Like agent-device, flags end at '--'; what follows is text.
const flags = args.includes('--') ? args.slice(0, args.indexOf('--')) : args;
const at = flags.indexOf('--state-dir');
const given = at === -1 ? process.env.AGENT_DEVICE_STATE_DIR : flags[at + 1];
// Like agent-device, ~ is the home directory (else "~/x" would be created under the working directory).
const dir = given.startsWith('~/') ? path.join(os.homedir(), given.slice(2)) : given;
mkdirSync(dir, { recursive: true });
// And the runner settings this call would start a daemon with.
const runner = { idleStopMs: process.env.AGENT_DEVICE_IOS_RUNNER_IDLE_STOP_MS ?? null, detach: process.env.AGENT_DEVICE_IOS_RUNNER_DETACH ?? null };
appendFileSync(path.join(dir, 'calls.log'), JSON.stringify({ root, args, runner }) + '\n');
const infoPath = path.join(dir, 'daemon.json');
const release = (v) => v.split('+')[0].split('.').map(Number);
const newer = (a, b) => { const x = release(a), y = release(b); for (let i = 0; i < 3; i += 1) if (x[i] !== y[i]) return x[i] > y[i]; return false; };
if (args[0] === 'daemon') {
  if (existsSync(path.join(dir, 'stop-slow'))) Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 700);
  if (existsSync(path.join(dir, 'stop-fails'))) {
    console.log(JSON.stringify({ success: false, error: { message: "The daemon didn't stop" } }));
    process.exit(1);
  }
  const stopped = existsSync(infoPath);
  rmSync(infoPath, { force: true });
  console.log(JSON.stringify({ success: true, data: { stopped } }));
} else {
  let info = existsSync(infoPath) ? JSON.parse(readFileSync(infoPath, 'utf8')) : null;
  if (!info || (info.version !== version && !newer(info.version, version))) {
    info = { pid: 4242, token: 't', version, startedFrom: root };
    writeFileSync(infoPath, JSON.stringify(info));
    // A command killed (a timeout, SIGKILL) once its daemon is up, before it could exit.
    if (existsSync(path.join(dir, 'die-after-start'))) process.kill(process.pid, 'SIGKILL');
  }
  console.log(JSON.stringify({ success: true, data: { daemonFrom: info.startedFrom, daemonVersion: info.version } }));
}
`;

function install(t, { version = VERSION } = {}) {
  const scratch = mkdtempSync(path.join(os.tmpdir(), 'extend-runtime-entry-'));
  t.after(() => rmSync(scratch, { recursive: true, force: true }));
  const root = path.join(scratch, 'A');
  mkdirSync(path.join(root, 'bin'), { recursive: true });
  writeFileSync(path.join(root, 'package.json'), JSON.stringify({ name: 'agent-device', version }));
  cpSync(path.join(here, 'runtime-entry.mjs'), path.join(root, 'bin/agent-device.mjs'));
  writeFileSync(path.join(root, 'bin/agent-device-cli.mjs'), FAKE_CLI);
  const state = path.join(scratch, 'state');
  return { scratch, root, state };
}

function run(root, args, { state, env = {}, viaStdin = false } = {}) {
  const base = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('AGENT_DEVICE_')));
  const fullEnv = { ...base, AGENT_DEVICE_STATE_DIR: state, ...env };
  const entry = path.join(root, 'bin/agent-device.mjs');
  return viaStdin
    ? spawnSync(process.execPath, ['--input-type=module', '-e', ARGS_FROM_STDIN, '--', entry], { env: fullEnv, input: JSON.stringify(args), encoding: 'utf8' })
    : spawnSync(process.execPath, [entry, ...args], { env: fullEnv, encoding: 'utf8' });
}

function ok(result) {
  assert.equal(result.status, 0, result.stderr + result.stdout);
  return JSON.parse(result.stdout).data;
}

function calls(state) {
  const log = path.join(state, 'calls.log');
  if (!existsSync(log)) return [];
  return readFileSync(log, 'utf8').trim().split('\n').map((line) => JSON.parse(line));
}

function stops(state) {
  return calls(state).filter((call) => call.args[0] === 'daemon');
}

// The location the record says a daemon of `version` here was started from.
function recorded(state, version = VERSION) {
  try {
    const saved = JSON.parse(readFileSync(path.join(state, RECORD), 'utf8'));
    return saved.roots ? saved.roots[version] : saved.root;
  } catch {
    return undefined;
  }
}

function writeDaemon(state, info) {
  mkdirSync(state, { recursive: true });
  writeFileSync(path.join(state, 'daemon.json'), JSON.stringify({ pid: 4242, token: 't', ...info }));
}

test('a daemon started from the same location is reused, and the record is not rewritten', (t) => {
  const { root, state } = install(t);
  const real = realpathSync(root);
  assert.equal(ok(run(root, ['session', 'list', '--json'], { state })).daemonFrom, real);
  assert.equal(recorded(state), real);
  utimesSync(path.join(state, RECORD), new Date(0), new Date(0));
  assert.equal(ok(run(root, ['snapshot', '--json'], { state })).daemonFrom, real);
  assert.deepEqual(stops(state), []);
  assert.equal(statSync(path.join(state, RECORD)).mtimeMs, 0);
});

test('a moved install replaces the daemon its old location started', (t) => {
  const { scratch, root, state } = install(t);
  ok(run(root, ['session', 'list', '--json'], { state }));
  const moved = path.join(scratch, 'Applications');
  renameSync(root, moved);
  const result = run(moved, ['session', 'list', '--json'], { state, viaStdin: true });
  const real = realpathSync(moved);
  assert.equal(ok(result).daemonFrom, real, 'the command must run on a daemon started from the new location');
  assert.deepEqual(stops(state).map((call) => call.args), [['daemon', 'stop', '--state-dir', path.resolve(state), '--json']]);
  assert.equal(stops(state)[0].root, real, "the stop runs the new location's own agent-device");
  assert.equal(recorded(state), real);
  assert.equal(result.stderr, '');
  assert.deepEqual(Object.keys(JSON.parse(result.stdout)), ['success', 'data'], "stdout carries only agent-device's reply");
});

test('an identical copy elsewhere replaces the daemon, even while the original remains', (t) => {
  const { scratch, root, state } = install(t);
  ok(run(root, ['session', 'list', '--json'], { state }));
  const copy = path.join(scratch, 'B');
  cpSync(root, copy, { recursive: true });
  assert.equal(ok(run(copy, ['session', 'list', '--json'], { state })).daemonFrom, realpathSync(copy));
  assert.equal(stops(state).length, 1);
  // And back: every change of location restarts the daemon once, then it is reused.
  assert.equal(ok(run(root, ['session', 'list', '--json'], { state })).daemonFrom, realpathSync(root));
  ok(run(root, ['session', 'list', '--json'], { state }));
  assert.equal(stops(state).length, 2);
});

test('a symlinked path to the same install is the same location', (t) => {
  const { scratch, root, state } = install(t);
  const link = path.join(scratch, 'link');
  symlinkSync(root, link, 'dir');
  ok(run(link, ['session', 'list', '--json'], { state }));
  assert.equal(recorded(state), realpathSync(root));
  ok(run(root, ['session', 'list', '--json'], { state }));
  assert.deepEqual(stops(state), []);
});

function runAsync(root, args, state) {
  const env = { ...Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('AGENT_DEVICE_'))), AGENT_DEVICE_STATE_DIR: state };
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [path.join(root, 'bin/agent-device.mjs'), ...args], { env });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', (chunk) => { stdout += chunk; });
    child.stderr.on('data', (chunk) => { stderr += chunk; });
    child.on('close', (status) => resolve({ status, stdout, stderr }));
  });
}

test('commands from a new location at the same time stop the old daemon once', async (t) => {
  const { scratch, root, state } = install(t);
  ok(run(root, ['session', 'list', '--json'], { state }));
  writeFileSync(path.join(state, 'stop-slow'), '');
  const moved = path.join(scratch, 'moved');
  renameSync(root, moved);
  const results = await Promise.all([1, 2, 3, 4].map(() => runAsync(moved, ['session', 'list', '--json'], state)));
  for (const result of results) assert.equal(ok(result).daemonFrom, realpathSync(moved));
  assert.equal(stops(state).length, 1, 'only the first command stops the old daemon');
  assert.equal(recorded(state), realpathSync(moved));
  assert.equal(existsSync(path.join(state, `${RECORD}.lock`)), false);
});

test('a lock left by a command that died is taken over', (t) => {
  const { scratch, root, state } = install(t);
  ok(run(root, ['session', 'list', '--json'], { state }));
  const dead = spawnSync(process.execPath, ['-e', 'process.stdout.write(String(process.pid))'], { encoding: 'utf8' }).stdout;
  writeFileSync(path.join(state, `${RECORD}.lock`), dead);
  const moved = path.join(scratch, 'moved');
  renameSync(root, moved);
  assert.equal(ok(run(moved, ['session', 'list', '--json'], { state })).daemonFrom, realpathSync(moved));
  assert.equal(stops(state).length, 1);
  assert.equal(existsSync(path.join(state, `${RECORD}.lock`)), false);
});

test('a daemon of this version with no record of its location is replaced', (t) => {
  const { root, state } = install(t);
  writeDaemon(state, { version: VERSION, startedFrom: '/somewhere/else' });
  assert.equal(ok(run(root, ['session', 'list', '--json'], { state })).daemonFrom, realpathSync(root));
  assert.equal(stops(state).length, 1);
});

test('a newer daemon is left to agent-device, and its record is kept', (t) => {
  const { root, state } = install(t);
  writeDaemon(state, { version: '0.22.0+extend.2222', startedFrom: '/Applications/newer' });
  writeFileSync(path.join(state, RECORD), JSON.stringify({ root: '/Applications/newer' }));
  assert.equal(ok(run(root, ['session', 'list', '--json'], { state })).daemonVersion, '0.22.0+extend.2222');
  assert.deepEqual(stops(state), []);
  assert.equal(recorded(state, '0.22.0+extend.2222'), '/Applications/newer');
});

test("an older copy's claim never makes the newer location stop its own daemon", (t) => {
  const { scratch, root, state } = install(t);
  const newer = path.join(scratch, 'Newer');
  cpSync(root, newer, { recursive: true });
  writeFileSync(path.join(newer, 'package.json'), JSON.stringify({ name: 'agent-device', version: '0.22.0+extend.2222' }));
  assert.equal(ok(run(newer, ['session', 'list', '--json'], { state })).daemonFrom, realpathSync(newer));
  // The older copy runs once (agent-device keeps the newer daemon), then the newer one again.
  assert.equal(ok(run(root, ['session', 'list', '--json'], { state })).daemonVersion, '0.22.0+extend.2222');
  assert.equal(ok(run(newer, ['session', 'list', '--json'], { state })).daemonFrom, realpathSync(newer));
  assert.deepEqual(stops(state), [], 'nobody stopped the newer daemon');
  assert.equal(recorded(state, '0.22.0+extend.2222'), realpathSync(newer));
});

test('a first command after an update that is killed still leaves the record', (t) => {
  const { root, state } = install(t);
  writeDaemon(state, { version: '0.20.0', startedFrom: '/Applications/older' });
  writeFileSync(path.join(state, RECORD), JSON.stringify({ root: '/Applications/older' }));
  writeFileSync(path.join(state, 'die-after-start'), '');
  const killed = run(root, ['session', 'list', '--json'], { state });
  assert.equal(killed.signal, 'SIGKILL', 'the command was killed after agent-device replaced the daemon');
  assert.equal(recorded(state), realpathSync(root));
  rmSync(path.join(state, 'die-after-start'));
  assert.equal(ok(run(root, ['snapshot', '--json'], { state })).daemonFrom, realpathSync(root));
  assert.deepEqual(stops(state), [], 'the next command keeps the daemon this location started');
});

test('an older release is replaced by agent-device and recorded once it is', (t) => {
  const { root, state } = install(t);
  writeDaemon(state, { version: '0.20.0', startedFrom: '/Applications/older' });
  writeFileSync(path.join(state, RECORD), JSON.stringify({ root: '/Applications/older' }));
  assert.equal(ok(run(root, ['session', 'list', '--json'], { state })).daemonFrom, realpathSync(root));
  assert.deepEqual(stops(state), []);
  assert.equal(recorded(state), realpathSync(root));
  ok(run(root, ['session', 'list', '--json'], { state }));
  assert.deepEqual(stops(state), [], 'the next command reuses the daemon it started');
});

test('an update of the same release (a new build) is recorded before agent-device replaces it', (t) => {
  const { root, state } = install(t);
  writeDaemon(state, { version: '0.21.15+extend.0000', startedFrom: '/Applications/old-build' });
  writeFileSync(path.join(state, RECORD), JSON.stringify({ root: '/Applications/old-build' }));
  ok(run(root, ['session', 'list', '--json'], { state }));
  assert.deepEqual(stops(state), []);
  assert.equal(recorded(state), realpathSync(root));
});

test('a failed stop is reported with what to do, and the command still runs', (t) => {
  const { scratch, root, state } = install(t);
  ok(run(root, ['session', 'list', '--json'], { state }));
  writeFileSync(path.join(state, 'stop-fails'), '');
  const moved = path.join(scratch, 'moved');
  renameSync(root, moved);
  const result = run(moved, ['session', 'list', '--json'], { state });
  assert.equal(result.status, 0);
  assert.deepEqual(Object.keys(JSON.parse(result.stdout)), ['success', 'data']);
  assert.match(result.stderr, /agent-device's daemon \(pid 4242\) was started from .*\/A, not from .*\/moved, and stopping it failed: The daemon didn't stop\./);
  assert.match(result.stderr, /Cannot find module/);
  assert.match(result.stderr, /daemon stop --state-dir ".*state"/);
  assert.equal(recorded(state), realpathSync(scratch) + '/A', 'the record is kept, so the next command tries again');
  rmSync(path.join(state, 'stop-fails'));
  ok(run(moved, ['session', 'list', '--json'], { state }));
  assert.equal(recorded(state), realpathSync(moved));
});

for (const [label, args, env] of [
  ['--version', ['--version'], {}],
  ['help', ['help', 'open'], {}],
  ['a --help run', ['session', 'list', '--help'], {}],
  ['daemon stop', ['daemon', 'stop', '--json'], {}],
  ['a remote daemon (environment)', ['session', 'list', '--json'], { AGENT_DEVICE_DAEMON_BASE_URL: 'https://daemon.example' }],
  ['a remote daemon (flag)', ['session', 'list', '--daemon-base-url=https://daemon.example', '--json'], {}],
]) {
  test(`${label} leaves the daemon alone`, (t) => {
    const { root, state } = install(t);
    writeDaemon(state, { version: VERSION, startedFrom: '/Applications/other' });
    writeFileSync(path.join(state, RECORD), JSON.stringify({ root: '/Applications/other' }));
    run(root, args, { state, env });
    assert.deepEqual(calls(state).filter((call) => call.args.includes('--state-dir')), [], 'no stop from the entry');
    assert.equal(recorded(state), '/Applications/other');
  });
}

for (const spelling of ['separate', 'joined']) {
  test(`--state-dir (${spelling}) wins over AGENT_DEVICE_STATE_DIR, and ~ is the home directory`, (t) => {
    const { scratch, root, state } = install(t);
    const home = path.join(scratch, 'home');
    const flagged = path.join(home, 'flagged');
    writeDaemon(flagged, { version: VERSION, startedFrom: '/Applications/other' });
    const flag = spelling === 'separate' ? ['--state-dir', '~/flagged'] : ['--state-dir=~/flagged'];
    const result = run(root, ['session', 'list', ...flag, '--json'], { state, env: { HOME: home } });
    assert.equal(result.status, 0, result.stderr);
    assert.deepEqual(stops(flagged).map((call) => call.args), [['daemon', 'stop', '--state-dir', flagged, '--json']]);
    assert.equal(recorded(flagged), realpathSync(root));
    assert.equal(existsSync(path.join(state, RECORD)), false, 'the environment directory is not the one in use');
  });
}

for (const [label, args] of [
  ['joined', ['type', '--', '--state-dir=~/notes']],
  ['separate', ['type', '--', '--state-dir', '~/notes']],
]) {
  test(`text after -- is never read as --state-dir (${label})`, (t) => {
    const { scratch, root, state } = install(t);
    const home = path.join(scratch, 'home');
    mkdirSync(home);
    const result = run(root, args, { state, env: { HOME: home } });
    assert.equal(result.status, 0, result.stderr);
    assert.equal(existsSync(path.join(home, 'notes')), false, 'the typed text created a directory');
    assert.equal(recorded(state), realpathSync(root), 'the record went to the state directory in use');
    assert.deepEqual(calls(state).map((call) => call.args), [args]);
  });
}

for (const [label, args] of [
  ['--help', ['fill', '@e1', '--', '--help']],
  ['-h', ['type', '--', '-h']],
  ['--daemon-base-url', ['type', '--', '--daemon-base-url=https://daemon.example']],
]) {
  test(`${label} typed after -- still gets the daemon checked`, (t) => {
    const { root, state } = install(t);
    writeDaemon(state, { version: VERSION, startedFrom: '/Applications/other' });
    writeFileSync(path.join(state, RECORD), JSON.stringify({ root: '/Applications/other' }));
    assert.equal(ok(run(root, args, { state })).daemonFrom, realpathSync(root));
    assert.equal(stops(state).length, 1, "the other location's daemon was replaced");
    assert.equal(recorded(state), realpathSync(root));
  });
}

test('agent-device gets the runner defaults, and a value already in the environment wins', (t) => {
  const { root, state } = install(t);
  ok(run(root, ['snapshot', '--json'], { state }));
  ok(run(root, ['snapshot', '--json'], { state, viaStdin: true }));
  ok(run(root, ['snapshot', '--json'], {
    state,
    env: { AGENT_DEVICE_IOS_RUNNER_IDLE_STOP_MS: '0', AGENT_DEVICE_IOS_RUNNER_DETACH: '1' },
  }));
  assert.deepEqual(calls(state).map((call) => call.runner), [
    { idleStopMs: '30000', detach: '0' },
    { idleStopMs: '30000', detach: '0' },
    { idleStopMs: '0', detach: '1' },
  ]);
});

test('a runtime whose manifest has no version is handed to agent-device unchecked', (t) => {
  const { root, state } = install(t);
  writeFileSync(path.join(root, 'package.json'), JSON.stringify({ name: 'agent-device' }));
  writeDaemon(state, { version: VERSION });
  run(root, ['session', 'list', '--json'], { state });
  assert.deepEqual(stops(state), []);
});
