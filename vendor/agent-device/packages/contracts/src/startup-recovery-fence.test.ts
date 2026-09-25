import { afterEach, expect, test } from 'vitest';
import {
  resetStartupRecoveryFencesForTests,
  runStartupRecoveryFence,
  waitForStartupRecoveryFence,
} from './startup-recovery-fence.ts';

afterEach(() => {
  resetStartupRecoveryFencesForTests();
});

test('planted race: registers the pre-mutation fence before lazy recovery work awaits', async () => {
  let releaseRecovery!: () => void;
  const recovery = runStartupRecoveryFence('/state', async () => {
    await new Promise<void>((resolve) => {
      releaseRecovery = resolve;
    });
  });
  let mutationReleased = false;
  const mutation = waitForStartupRecoveryFence('/state').then(() => {
    mutationReleased = true;
  });

  await Promise.resolve();
  expect(mutationReleased).toBe(false);

  releaseRecovery();
  await recovery;
  await mutation;
  expect(mutationReleased).toBe(true);
});
