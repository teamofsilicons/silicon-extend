#!/usr/bin/env node
// Real daemon restart test using isolated artifact copies and an empty session store. No devices.
import assert from 'node:assert/strict';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { stampRuntime } from './stamp-runtime.mjs';

const [runtime] = process.argv.slice(2);
if (!runtime) throw new Error('Usage: node runtime-update-e2e.mjs <built-agent-device>');
const scratch = mkdtempSync(path.join(os.tmpdir(), 'extend-runtime-update-'));
const state = path.join(scratch, 'state');
const marker = path.join(scratch, 'started-build');
const env = { ...process.env, AGENT_DEVICE_STATE_DIR: state, EXTEND_ARTIFACT_MARKER_PATH: marker };
delete env.AGENT_DEVICE_DAEMON_BASE_URL;
delete env.AGENT_DEVICE_DAEMON_AUTH_TOKEN;
const artifacts = ['A', 'B'].map((label) => {
  const root = path.join(scratch, label);
  mkdirSync(root);
  for (const name of ['bin', 'dist', 'package.json']) cpSync(path.join(runtime, name), path.join(root, name), { recursive: true });
  const manifestPath = path.join(root, 'package.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  manifest.version = manifest.extendRuntime?.upstreamVersion ?? manifest.version;
  delete manifest.extendRuntime;
  writeFileSync(manifestPath, JSON.stringify(manifest));
  const entry = path.join(root, 'dist/src/internal/daemon.js');
  assert.ok(existsSync(entry), 'expected the packaged daemon entry');
  writeFileSync(entry, `import { writeFileSync as writeExtendBuildMarker } from 'node:fs';\nwriteExtendBuildMarker(process.env.EXTEND_ARTIFACT_MARKER_PATH, '${label}');\n${readFileSync(entry, 'utf8')}`);
  return root;
});
const [a, b] = artifacts;
let active = a;
function cli(root, args) {
  active = root;
  const result = spawnSync(process.execPath, [path.join(root, 'bin/agent-device.mjs'), ...args, '--state-dir', state, '--json'], { env, encoding: 'utf8', timeout: 30_000 });
  assert.equal(result.status, 0, result.error?.message ?? result.stderr + result.stdout);
  const response = JSON.parse(result.stdout);
  assert.equal(response.success, true, result.stdout);
  return response;
}
function info() { return JSON.parse(readFileSync(path.join(state, 'daemon.json'), 'utf8')); }
try {
  cli(a, ['session', 'list']);
  const original = info();
  assert.equal(readFileSync(marker, 'utf8'), 'A');
  cli(b, ['session', 'list']);
  assert.equal(info().pid, original.pid, 'unstamped baseline should reproduce stale reuse');
  assert.equal(readFileSync(marker, 'utf8'), 'A');
  console.log('REPRODUCED: changed unstamped runtime reused the previous daemon');

  const version = stampRuntime(b);
  cli(b, ['session', 'list']);
  const updated = info();
  assert.notEqual(updated.pid, original.pid);
  assert.equal(updated.version, version);
  assert.equal(readFileSync(marker, 'utf8'), 'B', 'new daemon must execute new artifact code');
  console.log('PASS: changed stamped runtime replaced the old daemon and executed build B');

  const relocated = path.join(scratch, 'relocated');
  cpSync(b, relocated, { recursive: true });
  utimesSync(path.join(relocated, 'dist/src/internal/daemon.js'), new Date(0), new Date(0));
  assert.equal(stampRuntime(relocated), version);
  cli(relocated, ['session', 'list']);
  assert.equal(info().pid, updated.pid, 'identical relocated builds must not restart the daemon');
  console.log('PASS: identical relocated runtime reused the existing daemon');
} finally {
  // Use the runtime's ownership-aware stop, never a guessed PID or global process sweep.
  cli(active, ['daemon', 'stop', '--clean']);
  assert.equal(existsSync(path.join(state, 'daemon.json')), false);
  rmSync(scratch, { recursive: true, force: true });
}
