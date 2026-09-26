import assert from 'node:assert/strict';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, utimesSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { stampRuntime } from './stamp-runtime.mjs';

function fixture(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'extend-runtime-stamp-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const staged = path.join(root, 'first');
  for (const dir of ['bin', 'dist']) mkdirSync(path.join(staged, dir), { recursive: true });
  writeFileSync(path.join(staged, 'package.json'), JSON.stringify({ name: 'agent-device', version: '0.21.15' }));
  writeFileSync(path.join(staged, 'bin/agent-device.mjs'), "import '../dist/main.js';");
  writeFileSync(path.join(staged, 'dist/main.js'), "export const behavior = 'old';");
  return { root, staged };
}

test('identical relocated installs with different timestamps reuse the same build version', (t) => {
  const { root, staged } = fixture(t);
  const first = stampRuntime(staged);
  const copy = path.join(root, 'second');
  cpSync(staged, copy, { recursive: true });
  utimesSync(path.join(copy, 'dist/main.js'), new Date(0), new Date(0));
  assert.equal(stampRuntime(copy), first);
  assert.equal(stampRuntime(staged), first);
});

test('a same-size rewrite with the original timestamp changes the version', (t) => {
  const { staged } = fixture(t);
  const first = stampRuntime(staged);
  const file = path.join(staged, 'dist/main.js');
  const before = statSync(file);
  writeFileSync(file, "export const behavior = 'new';");
  utimesSync(file, before.atime, before.mtime);
  assert.notEqual(stampRuntime(staged), first);
  assert.equal(JSON.parse(readFileSync(path.join(staged, 'package.json'))).extendRuntime.upstreamVersion, '0.21.15');
});

test('native helper changes also invalidate the build identity', (t) => {
  const { root, staged } = fixture(t);
  const helper = path.join(root, 'native-helper');
  writeFileSync(helper, 'old');
  const first = stampRuntime(staged, [helper]);
  writeFileSync(helper, 'new');
  assert.notEqual(stampRuntime(staged, [helper]), first);
});

test('incomplete runtime trees are rejected without stamping the manifest', (t) => {
  const { staged } = fixture(t);
  rmSync(path.join(staged, 'dist'), { recursive: true });
  assert.throws(() => stampRuntime(staged), { code: 'ENOENT' });
  assert.equal(JSON.parse(readFileSync(path.join(staged, 'package.json'))).version, '0.21.15');
});
