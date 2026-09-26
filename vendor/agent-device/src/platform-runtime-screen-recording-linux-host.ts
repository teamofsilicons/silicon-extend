import fs from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';
import { hostEnvironment, hostPlatform } from '@agent-device/host-kit/process';
import { runCmdBackground, whichCmd } from '@agent-device/host-kit/command';
import { AppError } from '@agent-device/kernel/errors';
import type {
  HostCommandResult,
  ManagedProcessIdentity,
} from '@agent-device/contracts/platform-runtime-host';
import type {
  LinuxScreenRecordingHost,
  ScreenRecordingBackgroundProcess,
} from '@agent-device/contracts/screen-recording-runtime-host';
import {
  inspectManagedProcess,
  resolveManagedProcessIdentity,
  terminateManagedProcessSet,
} from './platform-runtime-screen-recording-process-host.ts';

export function createLinuxScreenRecordingHost(): LinuxScreenRecordingHost {
  return {
    availability: availability,
    start: startRecording,
    inspectProcess: async (marker) => inspectManagedProcess(marker),
    terminateProcesses: stopLinuxProcessSet,
  };
}

async function availability(): ReturnType<LinuxScreenRecordingHost['availability']> {
  const env = hostEnvironment();
  const unavailable = (hint: string) => ({ available: false as const, hint });
  if (hostPlatform() !== 'linux')
    return unavailable('Linux recording requires a local Linux host.');
  if (env.WAYLAND_DISPLAY || env.XDG_SESSION_TYPE === 'wayland')
    return unavailable(
      'Wayland recording requires the ScreenCast portal; portal capture is not implemented yet.',
    );
  if (!env.DISPLAY) return unavailable('Screen recording requires an X11 display.');
  for (const tool of ['python3', 'ffmpeg', 'ffprobe', 'xwininfo']) {
    if (!(await whichCmd(tool))) return unavailable(`Install ${tool} for X11 recording.`);
  }
  if (!(await workerPath())) return unavailable('The packaged Linux recording worker is missing.');
  return { available: true };
}

async function workerPath(): Promise<string | undefined> {
  let dir = path.dirname(fileURLToPath(import.meta.url));
  for (let depth = 0; depth < 6; depth++) {
    const candidate = path.join(dir, 'linux', 'screen-record.py');
    if (
      await fs.stat(candidate).then(
        (stat) => stat.isFile(),
        () => false,
      )
    )
      return candidate;
    dir = path.dirname(dir);
  }
  return undefined;
}

/**
 * Silicon Extend fork: where the worker publishes its state. It is derived from the native path,
 * which the durable recording descriptor keeps, so a stop or cleanup after a daemon restart can
 * remove it too (platform-linux's recording runtime derives the same path).
 */
function linuxRecordingStatusPath(nativePath: string): string {
  return `${nativePath}.status.json`;
}

/** The worker's last stderr line: its own account of why it stopped. */
function workerReason(stderr: string | undefined): string | undefined {
  return stderr?.trim().split('\n').at(-1)?.trim() || undefined;
}

async function startRecording(
  input: Parameters<LinuxScreenRecordingHost['start']>[0],
  signal?: AbortSignal,
): Promise<ScreenRecordingBackgroundProcess> {
  signal?.throwIfAborted();
  const support = await availability();
  if (!support.available) throw new AppError('UNSUPPORTED_OPERATION', support.hint);
  const worker = await workerPath();
  if (!worker) throw new AppError('TOOL_MISSING', 'Linux recording worker is missing');
  const statusPath = linuxRecordingStatusPath(input.outputPath);
  // Left by a recording at this path whose daemon died; that recording's native file is already
  // gone (the runtime prepares the path before starting), so its status is stale too.
  await fs.rm(statusPath, { force: true });
  // The worker refuses to start, and stops recording, once this daemon is no longer its parent.
  const args = [worker, '--out', input.outputPath, '--status', statusPath];
  args.push('--owner-pid', String(process.pid));
  if (input.fps !== undefined) args.push('--fps', String(input.fps));
  if (input.appId !== undefined) args.push('--app-id', input.appId);
  const background = runCmdBackground('python3', args, { allowFailure: true, captureOutput: true });
  let result: HostCommandResult | undefined;
  let failure: unknown;
  void background.wait.then(
    (value) => {
      result = value;
    },
    (error: unknown) => {
      failure = error;
    },
  );
  const markers: ManagedProcessIdentity[] = [];
  try {
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline) {
      signal?.throwIfAborted();
      if (failure) throw failure;
      if (result) {
        const reason = workerReason(result.stderr);
        throw new AppError(
          'COMMAND_FAILED',
          `Linux recorder exited before its first frame${reason ? `: ${reason}` : ''}`,
          { stderr: result.stderr },
        );
      }
      const status = await fs
        .readFile(statusPath, 'utf8')
        .then((text) => JSON.parse(text))
        .catch(() => undefined);
      if (
        status?.state === 'recording' &&
        status.pid === background.child.pid &&
        Number.isSafeInteger(status.encoderPid)
      ) {
        const root = await resolveManagedProcessIdentity(background.child.pid);
        const encoder = await resolveManagedProcessIdentity(status.encoderPid);
        if (root && encoder) {
          markers.push(root, encoder);
          break;
        }
      }
      if (status?.state === 'failed')
        throw new AppError(
          'COMMAND_FAILED',
          `Linux recording failed${typeof status.error === 'string' && status.error ? `: ${status.error}` : ''}`,
          { stderr: status.error },
        );
      await delay(50, undefined, { signal });
    }
    if (markers.length !== 2)
      throw new AppError(
        'COMMAND_FAILED',
        'Linux recording did not produce a frame within 15 seconds',
      );
    signal?.throwIfAborted();
  } catch (error) {
    background.child.kill('SIGTERM');
    const exited = await Promise.race([
      background.wait.then(
        () => true,
        () => true,
      ),
      delay(6000).then(() => false),
    ]);
    if (!exited) background.child.kill('SIGKILL');
    await background.wait.catch(() => undefined);
    await fs.rm(statusPath, { force: true });
    throw error;
  }
  let termination: Promise<void> | undefined;
  const terminate = () => {
    termination ??= stopLinuxProcessSet(markers, background)
      .then((outcome) => {
        if (outcome === 'ownership-lost')
          throw new AppError('COMMAND_FAILED', 'Linux recorder ownership changed before cleanup');
      })
      .catch((error: unknown) => {
        termination = undefined;
        throw error;
      });
    return termination;
  };
  return Object.freeze({
    markers,
    wait: background.wait.finally(async () => {
      await fs.rm(statusPath, { force: true });
    }),
    terminate,
  });
}

/** The supervisor finalizes its child. Signaling both would deliver SIGINT twice to ffmpeg. */
async function stopLinuxProcessSet(
  markers: readonly ManagedProcessIdentity[],
  background?: ReturnType<typeof runCmdBackground>,
): Promise<'terminated' | 'already-missing' | 'ownership-lost'> {
  const [supervisor, ...encoders] = markers;
  if (!supervisor) return 'already-missing';
  const stopped = await terminateManagedProcessSet([supervisor], background, undefined, {
    discoverDescendants: false,
  });
  if (stopped === 'ownership-lost') return stopped;
  // Only a supervisor that has finished can no longer signal this persisted encoder.
  const encodersStopped = await terminateManagedProcessSet(encoders, undefined, undefined, {
    discoverDescendants: false,
  });
  return encodersStopped === 'ownership-lost' ? encodersStopped : stopped;
}
