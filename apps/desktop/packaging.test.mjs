// Checks for the packaging steps shared by macos/build-app.sh and linux/build-package.sh:
// packaging.sh (the stamp's error path, the dist freshness check) and dist-manifest.mjs.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { chmodSync, cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, utimesSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { MANIFEST, check, record } from './dist-manifest.mjs';

const here = path.dirname(fileURLToPath(import.meta.url));

function scratch(t) {
  const dir = mkdtempSync(path.join(os.tmpdir(), 'extend-packaging-'));
  t.after(() => rmSync(dir, { recursive: true, force: true }));
  return dir;
}

// A small device engine tree: sources the build reads, what it doesn't read, and a built dist.
function fork(t) {
  const root = path.join(scratch(t), 'extend-engine');
  const write = (file, text) => {
    mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
    writeFileSync(path.join(root, file), text);
  };
  write('package.json', '{"name":"silicon-extend-engine","version":"0.21.15"}');
  write('tsdown.config.ts', 'export default {};');
  write('tsconfig.lib.json', '{}');
  write('src/bin.ts', 'export const a = 1;');
  write('src/old-helper.ts', 'export const helper = 1;');
  write('packages/kit/src/index.ts', 'export const kit = 1;');
  write('packages/kit/node_modules/dep/index.js', 'installed');
  write('packages/kit/dist-types/index.d.ts', 'generated');
  write('README.md', 'not read by the build');
  write('dist/src/internal/bin.js', 'built();');
  return { root, write };
}

test('a dist is fresh right after its source is recorded, and in a copy with other timestamps', (t) => {
  const { root } = fork(t);
  record(root);
  assert.deepEqual(check(root), { fresh: true });
  const copy = path.join(path.dirname(root), 'copy');
  cpSync(root, copy, { recursive: true });
  utimesSync(path.join(copy, 'src/bin.ts'), new Date(0), new Date(0));
  utimesSync(path.join(copy, 'dist/src/internal/bin.js'), new Date(0), new Date(0));
  assert.deepEqual(check(copy), { fresh: true }, 'timestamps are not what is compared');
});

test('a deleted source file makes the dist stale, and is named', (t) => {
  const { root } = fork(t);
  record(root);
  rmSync(path.join(root, 'src/old-helper.ts'));
  const result = check(root);
  assert.equal(result.fresh, false);
  assert.match(result.reason, /deleted: src\/old-helper\.ts/);
  assert.match(result.reason, /pnpm build.*dist-manifest\.mjs record/);
});

test('an edited or added source file makes the dist stale, and is named', (t) => {
  const { root, write } = fork(t);
  record(root);
  write('packages/kit/src/index.ts', 'export const kit = 2;');
  write('src/new.ts', 'export {};');
  write('tsconfig.json', '{}');
  const { fresh, reason } = check(root);
  assert.equal(fresh, false);
  assert.match(reason, /changed: packages\/kit\/src\/index\.ts/);
  assert.match(reason, /added: src\/new\.ts, tsconfig\.json/);
});

test('what the build does not read never makes it stale', (t) => {
  const { root, write } = fork(t);
  record(root);
  write('README.md', 'edited');
  write('packages/kit/node_modules/dep/index.js', 'reinstalled');
  write('packages/kit/dist-types/index.d.ts', 'regenerated');
  assert.deepEqual(check(root), { fresh: true });
});

test('a dist rebuilt without recording, or never recorded, is refused', (t) => {
  const { root, write } = fork(t);
  assert.match(check(root).reason, new RegExp(`no record of the source it was built from \\(${MANIFEST.replace('.', '\\.')}`));
  record(root);
  write('dist/src/internal/bin.js', 'rebuilt();');
  assert.match(check(root).reason, /dist changed since its source was recorded/);
  rmSync(path.join(root, 'dist'), { recursive: true });
  assert.throws(() => record(root), /has no files, so the build didn't run or failed/);
});

test('the CLI records and checks, failing with the reason', (t) => {
  const { root } = fork(t);
  const cli = (...args) => spawnSync(process.execPath, [path.join(here, 'dist-manifest.mjs'), ...args], { encoding: 'utf8' });
  assert.equal(cli('record', root).status, 0);
  assert.equal(cli('check', root).status, 0);
  rmSync(path.join(root, 'src/old-helper.ts'));
  const stale = cli('check', root);
  assert.equal(stale.status, 1);
  assert.match(stale.stderr, /deleted: src\/old-helper\.ts/);
  assert.equal(cli('nope').status, 2);
});

// Runs `body` in bash with packaging.sh sourced and build-app.sh's die().
function inBash(body, env = {}) {
  const script = `set -euo pipefail\ndie() { printf '%s\\n' "$*" >&2; exit 1; }\nsource "${path.join(here, 'packaging.sh')}"\n${body}`;
  return spawnSync('bash', ['-c', script], { encoding: 'utf8', env: { ...process.env, ...env } });
}

function fakeNode(t, body) {
  const file = path.join(scratch(t), 'node');
  writeFileSync(file, `#!/bin/sh\n${body}\n`);
  chmodSync(file, 0o755);
  return file;
}

test('a stamp killed by a signal stops the build saying so', (t) => {
  const node = fakeNode(t, 'kill -9 $$');
  const result = inBash(`stamp_runtime "${node}" /staged/engine; echo "packaged anyway: $STAMPED"`);
  assert.equal(result.status, 1);
  assert.doesNotMatch(result.stdout, /packaged anyway/);
  assert.match(result.stderr, /Stamping the device engine runtime in \/staged\/engine with its build identity failed: it was killed by signal 9 \(SIGKILL\)/);
  assert.match(result.stderr, /nothing was packaged\. Fix the cause and package again\./);
});

test('a stamp that fails stops the build, pointing at its own reason', (t) => {
  const node = fakeNode(t, "echo \"Can't stamp: dist/src/internal/png-worker.js is missing\" >&2; exit 1");
  const result = inBash(`stamp_runtime "${node}" /staged/engine`);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /png-worker\.js is missing/);
  assert.match(result.stderr, /it exited with status 1; its reason is above/);
});

test('a stamp that works sets STAMPED, passing the runtime and helpers on', (t) => {
  const node = fakeNode(t, 'shift; echo "0.21.15+extend.$(printf %064d 0) for $*"');
  const result = inBash(`stamp_runtime "${node}" /rt /helper; printf '%s' "$STAMPED"`);
  assert.equal(result.status, 0, result.stderr);
  assert.equal(result.stdout, `0.21.15+extend.${'0'.repeat(64)} for /rt /helper`);
});

test('require_fresh_dist stops the build with what changed', (t) => {
  const { root } = fork(t);
  record(root);
  assert.equal(inBash(`require_fresh_dist "${root}"`).status, 0);
  rmSync(path.join(root, 'src/old-helper.ts'));
  const result = inBash(`require_fresh_dist "${root}"`);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /deleted: src\/old-helper\.ts/);
});

test('both packaging scripts stamp through the checked step and never read the dist by age', () => {
  for (const script of ['macos/build-app.sh', 'linux/build-package.sh']) {
    const text = readFileSync(path.join(here, script), 'utf8');
    assert.match(text, /source "\$ROOT\/apps\/desktop\/packaging\.sh"/, script);
    assert.match(text, /^stamp_runtime "\$BUNDLED_NODE" "\$RUNTIME"/m, script);
    assert.doesNotMatch(text, /^STAMPED="\$\(/m, `${script} stamps without an error path`);
  }
  const linux = readFileSync(path.join(here, 'linux/build-package.sh'), 'utf8');
  assert.match(linux, /require_fresh_dist "\$ENGINE"/);
  assert.doesNotMatch(linux, /-newer/);
  // The X11 recorder loads these through ctypes, so dpkg-shlibdeps can't find them.
  assert.match(linux, /^Recommends: .*\blibxcomposite1, libxdamage1, libxfixes3\b/m);
  assert.match(linux, /libxcomposite1, libxdamage1 and libxfixes3/);
});
