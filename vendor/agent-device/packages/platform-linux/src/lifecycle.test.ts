import { expect, test } from 'vitest';
import { bindLinuxApplicationLifecycle } from './lifecycle.ts';

const lifecycle = bindLinuxApplicationLifecycle({
  device: {
    platform: 'linux',
    id: 'linux',
    name: 'Linux',
    kind: 'device',
    target: 'desktop',
    booted: true,
  },
  host: {
    resolve: async () => {
      throw new Error('identity resolution must not launch an application');
    },
  },
  signal: new AbortController().signal,
});

test('a named Linux app becomes the bound recording identity', async () => {
  expect(
    await lifecycle.resolveOpenTarget({ target: 'org.example.Editor', surface: 'app' }),
  ).toEqual({ appName: 'org.example.Editor', appBundleId: 'org.example.Editor' });
});

test.each(['https://example.test', 'mailto:person@example.test', undefined])(
  'does not invent an app identity for %s',
  async (target) => {
    expect(
      (await lifecycle.resolveOpenTarget({ target, surface: 'app' })).appBundleId,
    ).toBeUndefined();
  },
);
