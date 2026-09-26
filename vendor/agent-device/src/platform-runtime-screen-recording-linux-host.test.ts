import { beforeEach, expect, test, vi } from 'vitest';
import { createLinuxScreenRecordingHost } from './platform-runtime-screen-recording-linux-host.ts';
const state = vi.hoisted(() => ({
  platform: 'linux',
  env: { DISPLAY: ':99' } as Record<string, string>,
  missing: '',
}));
vi.mock('@agent-device/host-kit/process', async (original) => ({
  ...(await original<typeof import('@agent-device/host-kit/process')>()),
  hostPlatform: () => state.platform,
  hostEnvironment: () => state.env,
}));
vi.mock('@agent-device/host-kit/command', async (original) => ({
  ...(await original<typeof import('@agent-device/host-kit/command')>()),
  whichCmd: async (name: string) => name !== state.missing,
}));
beforeEach(() => {
  state.platform = 'linux';
  state.env = { DISPLAY: ':99' };
  state.missing = '';
});

test('admits the packaged X11 worker when display and tools exist', async () => {
  expect(await createLinuxScreenRecordingHost().availability()).toEqual({ available: true });
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
