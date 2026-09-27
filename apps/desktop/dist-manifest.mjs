#!/usr/bin/env node
// Which source a built device engine (vendor/extend-engine/dist) was built from.
//
//   node apps/desktop/dist-manifest.mjs record <engine>   after `pnpm build`
//   node apps/desktop/dist-manifest.mjs check <engine>    before packaging a dist it can't rebuild
//
// `record` writes <engine>/.extend-build-manifest.json: the SHA-256 of every file the build
// reads (src, packages, the manifests and build configs) and of the dist it produced. `check`
// compares the tree with it and names what changed since: an edited file, a new one, and a
// deleted one, which a "newer than the dist" test can't see. Content hashes, not timestamps, so a
// copy of the tree (the linux-e2e container's) checks the same as the original. Packaging a stale
// dist would ship old behaviour under a valid build identity (stamp-runtime.mjs).
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, readFileSync, readdirSync, realpathSync, renameSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const MANIFEST = '.extend-build-manifest.json';
// What `pnpm build` (tsdown) reads, as build-package.sh has always counted it.
const INPUT_DIRECTORIES = ['src', 'packages'];
const INPUT_FILES = /^(package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|tsdown\.config\.ts|tsconfig[^/]*\.json)$/;
// Never inputs: installed dependencies and what builds and type checks write.
const PRUNED = new Set(['node_modules', 'dist', 'dist-types', '.build', '.swiftpm', '.tmp', 'coverage']);
const REBUILD = 'Rebuild it where pnpm is: (cd vendor/extend-engine && pnpm install --frozen-lockfile && pnpm build) && node apps/desktop/dist-manifest.mjs record vendor/extend-engine, then package again.';

function sha256(bytes) {
  return createHash('sha256').update(bytes).digest('hex');
}

function walk(root, relative, out) {
  const entry = path.join(root, relative);
  const stat = lstatSync(entry);
  if (stat.isDirectory()) {
    for (const name of readdirSync(entry).sort()) {
      if (!PRUNED.has(name)) walk(root, relative ? `${relative}/${name}` : name, out);
    }
  } else if (stat.isFile()) {
    out[relative] = sha256(readFileSync(entry));
  }
  // Symlinks (workspace links) point at inputs that are hashed where they live.
}

/** The build's inputs: relative path → SHA-256. */
export function sources(root) {
  const out = {};
  for (const name of readdirSync(root).sort()) {
    if (INPUT_FILES.test(name) && lstatSync(path.join(root, name)).isFile()) out[name] = sha256(readFileSync(path.join(root, name)));
  }
  for (const directory of INPUT_DIRECTORIES) if (existsSync(path.join(root, directory))) walk(root, directory, out);
  return out;
}

/** One SHA-256 over every file of the built dist, with its path. */
export function distDigest(root) {
  const files = {};
  if (existsSync(path.join(root, 'dist'))) walk(root, 'dist', files);
  const hash = createHash('sha256');
  for (const [file, digest] of Object.entries(files)) hash.update(file).update('\0').update(digest).update('\n');
  return { digest: hash.digest('hex'), count: Object.keys(files).length };
}

export function record(root) {
  const dist = distDigest(root);
  if (dist.count === 0) {
    throw new Error(`Can't record what ${path.join(root, 'dist')} was built from: it has no files, so the build didn't run or failed. ${REBUILD}`);
  }
  const manifest = { version: 1, dist: dist.digest, sources: sources(root) };
  const file = path.join(root, MANIFEST);
  const temporary = `${file}.${process.pid}.tmp`;
  writeFileSync(temporary, `${JSON.stringify(manifest)}\n`);
  renameSync(temporary, file);
  return manifest;
}

function listed(kind, files) {
  if (!files.length) return [];
  const shown = [...files].sort().slice(0, 5).join(', ');
  return [`${kind} ${shown}${files.length > 5 ? ` and ${files.length - 5} more` : ''}`];
}

/** `{ fresh: true }`, or `{ fresh: false, reason }` saying what changed and what to do. */
export function check(root) {
  const where = path.join(root, 'dist');
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(path.join(root, MANIFEST), 'utf8'));
  } catch {
    return { fresh: false, reason: `${where} has no record of the source it was built from (${MANIFEST} is missing or unreadable), so it can't be checked against the current code. ${REBUILD}` };
  }
  if (manifest?.version !== 1 || typeof manifest.dist !== 'string' || typeof manifest.sources !== 'object') {
    return { fresh: false, reason: `${path.join(root, MANIFEST)} isn't a build record this version understands. ${REBUILD}` };
  }
  if (distDigest(root).digest !== manifest.dist) {
    return { fresh: false, reason: `${where} changed since its source was recorded (it was rebuilt or edited without recording), so which code it holds is unknown. ${REBUILD}` };
  }
  const now = sources(root);
  const changed = [], added = [], deleted = [];
  for (const [file, digest] of Object.entries(manifest.sources)) {
    if (!(file in now)) deleted.push(file);
    else if (now[file] !== digest) changed.push(file);
  }
  for (const file of Object.keys(now)) if (!(file in manifest.sources)) added.push(file);
  const differences = [...listed('changed:', changed), ...listed('added:', added), ...listed('deleted:', deleted)];
  if (!differences.length) return { fresh: true };
  return { fresh: false, reason: `${where} was built from other source than vendor/extend-engine has now (${differences.join('; ')}), so it may not contain the current code. ${REBUILD}` };
}

function invokedAsScript() {
  if (!process.argv[1]) return false;
  try {
    return realpathSync(process.argv[1]) === realpathSync(fileURLToPath(import.meta.url));
  } catch {
    return false;
  }
}

if (invokedAsScript()) {
  const [command, root] = process.argv.slice(2);
  if (!['record', 'check'].includes(command) || !root) {
    console.error('Usage: dist-manifest.mjs record|check <engine directory>');
    process.exit(2);
  }
  try {
    if (command === 'record') {
      const manifest = record(root);
      console.log(`Recorded ${Object.keys(manifest.sources).length} source files for ${path.join(root, 'dist')}.`);
    } else {
      const result = check(root);
      if (!result.fresh) {
        console.error(result.reason);
        process.exit(1);
      }
      console.log(`${path.join(root, 'dist')} matches the current source.`);
    }
  } catch (error) {
    console.error(error instanceof Error ? error.message : String(error));
    process.exit(1);
  }
}
