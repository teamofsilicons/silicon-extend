#!/usr/bin/env node
// Real daemon restart test using isolated copies of a device engine runtime and an empty session
// store. No devices.
//
//   node apps/desktop/runtime-update-e2e.mjs <engine runtime> [--macos-helper <path>]
//
// Pass a packaged runtime to test what ships: "…/Silicon Extend.app/Contents/Resources/engine"
// or "…/lib/silicon-extend/engine" from the Linux tarball. When a Node is bundled beside it
// (../node/bin/node), the test reruns itself under that Node, so every CLI run, daemon and stamp
// uses the shipped Node. The app's macOS helper is found the same way. Each run is made the way
// extend-agent makes it: the environment it sets, and the arguments over stdin. A runtime that
// isn't packaged (vendor/extend-engine) gets Extend's entry (runtime-entry.mjs) installed the way
// packaging installs it.
//
// It covers an idle daemon across an in-place update, an identical reinstall, a moved install, a
// second copy and a deleted copy. It does not cover updating during an active device session.
import assert from 'node:assert/strict';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, renameSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { stampRuntime } from './stamp-runtime.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const usage = 'Usage: node runtime-update-e2e.mjs <engine runtime> [--macos-helper <path>]';
const args = process.argv.slice(2);
const helperFlag = args.indexOf('--macos-helper');
let helper;
if (helperFlag !== -1) {
  helper = args[helperFlag + 1];
  if (!helper) throw new Error(`--macos-helper needs a path. ${usage}`);
  args.splice(helperFlag, 2);
}
const [runtimeArg] = args;
if (!runtimeArg || args.length > 1) throw new Error(usage);
const runtime = path.resolve(runtimeArg);
if (!existsSync(path.join(runtime, 'package.json'))) {
  throw new Error(`${runtime} has no package.json, so it isn't a device engine runtime. ${usage}`);
}

const bundledNode = path.join(runtime, '..', 'node', 'bin', 'node');
if (existsSync(bundledNode) && realpathSync(bundledNode) !== realpathSync(process.execPath)) {
  const rerun = spawnSync(bundledNode, [fileURLToPath(import.meta.url), ...process.argv.slice(2)], { stdio: 'inherit' });
  if (rerun.error) throw new Error(`Couldn't run the bundled Node ${bundledNode}: ${rerun.error.message}`);
  process.exit(rerun.status ?? 1);
}
const appHelper = path.join(runtime, '..', '..', 'MacOS', 'Silicon Extend Helper');
if (!helper && process.platform === 'darwin' && existsSync(appHelper)) helper = appHelper;
const packagedEntry = existsSync(path.join(runtime, 'bin', 'extend-engine-cli.mjs'));

console.log(`node: ${process.execPath} ${process.version}${existsSync(bundledNode) ? ' (bundled with the runtime)' : ' (not a bundled Node: pass a packaged runtime to test what ships)'}`);
console.log(`runtime: ${runtime}`);
console.log(`entry: ${packagedEntry ? "the runtime's own Extend entry" : 'apps/desktop/runtime-entry.mjs, installed here the way packaging does'}`);
if (helper) console.log(`macOS helper: ${helper}`);

const scratch = mkdtempSync(path.join(os.tmpdir(), 'extend-runtime-update-'));
const state = path.join(scratch, 'state');
const marker = path.join(scratch, 'started-build');
// What extend-agent's run_process sets (crates/extend-agent/src/drivers/agent_device.rs), under the
// engine's EXTEND_ENGINE_* names; no engine setting from this shell, under either name, leaks in.
const env = Object.fromEntries(Object.entries(process.env).filter(([key]) => !key.startsWith('AGENT_DEVICE_') && !key.startsWith('EXTEND_ENGINE_')));
Object.assign(env, {
  EXTEND_ENGINE_STATE_DIR: state,
  EXTEND_ENGINE_NO_UPDATE_NOTIFIER: '1',
  EXTEND_ENGINE_JSON_TEXT: '1',
  NO_COLOR: '1',
  EXTEND_ARTIFACT_MARKER_PATH: marker,
});
if (helper) env.EXTEND_ENGINE_MACOS_HELPER_BIN = path.resolve(helper);
// extend-agent's ARGS_FROM_STDIN, in the same file.
const ARGS_FROM_STDIN = "import{pathToFileURL}from'node:url';let s='';process.stdin.setEncoding('utf8');for await(const c of process.stdin)s+=c;process.argv.push(...JSON.parse(s));await import(pathToFileURL(process.argv[1]).href);";

// Everything a package ships, without build caches the app and the .deb leave out.
const shipped = ['bin', 'dist', 'package.json', 'LICENSE', 'linux', 'apple'];
const skipped = new Set(['.build', '.swiftpm', 'DerivedData', 'xcuserdata', 'node_modules']);
function copyRuntime(from, to) {
  mkdirSync(to);
  for (const name of shipped) {
    if (!existsSync(path.join(from, name))) continue;
    cpSync(path.join(from, name), path.join(to, name), { recursive: true, filter: (source) => !skipped.has(path.basename(source)) });
  }
  if (!packagedEntry) {
    renameSync(path.join(to, 'bin/extend-engine.mjs'), path.join(to, 'bin/extend-engine-cli.mjs'));
    cpSync(path.join(here, 'runtime-entry.mjs'), path.join(to, 'bin/extend-engine.mjs'));
  }
}

// An unstamped build whose daemon writes which build it is and where it was started from.
function stage(label, to) {
  copyRuntime(runtime, to);
  const manifestPath = path.join(to, 'package.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  manifest.version = manifest.extendRuntime?.upstreamVersion ?? manifest.version;
  delete manifest.extendRuntime;
  writeFileSync(manifestPath, JSON.stringify(manifest));
  const entry = path.join(to, 'dist/src/internal/daemon.js');
  assert.ok(existsSync(entry), 'expected the packaged daemon entry');
  writeFileSync(entry, `import { writeFileSync as writeExtendBuildMarker } from 'node:fs';\nwriteExtendBuildMarker(process.env.EXTEND_ARTIFACT_MARKER_PATH, JSON.stringify({ build: '${label}', entry: process.argv[1] }));\n${readFileSync(entry, 'utf8')}`);
}

let active;
function cli(root, cliArgs, { entry = 'bin/extend-engine.mjs' } = {}) {
  const result = spawnSync(process.execPath, ['--input-type=module', '-e', ARGS_FROM_STDIN, '--', path.join(root, entry)], {
    env, cwd: scratch, encoding: 'utf8', timeout: 60_000, input: JSON.stringify([...cliArgs, '--json']),
  });
  assert.equal(result.status, 0, result.error?.message ?? result.stderr + result.stdout);
  // The engine may print a "Replacing daemon" line before its JSON reply, as extend-agent allows.
  const response = JSON.parse(result.stdout.slice(result.stdout.indexOf('{')));
  assert.equal(response.success, true, result.stdout);
  active = root;
  return response;
}
function info() { return JSON.parse(readFileSync(path.join(state, 'daemon.json'), 'utf8')); }
function daemonPid() {
  try { return info().pid; } catch { return undefined; }
}
function started() { return JSON.parse(readFileSync(marker, 'utf8')); }
function startedFrom(root) {
  const { entry } = started();
  return path.relative(realpathSync(root), entry) === path.join('dist', 'src', 'internal', 'daemon.js');
}
function recordedRoot() { return JSON.parse(readFileSync(path.join(state, 'extend-runtime-root.json'), 'utf8')).root; }

let passed = false;
try {
  // An update in place: build A, then build B at the same location.
  const install = path.join(scratch, 'install');
  stage('A', install);
  cli(install, ['session', 'list']);
  const original = info();
  assert.equal(started().build, 'A');
  rmSync(install, { recursive: true });
  stage('B', install);
  cli(install, ['session', 'list']);
  assert.equal(info().pid, original.pid, 'unstamped baseline should reproduce stale reuse');
  assert.equal(started().build, 'A');
  console.log('REPRODUCED: a changed unstamped runtime at the same location reused the previous daemon');

  const version = stampRuntime(install, helper ? [helper] : []);
  cli(install, ['session', 'list']);
  const updated = info();
  assert.notEqual(updated.pid, original.pid);
  assert.equal(updated.version, version);
  assert.equal(started().build, 'B', 'new daemon must execute new artifact code');
  console.log(`PASS: changed stamped runtime ${version} replaced the old daemon and executed build B`);

  // The same build reinstalled at the same location, with new timestamps.
  const reinstall = path.join(scratch, 'reinstall');
  cpSync(install, reinstall, { recursive: true });
  utimesSync(path.join(reinstall, 'dist/src/internal/daemon.js'), new Date(0), new Date(0));
  rmSync(install, { recursive: true });
  renameSync(reinstall, install);
  assert.equal(stampRuntime(install, helper ? [helper] : []), version);
  cli(install, ['session', 'list']);
  assert.equal(info().pid, updated.pid, 'an identical reinstall at the same location must not restart the daemon');
  assert.equal(recordedRoot(), realpathSync(install));
  console.log('PASS: an identical reinstall at the same location (new timestamps) reused the daemon');

  // A moved install (Downloads to Applications). Without Extend's entry, the old daemon is reused.
  const moved = path.join(scratch, 'Applications', 'moved');
  mkdirSync(path.dirname(moved));
  renameSync(install, moved);
  cli(moved, ['session', 'list'], { entry: 'bin/extend-engine-cli.mjs' });
  assert.equal(info().pid, updated.pid);
  assert.equal(existsSync(path.dirname(started().entry)), false, 'the reused daemon was started from a location that is gone');
  console.log("REPRODUCED: without Extend's entry, the moved install reused a daemon whose location is gone");
  cli(moved, ['session', 'list']);
  const afterMove = info();
  assert.notEqual(afterMove.pid, updated.pid, 'a daemon started from the old location must be replaced');
  assert.equal(afterMove.version, version);
  assert.ok(startedFrom(moved), `the new daemon must run from the moved install, not ${started().entry}`);
  assert.equal(recordedRoot(), realpathSync(moved));
  console.log('PASS: after the install moved, the daemon its old location started was replaced');

  // A second, identical copy while the first remains, then that copy deleted.
  const copy = path.join(scratch, 'copy');
  cpSync(moved, copy, { recursive: true });
  cli(copy, ['session', 'list']);
  const onCopy = info();
  assert.notEqual(onCopy.pid, afterMove.pid);
  assert.ok(startedFrom(copy));
  cli(copy, ['session', 'list']);
  assert.equal(info().pid, onCopy.pid, 'the copy reuses the daemon it started');
  console.log('PASS: an identical copy at another location started its own daemon, then reused it');
  rmSync(copy, { recursive: true });
  cli(moved, ['session', 'list']);
  assert.notEqual(info().pid, onCopy.pid);
  assert.ok(startedFrom(moved));
  console.log('PASS: after the copy that started the daemon was deleted, the remaining install replaced it');
  passed = true;
} finally {
  // Use the runtime's ownership-aware stop, never a guessed PID or global process sweep. A failed
  // stop must not hide the test's own failure.
  let stopError;
  try {
    if (!active || !existsSync(active)) throw new Error('no runtime copy is left to stop the daemon with');
    cli(active, ['daemon', 'stop', '--clean']);
    assert.equal(existsSync(path.join(state, 'daemon.json')), false, 'daemon stop --clean left daemon.json behind');
  } catch (error) {
    stopError = error;
  }
  if (stopError) {
    const pid = daemonPid();
    console.error(`Couldn't stop the test daemon${pid ? ` (pid ${pid})` : ''}: ${stopError.message}\n`
      + `It exits by itself after about 5 idle minutes${pid ? `, or stop it now with: kill ${pid}` : ''}. `
      + `Its files are kept in ${scratch} until then; delete that directory afterwards.`);
    if (passed) throw stopError;
  } else {
    rmSync(scratch, { recursive: true, force: true });
  }
}
