import { expect, test } from 'vitest';
import { applyExtendEngineEnv } from './extend-env.ts';

test('an EXTEND_ENGINE_ setting is copied onto its AGENT_DEVICE_ name and wins', () => {
  const env: NodeJS.ProcessEnv = {
    EXTEND_ENGINE_STATE_DIR: '/new/state',
    AGENT_DEVICE_STATE_DIR: '/old/state',
    EXTEND_ENGINE_IOS_RUNNER_DETACH: '0',
    // An empty value is a setting too: it is copied, as the engine reads it.
    EXTEND_ENGINE_DAEMON_BASE_URL: '',
  };
  applyExtendEngineEnv(env);
  expect(env.AGENT_DEVICE_STATE_DIR).toBe('/new/state');
  expect(env.AGENT_DEVICE_IOS_RUNNER_DETACH).toBe('0');
  expect(env.AGENT_DEVICE_DAEMON_BASE_URL).toBe('');
  expect(env.EXTEND_ENGINE_STATE_DIR).toBe('/new/state');
});

test('an old AGENT_DEVICE_ name set alone keeps working', () => {
  const env: NodeJS.ProcessEnv = { AGENT_DEVICE_STATE_DIR: '/old/state', HOME: '/home/c' };
  applyExtendEngineEnv(env);
  expect(env).toEqual({ AGENT_DEVICE_STATE_DIR: '/old/state', HOME: '/home/c' });
});

test('only the EXTEND_ENGINE_ prefix is mapped', () => {
  const env: NodeJS.ProcessEnv = {
    // Extend's own locator for the engine, not a setting of the engine.
    EXTEND_ENGINE: '/opt/engine/bin/extend-engine.mjs',
    EXTEND_ENGINE_: 'nothing after the prefix',
    EXTEND_ENGINEX_STATE_DIR: '/elsewhere',
  };
  applyExtendEngineEnv(env);
  expect(Object.keys(env).filter((name) => name.startsWith('AGENT_DEVICE'))).toEqual([]);
});
