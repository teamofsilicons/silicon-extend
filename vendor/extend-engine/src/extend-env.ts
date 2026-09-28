// Silicon Extend names every setting of this engine EXTEND_ENGINE_<X>; the engine's own code reads
// the fork's AGENT_DEVICE_<X>. Every entry point (bin/extend-engine.mjs, src/bin.ts, src/daemon.ts
// and Extend's runtime entry, apps/desktop/runtime-entry.mjs) imports this module before anything
// else, so each set EXTEND_ENGINE_<X> is copied onto AGENT_DEVICE_<X> before any code reads the
// environment: the new name wins, and a setting made only under the old name still works. The
// daemon and the workers the engine starts inherit the result. Nothing in the engine reads a
// setting while its modules load (only when it runs), so the order the bundler gives this import
// among a chunk's imports doesn't matter.
//
// Built as its own entry (dist/src/internal/extend-env.js, tsdown.config.ts) so the runtime entry,
// which is plain JavaScript outside this package, can import it too.

export const EXTEND_ENGINE_ENV_PREFIX = 'EXTEND_ENGINE_';
export const ENGINE_ENV_PREFIX = 'AGENT_DEVICE_';

/** Copies every set `EXTEND_ENGINE_<X>` in `env` onto `AGENT_DEVICE_<X>`, overwriting it. */
export function applyExtendEngineEnv(env: NodeJS.ProcessEnv = process.env): void {
  for (const [name, value] of Object.entries(env)) {
    if (value === undefined || !name.startsWith(EXTEND_ENGINE_ENV_PREFIX)) continue;
    const setting = name.slice(EXTEND_ENGINE_ENV_PREFIX.length);
    if (setting) env[`${ENGINE_ENV_PREFIX}${setting}`] = value;
  }
}

applyExtendEngineEnv();
