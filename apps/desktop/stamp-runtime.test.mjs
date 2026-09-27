import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, symlinkSync, utimesSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { REQUIRED_ENTRIES, STAMPED_VERSION, stampRuntime } from './stamp-runtime.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));
const vendored = path.resolve(here, '../../vendor/extend-engine');

function fixture(t) {
  const root = mkdtempSync(path.join(os.tmpdir(), 'extend-runtime-stamp-'));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const staged = path.join(root, 'first');
  for (const dir of ['bin', 'dist/src/internal']) mkdirSync(path.join(staged, dir), { recursive: true });
  writeFileSync(path.join(staged, 'package.json'), JSON.stringify({ name: 'silicon-extend-engine', version: '0.21.15' }));
  writeFileSync(path.join(staged, 'bin/extend-engine.mjs'), "await import('../dist/src/internal/bin.js');");
  writeFileSync(path.join(staged, 'dist/src/internal/bin.js'), 'const help = () => import(`../help.js`); export { help };');
  writeFileSync(path.join(staged, 'dist/src/internal/daemon.js'), 'import{run}from"../main.js";import"../side.js";run();');
  // Loaded by computed paths (new Worker(path), spawn(node, [path])), so only REQUIRED_ENTRIES covers
  // them; and the EXTEND_ENGINE_* settings shim, which Extend's entry imports.
  for (const entry of ['png-worker', 'companion-tunnel', 'run-script-http-child', 'update-check-entry', 'extend-env']) {
    writeFileSync(path.join(staged, `dist/src/internal/${entry}.js`), `export const entry = '${entry}';`);
  }
  writeFileSync(path.join(staged, 'dist/src/main.js'), "export const run = () => 'old';");
  writeFileSync(path.join(staged, 'dist/src/help.js'), "export const text = 'help';");
  writeFileSync(path.join(staged, 'dist/src/side.js'), '');
  return { root, staged };
}

function manifest(staged) {
  return JSON.parse(readFileSync(path.join(staged, 'package.json'), 'utf8'));
}

function runCli(script, args) {
  return spawnSync(process.execPath, [script, ...args], { encoding: 'utf8' });
}

test('identical relocated installs with different timestamps reuse the same build version', (t) => {
  const { root, staged } = fixture(t);
  const first = stampRuntime(staged);
  assert.match(first, STAMPED_VERSION);
  const copy = path.join(root, 'second');
  cpSync(staged, copy, { recursive: true });
  utimesSync(path.join(copy, 'dist/src/main.js'), new Date(0), new Date(0));
  assert.equal(stampRuntime(copy), first);
  assert.equal(stampRuntime(staged), first);
});

test('a same-size rewrite with the original timestamp changes the version', (t) => {
  const { staged } = fixture(t);
  const first = stampRuntime(staged);
  const file = path.join(staged, 'dist/src/main.js');
  const before = statSync(file);
  writeFileSync(file, "export const run = () => 'new';");
  utimesSync(file, before.atime, before.mtime);
  assert.notEqual(stampRuntime(staged), first);
  assert.equal(manifest(staged).extendRuntime.upstreamVersion, '0.21.15');
});

test('native helper changes also invalidate the build identity', (t) => {
  const { root, staged } = fixture(t);
  const helper = path.join(root, 'native-helper');
  writeFileSync(helper, 'old');
  const first = stampRuntime(staged, [helper]);
  writeFileSync(helper, 'new');
  assert.notEqual(stampRuntime(staged, [helper]), first);
});

test('a missing dist directory is rejected without stamping the manifest', (t) => {
  const { staged } = fixture(t);
  rmSync(path.join(staged, 'dist'), { recursive: true });
  assert.throws(() => stampRuntime(staged), /dist\/src\/internal\/bin\.js, dist\/src\/internal\/daemon\.js, .* are missing or empty.*pnpm build/);
  assert.deepEqual(manifest(staged), { name: 'silicon-extend-engine', version: '0.21.15' });
});

for (const entry of REQUIRED_ENTRIES) {
  test(`a runtime without ${entry} is rejected without stamping the manifest`, (t) => {
    const { staged } = fixture(t);
    rmSync(path.join(staged, entry));
    assert.throws(() => stampRuntime(staged), (error) => error.message.includes(`${entry} is missing or empty`));
    assert.deepEqual(manifest(staged), { name: 'silicon-extend-engine', version: '0.21.15' });
  });
}

test('an empty required entry (a truncated write) is rejected', (t) => {
  const { staged } = fixture(t);
  writeFileSync(path.join(staged, 'dist/src/internal/daemon.js'), '');
  assert.throws(() => stampRuntime(staged), /daemon\.js is missing or empty/);
  assert.equal(manifest(staged).extendRuntime, undefined);
});

for (const [chunk, importer] of [
  ['dist/src/main.js', 'static import'],
  ['dist/src/side.js', 'side-effect import'],
  ['dist/src/help.js', 'dynamic import'],
]) {
  test(`a partial dist missing a chunk loaded by ${importer} is rejected`, (t) => {
    const { staged } = fixture(t);
    rmSync(path.join(staged, chunk));
    assert.throws(() => stampRuntime(staged), (error) =>
      error.message.includes(`imports ../${path.basename(chunk)}`) && error.message.includes('build is incomplete'));
    assert.equal(manifest(staged).extendRuntime, undefined);
  });
}

test("a staged runtime behind Extend's entry stamps, and is rejected without the engine's own entry", (t) => {
  const { staged } = fixture(t);
  const bin = path.join(staged, 'bin');
  writeFileSync(path.join(bin, 'extend-engine-cli.mjs'), readFileSync(path.join(bin, 'extend-engine.mjs')));
  cpSync(path.join(here, 'runtime-entry.mjs'), path.join(bin, 'extend-engine.mjs'));
  assert.match(stampRuntime(staged), STAMPED_VERSION);
  rmSync(path.join(bin, 'extend-engine-cli.mjs'));
  const stamped = manifest(staged).version;
  assert.throws(() => stampRuntime(staged), (error) =>
    error.message.includes('bin/extend-engine.mjs imports ./extend-engine-cli.mjs') && error.message.includes('build is incomplete'));
  assert.equal(manifest(staged).version, stamped, 'a rejected runtime keeps its previous manifest');
});

test('a missing native helper is rejected', (t) => {
  const { root, staged } = fixture(t);
  assert.throws(() => stampRuntime(staged, [path.join(root, 'no-helper')]), /native helper .*no-helper is missing or empty/);
  assert.equal(manifest(staged).extendRuntime, undefined);
});

test('the CLI stamps when run through a symlinked checkout path', (t) => {
  const { root, staged } = fixture(t);
  const link = path.join(root, 'linked-desktop');
  symlinkSync(here, link, 'dir');
  const result = runCli(path.join(link, 'stamp-runtime.mjs'), [staged]);
  assert.equal(result.status, 0, result.stderr);
  const printed = result.stdout.trim();
  assert.match(printed, STAMPED_VERSION);
  assert.equal(manifest(staged).version, printed);
  assert.equal(manifest(staged).extendRuntime.upstreamVersion, '0.21.15');
});

test('the CLI fails loudly, leaving the manifest alone, when the runtime is incomplete', (t) => {
  const { staged } = fixture(t);
  rmSync(path.join(staged, 'dist/src/internal/daemon.js'));
  const result = runCli(path.join(here, 'stamp-runtime.mjs'), [staged]);
  assert.equal(result.status, 1);
  assert.equal(result.stdout, '');
  assert.match(result.stderr, /daemon\.js is missing or empty.*pnpm build/);
  assert.deepEqual(manifest(staged), { name: 'silicon-extend-engine', version: '0.21.15' });
});

test('the CLI without arguments prints usage and fails', () => {
  const result = runCli(path.join(here, 'stamp-runtime.mjs'), []);
  assert.equal(result.status, 2);
  assert.match(result.stderr, /Usage: stamp-runtime\.mjs/);
});

test('every internal entry the fork builds is required, since each is loaded by a computed path', () => {
  const config = readFileSync(path.join(vendored, 'tsdown.config.ts'), 'utf8');
  const entries = [...config.matchAll(/'internal\/([\w-]+)'\s*:/g)].map((m) => `dist/src/internal/${m[1]}.js`);
  assert.ok(entries.length >= 6, `found only ${entries.length} internal entries in tsdown.config.ts`);
  assert.deepEqual(entries.filter((entry) => !REQUIRED_ENTRIES.includes(entry)), []);
});

test('the built fork passes the completeness check', { skip: !existsSync(path.join(vendored, 'dist/src/internal/daemon.js')) && 'vendor/extend-engine is not built' }, (t) => {
  const staged = mkdtempSync(path.join(os.tmpdir(), 'extend-runtime-real-'));
  t.after(() => rmSync(staged, { recursive: true, force: true }));
  for (const name of ['bin', 'dist', 'package.json']) cpSync(path.join(vendored, name), path.join(staged, name), { recursive: true });
  assert.match(stampRuntime(staged), STAMPED_VERSION);
});
