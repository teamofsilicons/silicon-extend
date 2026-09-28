import { expect, test, vi } from 'vitest';
import { bindLocalFocusInteractor } from '@agent-device/contracts/focus-runtime';
import type { Interactor, RunnerContext } from '@agent-device/contracts/interactor-types';
import type { BoundDeviceRuntime } from '@agent-device/contracts/platform-runtime';
import { fillPointUse } from '@agent-device/contracts/platform-runtime-operations';
import {
  bindLocalTouchInteractor,
  touchRuntimeOperationFacts,
} from '@agent-device/contracts/touch-runtime';
import { bindLocalTypeTextInteractor } from '@agent-device/contracts/type-text-runtime';
import type { DaemonCommandContext } from '../context.ts';
import { executeFocusPoint } from '../focus-runtime.ts';
import { runtimeExecutionFromContext } from '../snapshot-runtime-capture-input.ts';
import { createBoundTouchExecutor } from '../touch-runtime.ts';
import { executeBoundTypeText } from '../type-text-runtime.ts';

const mac = {
  id: 'host-mac',
  name: 'Host Mac',
  platform: 'apple',
  appleOs: 'macos',
  kind: 'device',
  target: 'desktop',
  booted: true,
} as const;
const available = Object.freeze({ available: true } as const);

/**
 * A macOS frontmost-app session records the app that was frontmost when it opened as its bundle.
 * Text entry has to reach the owner with the session surface too, or the helper types into that
 * stale app after the Carbon or the Silicon has moved to another one.
 */
function frontmostAppContext(): DaemonCommandContext {
  return { appBundleId: 'com.tinyspeck.slackmacgap', surface: 'frontmost-app' };
}

function recordingInteractor() {
  const contexts: RunnerContext[] = [];
  const interactor = {
    focus: vi.fn(async () => undefined),
    type: vi.fn(async () => undefined),
    fill: vi.fn(async () => undefined),
  } as unknown as Interactor;
  const resolveInteractor = vi.fn(async (_device: unknown, runner: RunnerContext) => {
    contexts.push(runner);
    return interactor;
  });
  return { contexts, resolveInteractor };
}

test('the runner execution carries the session surface only when the session has one', () => {
  expect(runtimeExecutionFromContext(frontmostAppContext()).surface).toBe('frontmost-app');
  expect(Object.hasOwn(runtimeExecutionFromContext({}), 'surface')).toBe(false);
});

test('type, focus and fill on a frontmost-app session reach the owner with that surface', async () => {
  const { contexts, resolveInteractor } = recordingInteractor();
  const signal = new AbortController().signal;
  const typeText = bindLocalTypeTextInteractor({ device: mac, signal, resolveInteractor });
  await executeBoundTypeText(
    { operations: typeText },
    ['https://example.com'],
    frontmostAppContext(),
  );

  const focus = bindLocalFocusInteractor({ device: mac, signal, resolveInteractor });
  await executeFocusPoint({ operations: focus }, { x: 10, y: 20 }, frontmostAppContext());

  const touch = bindLocalTouchInteractor({
    device: mac,
    signal,
    resolveInteractor,
    facts: touchRuntimeOperationFacts({
      tap: available,
      longPress: available,
      fill: available,
      unsupported: { available: false, reason: 'unsupported-platform-leaf', hint: 'unused' },
    }),
    pause: async () => {},
  });
  const executor = createBoundTouchExecutor(
    {
      kind: 'fill',
      captured: false,
      runtime: { operations: touch } as unknown as BoundDeviceRuntime<typeof fillPointUse>,
    },
    frontmostAppContext(),
  );
  await executor.fillPoint?.({ x: 10, y: 20 }, 'value');

  expect(contexts).toHaveLength(3);
  for (const runner of contexts) {
    expect(runner).toMatchObject({
      surface: 'frontmost-app',
      appBundleId: 'com.tinyspeck.slackmacgap',
    });
  }
});
