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

test('a first frame observed just before the deadline is a successful start, not a timeout', async () => {
  const { process } = processFixture();
  let now = 0;
  const clock = vi.spyOn(Date, 'now').mockImplementation(() => now);
  try {
    // The last poll starts inside the 15 s window, and its identity lookup finishes after it.
    seams.identity.mockImplementation(async () => {
      now = 15_200;
      return marker;
    });
    const handle = await startMacOsRecording({ outputPath: '/tmp/movie.mp4' });
    expect(handle.markers).toEqual([marker]);
    expect(process.child.kill).not.toHaveBeenCalled();
  } finally {
    clock.mockRestore();
  }
});

test('a recorder that never reports a frame is stopped with a reason and a way forward', async () => {
  const { process } = processFixture();
  let now = 0;
  const clock = vi.spyOn(Date, 'now').mockImplementation(() => now);
  try {
    seams.read.mockImplementation(async () => {
      now += 5_000;
      return JSON.stringify({ state: 'starting', pid: 42 });
    });
    await expect(startMacOsRecording({ outputPath: '/tmp/movie.mp4' })).rejects.toMatchObject({
      code: 'COMMAND_FAILED',
      details: {
        reason: 'recording_first_frame_timeout',
        hint: expect.stringContaining('Screen Recording'),
      },
    });
    expect(process.child.kill).toHaveBeenCalledWith('SIGTERM');
  } finally {
    clock.mockRestore();
  }
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
    message: expect.stringContaining('exited with code 1 before it recorded its first frame'),
    details: {
      reason: 'recording_start_failed',
      exitCode: 1,
      stdout: 'permission denied',
      hint: expect.stringContaining('Screen Recording'),
    },
  });
});

function helperFailureLine(message: string, details?: Record<string, string>): string {
  return `${JSON.stringify({ ok: false, error: { message, ...(details ? { details } : {}) } })}\n`;
}

test('a recorder that refuses to start passes on its own reason and a way forward', async () => {
  const { settle } = processFixture();
  const refusal =
    'the requested app has no window on screen to record. Show one of its windows on the current Space (unhide the app or unminimize the window), then start the recording again.';
  settle({
    exitCode: 1,
    stdout: helperFailureLine(refusal, {
      reason: 'app_window_not_on_screen',
      bundleId: 'com.example.app',
      pid: '77',
    }),
    stderr: '',
  });
  const failure = startMacOsRecording({
    outputPath: '/tmp/movie.mp4',
    bundleId: 'com.example.app',
  });
  await expect(failure).rejects.toMatchObject({
    code: 'COMMAND_FAILED',
    message: `Native macOS recording could not start: ${refusal}`,
    details: {
      reason: 'app_window_not_on_screen',
      bundleId: 'com.example.app',
      exitCode: 1,
      hint: expect.stringContaining('unhide the app or unminimize the window'),
    },
  });
  await expect(failure).rejects.not.toMatchObject({
    message: expect.stringContaining('exited before startup completed'),
  });
});

test('a Screen Recording refusal tells the caller which permission to allow', async () => {
  const { settle } = processFixture();
  settle({
    exitCode: 1,
    stdout: helperFailureLine(
      'macOS refused the screen capture because Screen Recording is not allowed for Silicon Extend',
      { reason: 'screen_recording_permission_denied', permission: 'screen-recording' },
    ),
    stderr: '',
  });
  await expect(startMacOsRecording({ outputPath: '/tmp/movie.mp4' })).rejects.toMatchObject({
    message: expect.stringContaining('Screen Recording is not allowed'),
    details: {
      reason: 'screen_recording_permission_denied',
      hint: expect.stringContaining('Screen & System Audio Recording'),
    },
  });
});

test('a recorder failure without a reason keeps its message and gets the general way forward', async () => {
  const { settle } = processFixture();
  settle({
    exitCode: 1,
    stdout: `progress\n${helperFailureLine('recording output already exists')}`,
    stderr: '',
  });
  await expect(startMacOsRecording({ outputPath: '/tmp/movie.mp4' })).rejects.toMatchObject({
    message: 'Native macOS recording could not start: recording output already exists',
    details: {
      reason: 'recording_start_failed',
      hint: expect.stringContaining('run record start'),
    },
  });
});

test('a recorder that refuses during the last wait before the deadline reports its refusal, not a timeout', async () => {
  const { settle } = processFixture();
  let now = 0;
  const clock = vi.spyOn(Date, 'now').mockImplementation(() => now);
  try {
    seams.read.mockImplementation(async () => {
      now = 15_000;
      // The recorder exits after this read, while the loop waits for its next poll.
      setTimeout(() =>
        settle({
          exitCode: 1,
          stdout: helperFailureLine(
            'macOS delivered no screen frames within 10 seconds of starting the capture',
            {
              reason: 'no_screen_frames',
            },
          ),
          stderr: '',
        }),
      );
      return JSON.stringify({ state: 'starting', pid: 42 });
    });
    await expect(startMacOsRecording({ outputPath: '/tmp/movie.mp4' })).rejects.toMatchObject({
      details: { reason: 'no_screen_frames' },
    });
  } finally {
    clock.mockRestore();
  }
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
