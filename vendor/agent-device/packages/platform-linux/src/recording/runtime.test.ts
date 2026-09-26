import { expect, test, vi } from 'vitest';
import { localRuntimeOwner } from '@agent-device/contracts/platform-runtime';
import type { ScreenRecordingRuntimeHost } from '@agent-device/contracts/screen-recording-runtime-host';
import type { ScreenRecordingStartInput } from '@agent-device/contracts/screen-recording-runtime';
import type { DeviceInfo } from '@agent-device/kernel/device';
import { createLinuxRecordingOperations } from './runtime.ts';

const device: DeviceInfo = {
  platform: 'linux',
  id: 'local',
  name: 'Linux',
  kind: 'device',
  target: 'desktop',
  booted: true,
};
const input: ScreenRecordingStartInput = {
  sessionId: 'test',
  outputPath: '/session/video.mp4',
  scope: 'device',
  showTouches: false,
  hideTouchesRequested: true,
  recordOnlySession: true,
  fps: 12,
  fence: { token: 'test', generation: 1 },
};
const markers = [
  { pid: 100, command: 'python3 screen-record.py', startTime: 'first' },
  { pid: 101, command: 'ffmpeg -i :99', startTime: 'second' },
];
function setup() {
  const files = new Map<string, string>();
  const terminate = vi.fn(async () => {});
  const host = {
    linux: {
      availability: async () => ({ available: true as const }),
      start: vi.fn(async ({ outputPath }: { outputPath: string }) => {
        files.set(outputPath, 'native frames');
        return {
          markers,
          terminate,
          wait: Promise.resolve({ stdout: '', stderr: '', exitCode: 0 }),
        };
      }),
      inspectProcess: vi.fn(async () => 'missing' as const),
      terminateProcesses: vi.fn(
        async () => 'already-missing' as 'already-missing' | 'ownership-lost',
      ),
    },
    outputs: {
      prepare: vi.fn(async (path: string) => {
        files.delete(path);
      }),
      copy: vi.fn(async ({ from, to }: { from: string; to: string }) => {
        if (!files.has(from)) throw new Error('source missing');
        files.set(to, files.get(from)!);
      }),
      remove: vi.fn(async (path: string) => {
        files.delete(path);
        return 'removed' as const;
      }),
    },
    finalize: {
      sniff: vi.fn(async ({ outputPath }: { outputPath: string }) => {
        if (!files.has(outputPath)) throw new Error('missing video');
      }),
      complete: vi.fn(async ({ outputPath }: { outputPath: string }) => {
        files.set(outputPath, 'exported video');
        return {};
      }),
    },
    ownedProcesses: { replace: vi.fn(), clear: vi.fn() },
  } satisfies Pick<ScreenRecordingRuntimeHost, 'linux' | 'outputs' | 'finalize' | 'ownedProcesses'>;
  const controller = new AbortController();
  const operations = createLinuxRecordingOperations({
    host,
    device,
    owner: localRuntimeOwner('linux'),
    signal: controller.signal,
  });
  return { files, host, controller, operations, terminate };
}

test('starts a recorder, publishes both identities and exports a separate playable copy', async () => {
  const { operations, files, host, terminate } = setup();
  const started = await operations.screenRecordingStart(input);
  const handle = started.pendingHandle.transfer();
  expect(host.ownedProcesses.replace).toHaveBeenCalledWith(
    { kind: 'session', sessionId: 'test' },
    markers.map((marker) => ({ ...marker, purpose: 'linux-screen-recording' })),
  );
  const done = await handle.finish();
  expect(done.status).toBe('completed');
  expect(terminate).toHaveBeenCalledOnce();
  expect(files.get(input.outputPath)).toBe('exported video');
  expect(files.has('/session/video.native.mp4')).toBe(false);
  expect(host.finalize.sniff).toHaveBeenCalled();
  await handle[Symbol.asyncDispose]();
  expect(terminate).toHaveBeenCalledOnce();
});

test('export failure retains the native recording and permits retry without re-recording', async () => {
  const { operations, files, host } = setup();
  const started = await operations.screenRecordingStart(input);
  const handle = started.pendingHandle.transfer();
  host.finalize.complete.mockRejectedValueOnce(new Error('encoder unavailable'));
  await expect(handle.finish()).rejects.toThrow('encoder unavailable');
  expect(files.get('/session/video.native.mp4')).toBe('native frames');
  expect(files.has(input.outputPath)).toBe(false);
  expect((await handle.finish()).status).toBe('completed');
  expect(host.linux.start).toHaveBeenCalledOnce();
});

test('restart can export an already stopped recorder using persisted coordinates', async () => {
  const { operations, files, host } = setup();
  const started = await operations.screenRecordingStart(input);
  const restored = await operations.screenRecordingReattach({ envelope: started.envelope });
  expect(restored.status).toBe('active');
  if (restored.status !== 'active') throw new Error('expected recovered handle');
  expect((await restored.handle.finish()).status).toBe('completed');
  expect(files.get(input.outputPath)).toBe('exported video');
  expect(host.linux.terminateProcesses).toHaveBeenCalledWith(markers);
});

test('identity loss refuses cleanup and export, preserving the native artifact', async () => {
  const { operations, files, host } = setup();
  const started = await operations.screenRecordingStart(input);
  host.linux.terminateProcesses.mockResolvedValue('ownership-lost');
  expect(await operations.screenRecordingCleanup({ envelope: started.envelope })).toMatchObject({
    status: 'cleanup-pending',
    reason: 'ownership-fence-lost',
  });
  expect(files.has('/session/video.native.mp4')).toBe(true);
  expect(host.ownedProcesses.clear).not.toHaveBeenCalled();
});

test.each(['device', 'version', 'native-path', 'process'])(
  'malformed %s coordinates cannot signal or delete anything',
  async (mutation) => {
    const { operations, host, files } = setup();
    const started = await operations.screenRecordingStart(input);
    const envelope = structuredClone(started.envelope);
    const invalid =
      mutation === 'device'
        ? { ...envelope, device: { ...envelope.device, id: 'foreign' } }
        : mutation === 'version'
          ? { ...envelope, descriptor: { ...envelope.descriptor, version: 999 } }
          : {
              ...envelope,
              descriptor: {
                ...envelope.descriptor,
                body: {
                  ...envelope.descriptor.body,
                  ...(mutation === 'native-path'
                    ? { nativePath: '/unrelated/file' }
                    : { processes: [{ pid: -1 }] }),
                },
              },
            };
    expect(await operations.screenRecordingReattach({ envelope: invalid })).toMatchObject({
      status: 'unreattachable',
      reason: 'descriptor-invalid',
    });
    expect(await operations.screenRecordingCleanup({ envelope: invalid })).toMatchObject({
      status: 'cleanup-pending',
    });
    expect(host.linux.terminateProcesses).not.toHaveBeenCalled();
    expect(files.has('/session/video.native.mp4')).toBe(true);
  },
);

test('rejects unisolated app scope before preparing output or spawning a recorder', async () => {
  const { operations, host } = setup();
  await expect(operations.screenRecordingStart({ ...input, scope: 'app' })).rejects.toMatchObject({
    code: 'UNSUPPORTED_OPERATION',
  });
  expect(host.outputs.prepare).not.toHaveBeenCalled();
  expect(host.linux.start).not.toHaveBeenCalled();
});

test('cancelled startup terminates the just-created recorder', async () => {
  const { operations, host, controller, terminate } = setup();
  const start = host.linux.start.getMockImplementation()!;
  host.linux.start.mockImplementation(async (input) => {
    const process = await start(input);
    controller.abort();
    return process;
  });
  await expect(operations.screenRecordingStart(input)).rejects.toThrow();
  expect(terminate).toHaveBeenCalledOnce();
});
