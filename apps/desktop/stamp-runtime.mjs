#!/usr/bin/env node
// Stamp the staged fork, never the source checkout. Installed daemon reuse compares version
// strings, so each distinct Extend runtime needs its own version even between upstream releases.
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, readFileSync, readdirSync, renameSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

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

export function stampRuntime(root, nativeFiles = []) {
  const manifestPath = path.join(root, 'package.json');
  const manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  if (manifest.name !== 'agent-device' || typeof manifest.version !== 'string') {
    throw new Error('Expected the staged agent-device package');
  }
  const previous = manifest.extendRuntime;
  const base = previous && manifest.version === buildVersion(previous.upstreamVersion, previous.sha256)
    ? previous.upstreamVersion : manifest.version;
  delete manifest.extendRuntime;
  manifest.version = base;
  const hash = createHash('sha256');
  function add(label, bytes) {
    hash.update(label).update('\0').update(String(bytes.length)).update('\0').update(bytes);
  }
  function walk(relative) {
    const entry = path.join(root, relative);
    const stat = lstatSync(entry);
    if (stat.isDirectory()) {
      for (const name of readdirSync(entry).sort()) walk(`${relative}/${name}`);
    } else if (stat.isFile()) {
      add(relative, readFileSync(entry));
    } else {
      throw new Error(`Unsupported packaged runtime entry: ${relative}`);
    }
  }
  add('runtime', Buffer.from(`${process.platform}/${process.arch}/${process.version}`));
  add('package.json', Buffer.from(JSON.stringify(canonical(manifest))));
  // Required entry points first; missing/incomplete builds must not acquire a valid identity.
  for (const directory of ['bin', 'dist']) walk(directory);
  for (const directory of ['apple', 'linux']) if (existsSync(path.join(root, directory))) walk(directory);
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

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [root, ...nativeFiles] = process.argv.slice(2);
  if (!root) throw new Error('Usage: stamp-runtime.mjs <staged-agent-device> [native-helper ...]');
  console.log(stampRuntime(root, nativeFiles));
}
