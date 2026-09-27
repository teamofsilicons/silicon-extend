import fs from 'node:fs/promises';
import path from 'node:path';
import { beforeEach, expect, test, vi } from 'vitest';
import { createLinuxScreenRecordingHost } from './platform-runtime-screen-recording-linux-host.ts';
import { mkdtempForTest } from './__tests__/test-utils/tmp-dir.ts';
type Spawned = { args: string[]; staleStatusAtSpawn: boolean };
const state = vi.hoisted(() => ({
  platform: 'linux',
  env: { DISPLAY: ':99' } as Record<string, string>,
  missing: '',
  spawned: [] as Spawned[],
  // What the fake worker does once spawned: publish a status, or exit with this stderr.
  worker: {
    status: undefined as Record<string, unknown> | undefined,
    exitStderr: '',
  },
}));
vi.mock('@agent-device/host-kit/process', async (original) => ({
  ...(await original<typeof import('@agent-device/host-kit/process')>()),
  hostPlatform: () => state.platform,
  hostEnvironment: () => state.env,
}));
vi.mock('@agent-device/host-kit/command', async (original) => {
  const files = await import('node:fs');
  return {
    ...(await original<typeof import('@agent-device/host-kit/command')>()),
    whichCmd: async (name: string) => name !== state.missing,
    runCmdBackground: (_cmd: string, args: string[]) => {
      const status = args[args.indexOf('--status') + 1]!;
      const staleStatusAtSpawn = files.existsSync(status);
      state.spawned.push({ args, staleStatusAtSpawn });
      let settle: (value: { stdout: string; stderr: string; exitCode: number }) => void = () => {};
      const wait = new Promise<{
        stdout: string;
        stderr: string;
        exitCode: number;
      }>((resolve) => {
        settle = resolve;
      });
      const child = {
        pid: 4242,
        kill: () => {
          settle({ stdout: '', stderr: '', exitCode: 0 });
          return true;
        },
      };
      queueMicrotask(async () => {
        if (state.worker.status)
          await files.promises.writeFile(
            status,
            JSON.stringify({ pid: child.pid, ...state.worker.status }),
          );
        else settle({ stdout: '', stderr: state.worker.exitStderr, exitCode: 1 });
      });
      return { child, wait };
    },
  };
});
vi.mock('./platform-runtime-screen-recording-process-host.ts', async (original) => ({
  ...(await original<typeof import('./platform-runtime-screen-recording-process-host.ts')>()),
  resolveManagedProcessIdentity: async (pid: number) => ({
    pid,
    startTime: `start-${pid}`,
    command: 'python3 screen-record.py',
  }),
}));
let dir = '';
beforeEach(async () => {
  state.platform = 'linux';
  state.env = { DISPLAY: ':99' };
  state.missing = '';
  state.spawned = [];
  state.worker = { status: undefined, exitStderr: '' };
  dir = await mkdtempForTest('linux-recording-host-');
});

test('admits the packaged X11 worker when display and tools exist', async () => {
  expect(await createLinuxScreenRecordingHost().availability()).toEqual({
    available: true,
  });
});
test.each(['WAYLAND_DISPLAY', 'XDG_SESSION_TYPE'])(
  'refuses XWayland when %s identifies a Wayland session',
  async (key) => {
    state.env[key] = key === 'XDG_SESSION_TYPE' ? 'wayland' : 'wayland-0';
    expect(await createLinuxScreenRecordingHost().availability()).toMatchObject({
      available: false,
      hint: expect.stringContaining('portal'),
    });
  },
);
test('reports the actual missing encoder dependency', async () => {
  state.missing = 'ffprobe';
  expect(await createLinuxScreenRecordingHost().availability()).toMatchObject({
    available: false,
    hint: expect.stringContaining('ffprobe'),
  });
});
test('headless Linux has no screen recording capability', async () => {
  state.env = {};
  expect(await createLinuxScreenRecordingHost().availability()).toMatchObject({
    available: false,
    hint: expect.stringContaining('display'),
  });
});

test('binds the worker to this daemon and keeps its status beside the native recording', async () => {
  const outputPath = path.join(dir, 'video.native.mp4');
  const statusPath = `${outputPath}.status.json`;
  // A daemon that died mid-recording left this behind; the new recording must not trip over it.
  await fs.writeFile(statusPath, JSON.stringify({ state: 'completed', reason: 'owner-exited' }));
  state.worker.status = { state: 'recording', encoderPid: 4243 };
  const recording = await createLinuxScreenRecordingHost().start({
    outputPath,
    fps: 12,
  });
  const [spawned] = state.spawned;
  expect(spawned?.args.slice(1)).toEqual([
    '--out',
    outputPath,
    '--status',
    statusPath,
    '--owner-pid',
    String(process.pid),
    '--fps',
    '12',
  ]);
  expect(spawned?.staleStatusAtSpawn).toBe(false);
  expect(recording.markers?.map((marker) => marker.pid)).toEqual([4242, 4243]);
  await recording.terminate().catch(() => {});
});

// Silicon Extend fork: Linux exports the worker's own encode, so --quality reaches the worker.
test('hands the requested quality to the worker, and nothing when none was asked for', async () => {
  const outputPath = path.join(dir, 'video.native.mp4');
  state.worker.status = { state: 'recording', encoderPid: 4243 };
  const high = await createLinuxScreenRecordingHost().start({ outputPath, quality: 'high' });
  expect(state.spawned[0]?.args.slice(-2)).toEqual(['--quality', 'high']);
  await high.terminate().catch(() => {});
  const plain = await createLinuxScreenRecordingHost().start({ outputPath });
  expect(state.spawned[1]?.args).not.toContain('--quality');
  await plain.terminate().catch(() => {});
});

test("a refused recording says why, in the worker's own words", async () => {
  const outputPath = path.join(dir, 'video.native.mp4');
  const error = 'the app window did not redraw within 5 seconds of recording start';
  state.worker.status = { state: 'failed', error };
  await expect(createLinuxScreenRecordingHost().start({ outputPath })).rejects.toMatchObject({
    code: 'COMMAND_FAILED',
    message: `Linux recording failed: ${error}`,
  });
  await expect(fs.stat(`${outputPath}.status.json`)).rejects.toMatchObject({
    code: 'ENOENT',
  });
});

test('a worker that exits before its first frame reports its last stderr line', async () => {
  state.worker.exitStderr =
    "Traceback...\nthe recorder's owner (process 1) exited before recording started; nothing was recorded\n";
  await expect(
    createLinuxScreenRecordingHost().start({
      outputPath: path.join(dir, 'video.native.mp4'),
    }),
  ).rejects.toMatchObject({
    message:
      "Linux recorder exited before its first frame: the recorder's owner (process 1) exited before recording started; nothing was recorded",
  });
});
