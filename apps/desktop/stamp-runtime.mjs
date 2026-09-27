#!/usr/bin/env node
// Stamp the staged fork, never the source checkout. Installed daemon reuse compares version
// strings, so each distinct Extend runtime needs its own version even between upstream releases.
//
// The identity covers content only, not the install path, so timestamps and signing don't count.
// Where a daemon was started from is runtime-entry.mjs's job: packaging installs it as the runtime's
// bin/agent-device.mjs, and it replaces a daemon that an identical copy at another location started.
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, readFileSync, readdirSync, realpathSync, renameSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

// The packaged agent-device can't start without these: the CLI wrapper, its entry and the daemon,
// and the entries it loads by a computed path, which the import check below can't follow (every
// `internal/*` entry in vendor/agent-device/tsdown.config.ts): the PNG worker thread (screenshots),
// the Metro companion tunnel, the Maestro runScript HTTP child and the update check.
export const REQUIRED_ENTRIES = Object.freeze([
  'bin/agent-device.mjs',
  'dist/src/internal/bin.js',
  'dist/src/internal/daemon.js',
  'dist/src/internal/png-worker.js',
  'dist/src/internal/companion-tunnel.js',
  'dist/src/internal/run-script-http-child.js',
  'dist/src/internal/update-check-entry.js',
]);

// What a stamped version looks like: `0.21.15+extend.<sha256>` or `1.0.0+build.2.extend.<sha256>`.
export const STAMPED_VERSION = /^\S+[+.]extend\.[0-9a-f]{64}$/;

// Relative module specifiers in built JavaScript: static `from '…'`, side-effect `import '…'` and
// dynamic `import('…')`, in single, double or back quotes without interpolation.
const RELATIVE_IMPORT = /(?:\bfrom\s*|\bimport\s*\(\s*|\bimport\s*)(["'`])(\.{1,2}\/[^"'`$\n]+?)\1/g;
const REBUILD = 'Rebuild the fork (cd vendor/agent-device && pnpm install --frozen-lockfile && pnpm build) and package again.';

function canonical(value) {
  if (Array.isArray(value)) return value.map(canonical);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.keys(value).sort().map((key) => [key, canonical(value[key])]));
  }
  return value;
}

function buildVersion(base, digest) {
  return `${base}${base.includes('+') ? '.' : '+'}extend.${digest}`;
}

function isFileWithContent(file) {
  try {
    const stat = lstatSync(file);
    return stat.isFile() && stat.size > 0;
  } catch {
    return false;
  }
}

export function stampRuntime(root, nativeFiles = []) {
  const manifestPath = path.join(root, 'package.json');
  if (!existsSync(manifestPath)) {
    throw new Error(`Can't stamp ${root}: it has no package.json, so it isn't a staged agent-device. Pass the staged agent-device directory.`);
  }
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  } catch (error) {
    throw new Error(`Can't stamp ${root}: its package.json isn't valid JSON (${error.message}), so the staging is damaged. ${REBUILD}`);
  }
  if (manifest?.name !== 'agent-device' || typeof manifest.version !== 'string') {
    throw new Error(`Can't stamp ${root}: its package.json is not agent-device's (name ${JSON.stringify(manifest?.name)}), so there is nothing to stamp. Pass the staged agent-device directory.`);
  }
  // Missing or partial builds must not acquire a valid identity, so nothing is written until the
  // entry points, native helpers and every relative import in bin/ and dist/ check out.
  const missingEntries = REQUIRED_ENTRIES.filter((entry) => !isFileWithContent(path.join(root, entry)));
  if (missingEntries.length) {
    throw new Error(`Can't stamp ${root}: ${missingEntries.join(', ')} ${missingEntries.length === 1 ? 'is' : 'are'} missing or empty, so this agent-device build is incomplete and Silicon Extend couldn't start it. ${REBUILD}`);
  }
  for (const file of nativeFiles) {
    if (!isFileWithContent(file)) {
      throw new Error(`Can't stamp ${root}: the native helper ${file} is missing or empty, so the app would ship without it. Build the helper and package again.`);
    }
  }
  const previous = manifest.extendRuntime;
  const base = previous && manifest.version === buildVersion(previous.upstreamVersion, previous.sha256)
    ? previous.upstreamVersion : manifest.version;
  delete manifest.extendRuntime;
  manifest.version = base;
  const hash = createHash('sha256');
  const unresolved = [];
  function add(label, bytes) {
    hash.update(label).update('\0').update(String(bytes.length)).update('\0').update(bytes);
  }
  function checkImports(relative, entry, bytes) {
    if (!/\.m?js$/.test(relative)) return;
    for (const [, , specifier] of bytes.toString('utf8').matchAll(RELATIVE_IMPORT)) {
      if (!existsSync(path.resolve(path.dirname(entry), specifier))) unresolved.push(`${relative} imports ${specifier}`);
    }
  }
  function walk(relative, code) {
    const entry = path.join(root, relative);
    const stat = lstatSync(entry);
    if (stat.isDirectory()) {
      for (const name of readdirSync(entry).sort()) walk(`${relative}/${name}`, code);
    } else if (stat.isFile()) {
      const bytes = readFileSync(entry);
      add(relative, bytes);
      if (code) checkImports(relative, entry, bytes);
    } else {
      throw new Error(`Can't stamp ${root}: ${relative} is a symlink or special file, and the runtime's identity is computed from regular files only. Replace it with a regular file and package again.`);
    }
  }
  add('runtime', Buffer.from(`${process.platform}/${process.arch}/${process.version}`));
  add('package.json', Buffer.from(JSON.stringify(canonical(manifest))));
  for (const directory of ['bin', 'dist']) walk(directory, true);
  for (const directory of ['apple', 'linux']) if (existsSync(path.join(root, directory))) walk(directory, false);
  if (unresolved.length) {
    const shown = unresolved.slice(0, 5).join('; ');
    const more = unresolved.length > 5 ? ` (and ${unresolved.length - 5} more)` : '';
    throw new Error(`Can't stamp ${root}: ${shown}${more}, which ${unresolved.length === 1 ? "isn't" : "aren't"} in the staged runtime, so the build is incomplete (an interrupted or failed build). ${REBUILD}`);
  }
  for (const file of [...nativeFiles].sort((a, b) => path.basename(a).localeCompare(path.basename(b)))) {
    add(`native/${path.basename(file)}`, readFileSync(file));
  }
  const digest = hash.digest('hex');
  manifest.version = buildVersion(base, digest);
  manifest.extendRuntime = { upstreamVersion: base, sha256: digest };
  const temporary = `${manifestPath}.${process.pid}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(manifest, null, 2)}\n`);
  renameSync(temporary, manifestPath);
  return manifest.version;
}

// Compare real paths: Node resolves symlinks in the main module's URL but not in argv[1], so a
// build run through a symlinked checkout (macOS /tmp, /var) would otherwise skip stamping.
function invokedAsScript() {
  if (!process.argv[1]) return false;
  try {
    return realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (invokedAsScript()) {
  const [root, ...nativeFiles] = process.argv.slice(2);
  if (!root) {
    console.error('Usage: stamp-runtime.mjs <staged-agent-device> [native-helper ...]');
    process.exit(2);
  }
  try {
    console.log(stampRuntime(root, nativeFiles));
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
