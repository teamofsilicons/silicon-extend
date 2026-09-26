import fs from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { setTimeout as delay } from 'node:timers/promises';
import { AppError } from '@agent-device/kernel/errors';
import { startMacOsRecordingProcess } from '@agent-device/platform-apple/macos';
import type { ScreenRecordingBackgroundProcess } from '@agent-device/contracts/screen-recording-runtime-host';
import type { HostCommandResult } from '@agent-device/contracts/platform-runtime-host';
import {
  resolveManagedProcessIdentity,
  terminateManagedProcessSet,
} from './platform-runtime-screen-recording-process-host.ts';

export async function startMacOsRecording(
  input: Readonly<{ outputPath: string; bundleId?: string; fps?: number }>,
  signal?: AbortSignal,
): Promise<ScreenRecordingBackgroundProcess> {
  signal?.throwIfAborted();
  const statusPath = `${input.outputPath}.${randomUUID()}.status.json`;
  const background = await startMacOsRecordingProcess({ ...input, statusPath });
  let result: HostCommandResult | undefined;
  let processError: unknown;
  void background.wait.then(
    (value) => {
      result = value;
    },
    (error: unknown) => {
      processError = error;
    },
  );
  const assertRunning = () => {
    if (processError) throw processError;
    if (result)
      throw new AppError(
        'COMMAND_FAILED',
        'Native macOS recording exited before startup completed',
        {
          stdout: result.stdout,
          stderr: result.stderr,
        },
      );
  };
  let marker: Awaited<ReturnType<typeof resolveManagedProcessIdentity>>;
  try {
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline) {
      signal?.throwIfAborted();
      assertRunning();
      marker ??= await resolveManagedProcessIdentity(background.child.pid);
      const status = await fs
        .readFile(statusPath, 'utf8')
        .then((text) => JSON.parse(text))
        .catch(() => undefined);
      assertRunning();
      if (marker && status?.state === 'recording' && status.pid === background.child.pid) break;
      await delay(50, undefined, { signal });
    }
    if (!marker || Date.now() >= deadline)
      throw new Error('Native macOS recording did not produce its first frame within 15 seconds');
    signal?.throwIfAborted();
  } catch (error) {
    background.child.kill('SIGTERM');
    const exited = await Promise.race([
      background.wait.then(
        () => true,
        () => true,
      ),
      delay(5000).then(() => false),
    ]);
    if (!exited) background.child.kill('SIGKILL');
    await background.wait.catch(() => undefined);
    await fs.rm(statusPath, { force: true });
    throw error;
  }
  const markers = [marker];
  let termination: Promise<void> | undefined;
  const terminate = () => {
    termination ??= terminateManagedProcessSet(markers, background)
      .then((outcome) => {
        if (outcome === 'ownership-lost')
          throw new Error('Native macOS recorder ownership changed before cleanup');
      })
      .catch((error: unknown) => {
        termination = undefined;
        throw error;
      });
    return termination;
  };
  const wait = background.wait.finally(async () => {
    await fs.rm(statusPath, { force: true });
  });
  return Object.freeze({ markers, wait, terminate });
}
