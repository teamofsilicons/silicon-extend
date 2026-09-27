import { test } from 'vitest';
import assert from 'node:assert/strict';
import { runCliCapture } from './cli-capture.ts';

/**
 * The human `doctor` run, end to end over the CLI's own progress wiring: the
 * transport hands `command` progress to the sink the CLI installed, which
 * renders it to stderr and records that it did, and the output formatter then
 * prints the summary alone rather than repeating every check the reader has
 * already seen. Without progress it prints the checks.
 */

const DOCTOR_RESULT = {
  status: 'pass',
  summary: 'No blockers found.',
  checks: [
    {
      id: 'engine',
      status: 'pass',
      summary: 'Silicon Extend device engine 0.17.9 using /tmp/extend-engine',
    },
    { id: 'device', status: 'warn', summary: 'No booted device.' },
  ],
};

test('doctor prints only the summary when progress already streamed the checks', async () => {
  const result = await runCliCapture(['doctor'], async (_req, options) => {
    options?.onProgress?.({
      type: 'command',
      status: 'progress',
      message: '✓ engine: Silicon Extend device engine 0.17.9 using /tmp/extend-engine',
    });
    options?.onProgress?.({
      type: 'command',
      status: 'progress',
      message: '! device: No booted device.',
    });
    return { ok: true, data: DOCTOR_RESULT };
  });

  assert.equal(result.code, null);
  assert.match(
    result.stderr,
    /✓ engine: Silicon Extend device engine 0\.17\.9 using \/tmp\/extend-engine\n! device: No booted device\.\n/,
  );
  assert.match(result.stdout, /Doctor: pass\nNo blockers found\.\n/);
  assert.doesNotMatch(result.stdout, /✓ engine:/);
});

test('doctor prints the checks when no progress reached stderr', async () => {
  const result = await runCliCapture(['doctor'], async () => ({
    ok: true,
    data: DOCTOR_RESULT,
  }));

  assert.equal(result.code, null);
  assert.equal(result.stderr, '');
  assert.match(
    result.stdout,
    /✓ engine: Silicon Extend device engine 0\.17\.9 using \/tmp\/extend-engine/,
  );
  assert.match(result.stdout, /! device: No booted device\./);
});
