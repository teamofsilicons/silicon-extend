import { beforeEach, expect, test, vi } from 'vitest';
import type { DaemonResponse } from '../../../daemon-request.ts';
import type { SessionState } from '../../../session-state.ts';
import { makeSessionStore } from '../../../../__tests__/test-utils/store-factory.ts';
import { makeMacOsSession } from '../../../../__tests__/test-utils/session-factories.ts';
import {
  mockFocusPoint,
  mockTypeText,
  resetGetRuntimeFixture,
} from '../../../__tests__/interaction-get-runtime-fixture.ts';
import { legacyDispatchCapture } from '../../../__tests__/legacy-snapshot-capture-fixture.ts';
import { invokeFindHandler } from './find-handler-fixture.ts';

vi.mock('../../../snapshot-interactor-capture.ts', async () => {
  const fixture = await import('../../../__tests__/legacy-snapshot-capture-fixture.ts');
  return { captureSnapshotWithInteractor: fixture.captureSnapshotThroughLegacyDispatchFixture };
});

beforeEach(() => {
  resetGetRuntimeFixture();
  legacyDispatchCapture.mockReset();
});

const ADDRESS_FIELD = {
  index: 0,
  ref: 'e1',
  type: 'TextField',
  label: 'Address',
  hittable: true,
  rect: { x: 10, y: 20, width: 400, height: 30 },
};

/**
 * A macOS `frontmost-app` session records the app that was frontmost when it opened as its bundle
 * (Slack here). The Silicon has since moved to another app, so find's own focus and type legs must
 * tell the owner the session follows the frontmost app, or the helper activates Slack and types
 * the text there.
 */
async function runFindOnFrontmostAppSession(
  positionals: string[],
  overrides: Partial<SessionState> = { surface: 'frontmost-app' },
) {
  const sessionStore = makeSessionStore();
  const sessionName = 'default';
  sessionStore.set(
    sessionName,
    makeMacOsSession(sessionName, { appBundleId: 'com.tinyspeck.slackmacgap', ...overrides }),
  );
  legacyDispatchCapture.mockImplementation(async (_device, command) =>
    command === 'snapshot' ? { nodes: [ADDRESS_FIELD] } : {},
  );
  return await invokeFindHandler({
    sessionName,
    sessionStore,
    positionals,
    invoke: async () => ({ ok: true, data: {} }) as DaemonResponse,
  });
}

test('find type on a frontmost-app session sends the surface with both of its legs', async () => {
  const response = await runFindOnFrontmostAppSession(['Address', 'type', 'https://example.com']);

  expect(response?.ok).toBe(true);
  expect(mockFocusPoint).toHaveBeenCalledTimes(1);
  expect(mockFocusPoint.mock.calls[0]?.[0].execution?.surface).toBe('frontmost-app');
  expect(mockTypeText).toHaveBeenCalledTimes(1);
  expect(mockTypeText.mock.calls[0]?.[0]).toMatchObject({
    text: 'https://example.com',
    execution: { surface: 'frontmost-app' },
  });
});

test('find focus on a frontmost-app session sends the surface', async () => {
  const response = await runFindOnFrontmostAppSession(['Address', 'focus']);

  expect(response?.ok).toBe(true);
  expect(mockFocusPoint).toHaveBeenCalledTimes(1);
  expect(mockFocusPoint.mock.calls[0]?.[0]).toMatchObject({
    point: { x: 210, y: 35 },
    execution: { surface: 'frontmost-app' },
  });
});

test('find type on an app session keeps its surface and bundle', async () => {
  const response = await runFindOnFrontmostAppSession(['Address', 'type', 'hello'], {
    surface: 'app',
  });

  expect(response?.ok).toBe(true);
  expect(mockTypeText.mock.calls[0]?.[0]).toMatchObject({
    options: { appBundleId: 'com.tinyspeck.slackmacgap' },
    execution: { surface: 'app' },
  });
});

test('find type on a session without a surface sends none', async () => {
  const response = await runFindOnFrontmostAppSession(['Address', 'type', 'hello'], {});

  expect(response?.ok).toBe(true);
  expect(Object.hasOwn(mockTypeText.mock.calls[0]?.[0].execution ?? {}, 'surface')).toBe(false);
});
