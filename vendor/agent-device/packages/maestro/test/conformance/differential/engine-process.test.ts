import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import {
  classifyAgentDeviceFailure,
  resolveAgentDeviceCliArgv,
  runAgentDeviceEngine,
} from './engine-process.ts';

test('agent-device JSON distinguishes infrastructure from behavioral failures', () => {
  const result = (infrastructure?: true) =>
    JSON.stringify({
      success: true,
      data: {
        failures: [
          {
            status: 'failed',
            ...(infrastructure ? { infrastructure } : {}),
          },
        ],
      },
    });

  assert.equal(classifyAgentDeviceFailure(result(true)), 'infrastructure');
  assert.equal(classifyAgentDeviceFailure(result()), 'behavioral');
  assert.equal(classifyAgentDeviceFailure('not-json'), 'infrastructure');
});

test('agent-device execution accepts a CLI path containing spaces', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'agent-device-engine path-'));
  const cliPath = path.join(root, 'agent device.mjs');
  try {
    fs.writeFileSync(cliPath, '');
    assert.deepEqual(runAgentDeviceEngine([cliPath], []), {
      engine: 'agent-device',
      outcome: 'pass',
      exitCode: 0,
    });
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('the CLI argv keeps node flags and the entry script apart', () => {
  // The default is the source CLI the device workflows name explicitly.
  assert.deepEqual(resolveAgentDeviceCliArgv(undefined, undefined), [
    '--experimental-strip-types',
    'src/bin.ts',
  ]);
  // An entry path is never split, so spaces in it survive.
  assert.deepEqual(resolveAgentDeviceCliArgv('/tmp/agent device.mjs', ''), [
    '/tmp/agent device.mjs',
  ]);
  assert.deepEqual(resolveAgentDeviceCliArgv('/tmp/agent device.mjs', undefined), [
    '--experimental-strip-types',
    '/tmp/agent device.mjs',
  ]);
  // Flags are split, which is exact because a node flag cannot contain a space.
  assert.deepEqual(resolveAgentDeviceCliArgv('bin/x.mjs', '--no-warnings  --enable-source-maps'), [
    '--no-warnings',
    '--enable-source-maps',
    'bin/x.mjs',
  ]);
});
