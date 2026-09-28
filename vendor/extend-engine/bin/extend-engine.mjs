#!/usr/bin/env node
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const distPaths = [
  join(here, '..', 'dist', 'src', 'internal', 'bin.js'),
  join(here, '..', 'dist', 'src', 'bin.js'),
];
const distPath = distPaths.find((candidate) => existsSync(candidate));

if (!distPath) {
  process.stderr.write('Missing dist build. Run `pnpm build` before using the binary.\n');
  process.exit(1);
}

// EXTEND_ENGINE_* settings first (src/extend-env.ts); internal/bin.js imports it too.
const extendEnv = join(here, '..', 'dist', 'src', 'internal', 'extend-env.js');
if (existsSync(extendEnv)) await import(pathToFileURL(extendEnv).href);

await import(pathToFileURL(distPath).href);
