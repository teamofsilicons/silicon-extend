import { beforeEach, expect, test, vi } from 'vitest';
import type { HostCommandResult } from '@agent-device/contracts/platform-runtime-host';
import { startMacOsRecording } from './platform-runtime-screen-recording-macos-host.ts';

const seams = vi.hoisted(() => ({
  start: vi.fn(),
  read: vi.fn(),
  remove: vi.fn(),
  identity: vi.fn(),
  terminate: vi.fn(),
}));
vi.mock('@agent-device/platform-apple/macos', () => ({ startMacOsRecordingProcess: seams.start }));
vi.mock('node:fs/promises', () => ({ default: { readFile: seams.read, rm: seams.remove } }));
vi.mock('./platform-runtime-screen-recording-process-host.ts', () => ({
  resolveManagedProcessIdentity: seams.identity,
  terminateManagedProcessSet: seams.terminate,
}));

const marker = { pid: 42, startTime: 'known-start', command: 'helper record --out /tmp/movie.mp4' };
beforeEach(() => {
  vi.resetAllMocks();
  seams.identity.mockResolvedValue(marker);
  seams.read.mockResolvedValue(JSON.stringify({ state: 'recording', pid: 42 }));
  seams.remove.mockResolvedValue(undefined);
  seams.terminate.mockResolvedValue('terminated');
});

function processFixture() {
  let settle!: (result: HostCommandResult) => void;
  const wait = new Promise<HostCommandResult>((resolve) => {
    settle = resolve;
  });
  const child = {
    pid: 42,
    kill: vi.fn(() => {
      settle({ exitCode: 0, stdout: '', stderr: '' });
      return true;
    }),
  };
  const process = { child, wait };
  seams.start.mockResolvedValue(process);
  return { process, settle };
}

test('publishes first-frame readiness with exact process identity and terminates only that owner', async () => {
  const { process, settle } = processFixture();
  const handle = await startMacOsRecording({
    outputPath: '/tmp/movie.mp4',
    fps: 24,
    bundleId: 'com.example.app',
  });
  expect(handle.markers).toEqual([marker]);
  expect(seams.start).toHaveBeenCalledWith(
    expect.objectContaining({ outputPath: '/tmp/movie.mp4', fps: 24, bundleId: 'com.example.app' }),
  );
  await handle.terminate();
  expect(seams.terminate).toHaveBeenCalledWith([marker], process);
  settle({ exitCode: 0, stdout: 'completed', stderr: '' });
  await expect(handle.wait).resolves.toMatchObject({ stdout: 'completed' });
  expect(seams.remove).toHaveBeenCalledTimes(1);
});

test('cancellation during acquisition terminates the unpublished recorder and preserves the reason', async () => {
  const { process } = processFixture();
  const controller = new AbortController();
  const reason = new Error('cancel capture startup');
  seams.identity.mockImplementation(async () => {
    controller.abort(reason);
    return marker;
  });
  await expect(
    startMacOsRecording({ outputPath: '/tmp/movie.mp4' }, controller.signal),
  ).rejects.toBe(reason);
  expect(process.child.kill).toHaveBeenCalledWith('SIGTERM');
  expect(seams.remove).toHaveBeenCalledTimes(1);
});

test('an exited recorder cannot publish a stale readiness file', async () => {
  const { settle } = processFixture();
  settle({ exitCode: 1, stdout: 'permission denied', stderr: '' });
  await expect(startMacOsRecording({ outputPath: '/tmp/movie.mp4' })).rejects.toMatchObject({
    code: 'COMMAND_FAILED',
  });
});

test('failed ownership verification remains retryable and never uses a foreign pid', async () => {
  const { settle } = processFixture();
  const handle = await startMacOsRecording({ outputPath: '/tmp/movie.mp4' });
  seams.terminate.mockResolvedValueOnce('ownership-lost').mockResolvedValueOnce('terminated');
  await expect(handle.terminate()).rejects.toThrow('ownership changed');
  await expect(handle.terminate()).resolves.toBeUndefined();
  expect(seams.terminate).toHaveBeenCalledTimes(2);
  settle({ exitCode: 0, stdout: '', stderr: '' });
  await handle.wait;
});
